// SPDX-License-Identifier: GPL-3.0-or-later
//! The config task (ADR-0005 7.6): the only place that reads or writes `gorca.toml`.
//!
//! ```text
//! inotify(dir) -> debounce 100 ms -> read (spawn_blocking, 1 s deadline) -> hash
//!   -> equal to the last load or to our own last save? stop
//!   -> gorca_config::load -> compile (names to commands, keys to physical keys)
//!   -> store the keymap and lane settings (ArcSwap) -> ControlMsg::Settings(Loaded) to the Reader
//! ```
//!
//! A failed read, a syntax error or a panic while loading keeps the **last good** settings
//! (HR-2). Nothing here runs on a hot lane: the lanes only `ArcSwap::load`.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures_lite::StreamExt;
use gorca_config::profiles::naming::is_valid_file_id;
use gorca_config::profiles::{
    ExtSchema, FileId, ProfileLoader, SavePlan, SaveRefusal, WriteStep, build_layers,
};
use gorca_config::{Diag, DiagMsg, Presets, SettingsPatch, diags_hash, fnv1a};
use gorca_core::{
    Compiled, ConfigRequest, ControlMsg, LoadCause, LoadedSettings, ModeKeys, PlatformFacts,
    ProfileSaveOutcome, ProfilesSaved, SaveKind, SaveProfiles, SaveResult, SettingsMsg, compile,
};
use gorca_input_api::{LaneLinks, StaticResolver};
use gorca_output_api::VoiceSetup;
use gorca_rt::{CancellationToken, mpsc};
use gorca_speech::{SpeechControl, SpeechHandle};
use inotify::{Inotify, WatchMask};

use crate::platform::{PlatformOverrides, probe};

/// Reads of the settings file give up after this (home may be on NFS or FUSE).
pub const READ_DEADLINE: Duration = Duration::from_secs(1);
/// The file must stay quiet this long before it is reloaded.
pub const DEBOUNCE: Duration = Duration::from_millis(100);
const WATCH_RETRY: Duration = Duration::from_secs(3);
/// How often the input sources are read again (K-8b).
const LAYOUT_POLL: Duration = Duration::from_secs(10);

/// Where the settings come from.
#[derive(Clone, Debug, Default)]
pub struct ConfigSource {
    /// `None`: built-in defaults, nothing is read, watched or saved.
    pub path: Option<PathBuf>,
    pub overrides: PlatformOverrides,
    /// Do not run `gsettings` (tests and isolated runs).
    pub no_probe: bool,
}

/// The result of one load, ready to publish.
#[derive(Debug)]
pub struct Built {
    pub loaded: Arc<LoadedSettings>,
    /// The keys the global file sets, the user base layer of the profile stack.
    pub patch: SettingsPatch,
    pub compiled: Compiled,
    /// The file could be parsed (or was missing).
    pub file_ok: bool,
}

fn render(d: &Diag, text: Option<&str>) -> Arc<str> {
    match text.and_then(|t| d.line_col(t)) {
        Some((line, _)) => Arc::from(format!("line {line}, {d}")),
        None => Arc::from(d.to_string()),
    }
}

/// Load, validate and compile `text`. Never fails and never panics: a panic inside is caught and
/// reported as a problem with the defaults in force.
pub fn build(text: Option<&str>, facts: &PlatformFacts, cause: LoadCause) -> Built {
    let attempt = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let l = gorca_config::load(text, &Presets::builtin());
        let c = compile(&l, facts, &StaticResolver);
        (l, c)
    }));
    let (l, c, panicked) = if let Ok((l, c)) = attempt {
        (l, c, false)
    } else {
        let l = gorca_config::load(None, &Presets::builtin());
        let c = compile(&l, facts, &StaticResolver);
        (l, c, true)
    };
    let mut diags: Vec<Diag> = l.diags.clone();
    diags.extend(c.diags.iter().cloned());
    if panicked {
        diags.push(Diag::error(
            "",
            None,
            DiagMsg::Other("the settings loader failed, using defaults".to_owned()),
        ));
    }
    let (mods, _) = gorca_core::gorca_modifiers(&l.settings, facts);
    // Chords without the gorca modifier (the desktop layout's keypad review keys) are grabbed
    // explicitly, and only once their command is implemented (K-1, K-8a).
    // Every key is grabbed under the keysyms of all configured layouts (K-8b).
    let keysyms = &facts.keysyms;
    let (commands, exact) =
        gorca_core::chord_grabs(&c.keymap, gorca_input_api::ModeKind::Focus, keysyms);
    // Browse mode (M7): the browse keymap's chords, and Escape for auto focus mode (modes.md 2).
    let (quick_nav, _) = gorca_core::browse_grabs(&c.keymap, keysyms);
    let web_escape = gorca_core::escape_grabs(&c.keymap, keysyms);
    let letters = gorca_core::letter_grabs(&c.keymap, keysyms);
    let (tab_next, tab_prev) = gorca_core::tab_grabs(&c.keymap, keysyms);
    let dropped =
        gorca_core::grab_collisions(&c.keymap, gorca_input_api::ModeKind::Browse, keysyms);
    if !dropped.is_empty() {
        tracing::info!(
            count = dropped.len(),
            "layout keysyms left out of the grabs (also typed by an unbound key of another layout)"
        );
    }
    let loaded = LoadedSettings {
        cause,
        settings: l.settings.clone(),
        hash: l.hash,
        problems: diags.iter().map(|d| render(d, text)).collect(),
        problems_hash: diags_hash(&diags),
        newer_schema: l.newer_schema,
        mode_keys: ModeKeys {
            modifiers: mods,
            commands,
            exact,
            quick_nav,
            web_escape,
            letters,
            tab_next,
            tab_prev,
            ..ModeKeys::default()
        },
        caps_lock_dropped: c.caps_lock_dropped,
        clock_24h: facts.clock_24h,
        profiles: None,
    };
    Built {
        loaded: Arc::new(loaded),
        patch: l.patch.clone(),
        compiled: c,
        file_ok: l.file_ok && !panicked,
    }
}

/// An empty `speech.language` becomes the language of the environment, so the Reader picks
/// the symbol table (names of punctuation) of the language the voice speaks.
fn fill_language(b: &mut Built, env_language: &str) {
    if b.loaded.settings.speech.language.trim().is_empty() && !env_language.is_empty() {
        env_language.clone_into(&mut Arc::make_mut(&mut b.loaded).settings.speech.language);
    }
}

#[derive(Debug, thiserror::Error)]
enum ReadError {
    #[error("the settings file could not be read: {0}")]
    Io(std::io::Error),
    #[error("reading the settings file took longer than {READ_DEADLINE:?}")]
    Deadline,
    #[error("the read task failed")]
    Task,
}

/// `Ok(None)` for a missing file. At most `MAX_FILE_BYTES + 1` bytes are read.
async fn read_file(path: &Path) -> Result<Option<String>, ReadError> {
    let p = path.to_owned();
    let job = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        match std::fs::File::open(&p) {
            Ok(f) => {
                let mut buf = Vec::new();
                f.take(gorca_config::MAX_FILE_BYTES as u64 + 1)
                    .read_to_end(&mut buf)?;
                Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    });
    match tokio::time::timeout(READ_DEADLINE, job).await {
        Ok(Ok(r)) => r.map_err(ReadError::Io),
        Ok(Err(_)) => Err(ReadError::Task),
        Err(_) => Err(ReadError::Deadline),
    }
}

/// Atomic write: a temporary file next to the target, then `rename`.
async fn write_atomic(path: &Path, text: String) -> std::io::Result<()> {
    let p = path.to_owned();
    tokio::task::spawn_blocking(move || {
        use std::io::Write;
        // A symlinked settings file (dotfile managers) is written through: the link stays a
        // link and its target gets the new text (F6).
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        if let Some(dir) = p.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut tmp = p.clone().into_os_string();
        tmp.push(".gorca-tmp");
        let tmp = PathBuf::from(tmp);
        {
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(text.as_bytes())?;
            if let Ok(m) = std::fs::metadata(&p) {
                // Keep the file's mode (a private settings file stays private).
                f.set_permissions(m.permissions())?;
            }
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &p)?;
        if let Some(dir) = p.parent()
            && let Ok(d) = std::fs::File::open(dir)
        {
            let _ = d.sync_all();
        }
        Ok(())
    })
    .await
    .map_err(|e| std::io::Error::other(e.to_string()))?
}

/// At most this many directory entries are looked at (the loader keeps 256 profiles, PC-3).
const MAX_PROFILE_ENTRIES: usize = 512;
/// Writes of profile files give up after this.
const WRITE_DEADLINE: Duration = Duration::from_secs(2);

/// The `(stem, text)` of every `*.toml` file of a profiles directory; a missing directory is
/// empty. Entries that are not regular files, or are too large, are skipped by the parser's
/// own size check (read at most one byte over the limit).
fn read_profile_dir(dir: &Path) -> std::io::Result<Vec<(String, String)>> {
    use std::io::Read;
    let rd = match std::fs::read_dir(dir) {
        Ok(r) => r,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut out = Vec::new();
    for ent in rd.flatten().take(MAX_PROFILE_ENTRIES) {
        let name = ent.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(stem) = name.strip_suffix(".toml") else {
            continue;
        };
        // Regular files only, looked at without following links: a FIFO or a device would
        // block the open, a link may point anywhere (R-4).
        if !ent.file_type().is_ok_and(|t| t.is_file()) {
            continue;
        }
        if stem.is_empty() || stem.starts_with('.') {
            continue;
        }
        let Ok(f) = std::fs::File::open(ent.path()) else {
            continue;
        };
        if !f.metadata().is_ok_and(|m| m.is_file()) {
            continue;
        }
        let mut buf = Vec::new();
        if f.take(gorca_config::MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut buf)
            .is_err()
        {
            continue;
        }
        out.push((stem.to_owned(), String::from_utf8_lossy(&buf).into_owned()));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Content hash of a profiles directory: what changed on disk, not who changed it.
fn profiles_hash(files: &[(String, String)]) -> u64 {
    let mut buf: Vec<u8> = Vec::new();
    for (stem, text) in files {
        buf.extend_from_slice(stem.as_bytes());
        buf.push(0);
        buf.extend_from_slice(&fnv1a(text.as_bytes()).to_le_bytes());
    }
    fnv1a(&buf)
}

/// Create `path` and write `text`, failing if anything exists there (`O_CREAT | O_EXCL`; a
/// symlink in the way is never followed, PE-6).
fn create_exclusive(path: &Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    if let Some(dir) = path.parent()
        && let Ok(d) = std::fs::File::open(dir)
    {
        let _ = d.sync_all();
    }
    Ok(())
}

/// Why a profile write did not happen.
#[derive(Debug, thiserror::Error)]
enum StepError {
    #[error("the file changed on disk")]
    Changed,
    #[error("{0}")]
    Io(std::io::Error),
    #[error("the write took longer than {WRITE_DEADLINE:?}")]
    Deadline,
}

#[derive(Debug)]
pub struct ConfigTask {
    pub source: ConfigSource,
    pub links: Arc<LaneLinks>,
    pub control: mpsc::Sender<ControlMsg>,
    pub requests: mpsc::Receiver<ConfigRequest>,
    /// The speech lane, which gets the language, output module and voice on every load.
    pub speech: Option<SpeechHandle>,
    /// The language the locale gives, used while `[speech] language` is empty.
    pub env_language: String,
}

/// What the speech server is told: the configured language, else the locale's, else `en`.
pub fn voice_setup(s: &gorca_config::SpeechSettings, env_language: &str) -> VoiceSetup {
    let language = if !s.language.trim().is_empty() {
        s.language.trim()
    } else if !env_language.is_empty() {
        env_language
    } else {
        "en"
    };
    VoiceSetup {
        language: Arc::from(language),
        output_module: Arc::from(s.output_module.trim()),
        voice: Arc::from(s.voice.trim()),
    }
}

/// `[speech] backend` of the settings file, read once before the speech lane is wired (a change
/// needs a restart). `None` when there is no readable file.
pub async fn startup_backend(path: &Path) -> Option<gorca_config::Backend> {
    let text = read_file(path).await.ok()??;
    Some(
        gorca_config::load(Some(&text), &Presets::builtin())
            .settings
            .speech
            .backend,
    )
}

struct State {
    facts: PlatformFacts,
    /// Hash of the text of the last load (of the empty string for a missing file).
    last_hash: u64,
    last_good: Option<Arc<LoadedSettings>>,
    /// The layouts the active keysym table was built for (the layout poll compares against it).
    layout_ids: Vec<String>,
    /// Set while a keymap build of the layout poll runs on its blocking thread.
    layout_busy: Arc<std::sync::atomic::AtomicBool>,
    /// Parse state of the profiles directory (last good parse per file, PC-0).
    loader: ProfileLoader,
    /// Hash of the profiles directory as last loaded or written by us.
    profiles_hash: u64,
    /// The files of the last good read of the directory.
    last_pfiles: Vec<(String, String)>,
    /// The last read of the directory failed: try again on the retry tick.
    profiles_failed: bool,
}

impl ConfigTask {
    fn publish(&self, b: &Built) {
        self.links.keymap.store(Arc::new(b.compiled.keymap.clone()));
        self.links.settings.store(Arc::new(b.compiled.lane.clone()));
        if let Some(speech) = &self.speech {
            let setup = voice_setup(&b.loaded.settings.speech, &self.env_language);
            speech.control(SpeechControl::SetVoice(setup));
        }
    }

    async fn send(&self, m: SettingsMsg) {
        let send = self.control.send(ControlMsg::Settings(m));
        if tokio::time::timeout(Duration::from_millis(250), send)
            .await
            .is_err()
        {
            tracing::warn!("the reader did not take a settings message in time");
        }
    }

    async fn facts(&self) -> PlatformFacts {
        if self.source.no_probe {
            // No gsettings, but an explicit layout list (tests, the e2e rig) still counts.
            let mut f = self.source.overrides.apply(PlatformFacts::default());
            if let Some(l) = &self.source.overrides.layouts {
                f.keysyms = crate::platform::keysyms(
                    crate::platform::parse_layout_list(l),
                    f.xkb_options.clone(),
                )
                .await;
            }
            f
        } else {
            probe(&self.source.overrides).await
        }
    }

    /// The layouts in effect: the override when given, else the input sources `gsettings`
    /// reports (`None`: unreadable).
    async fn layout_ids(&self) -> Option<Vec<String>> {
        match &self.source.overrides.layouts {
            Some(l) => Some(crate::platform::parse_layout_list(l)),
            None => crate::platform::poll_sources().await,
        }
    }

    /// One tick of the layout poll: read only the `sources` key, and rebuild the table only
    /// when the layouts differ. A failed read or build keeps the table in force and says so.
    async fn poll_layouts(&self, st: &mut State) {
        let Some(ids) = self.layout_ids().await else {
            tracing::debug!("input sources unreadable: keeping the keysym table");
            return;
        };
        if ids == st.layout_ids {
            return;
        }
        match crate::platform::try_keysyms(
            ids.clone(),
            st.facts.xkb_options.clone(),
            &st.layout_busy,
        )
        .await
        {
            Ok(table) => {
                st.layout_ids = ids;
                if table != st.facts.keysyms {
                    tracing::info!("the keyboard layouts changed: grabs rebuilt");
                    st.facts.keysyms = table;
                    self.reload(st, LoadCause::Layouts).await;
                }
            }
            Err(e) => {
                tracing::warn!(error = ?e, "the keysym table for the new layouts was not built in time: keeping the previous table, will retry");
            }
        }
    }

    /// `<settings dir>/profiles`.
    fn profiles_dir(&self) -> Option<PathBuf> {
        let dir = self
            .source
            .path
            .as_ref()?
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        Some(dir.join("profiles"))
    }

    /// The profile files, read off the loop with a deadline (home may be on NFS or FUSE).
    async fn read_profiles(&self) -> (Option<Vec<(String, String)>>, Option<Diag>) {
        let Some(dir) = self.profiles_dir() else {
            return (Some(Vec::new()), None);
        };
        let job = tokio::task::spawn_blocking(move || read_profile_dir(&dir));
        match tokio::time::timeout(READ_DEADLINE, job).await {
            Ok(Ok(Ok(files))) => (Some(files), None),
            Ok(Ok(Err(e))) => (
                None,
                Some(Diag::error(
                    "profiles",
                    None,
                    DiagMsg::Other(format!("the profiles directory could not be read: {e}")),
                )),
            ),
            Ok(Err(_)) | Err(_) => (
                None,
                Some(Diag::error(
                    "profiles",
                    None,
                    DiagMsg::Other(
                        "reading the profiles directory failed or took too long".to_owned(),
                    ),
                )),
            ),
        }
    }

    /// Compile the profile layers of this load and put them, with their problems, into `b`.
    fn attach_profiles(
        &self,
        b: &mut Built,
        st: &mut State,
        files: &[(String, String)],
        read_err: Option<Diag>,
    ) {
        let ext = ExtSchema::new();
        let mut layers = build_layers(&b.patch, b.loaded.hash, files, &ext, &mut st.loader);
        {
            // The speech language of the environment is the lowest layer under an empty
            // setting (the same rule `fill_language` applies to the plain settings).
            let base = Arc::make_mut(&mut layers.base);
            if base.defaults.speech.language.trim().is_empty() && !self.env_language.is_empty() {
                self.env_language
                    .clone_into(&mut base.defaults.speech.language);
            }
            if let Some(k) = gorca_config::KeyId::of("speech", "language")
                && matches!(base.user.file.get(k), Some(gorca_config::Value::Str(v)) if v.trim().is_empty())
            {
                base.user.file.remove(k);
            }
        }
        let mut extra: Vec<Arc<str>> = Vec::new();
        for (stem, d) in &layers.diags {
            extra.push(Arc::from(if stem.is_empty() {
                format!("profiles: {d}")
            } else {
                format!("profile {stem}: {d}")
            }));
        }
        if let Some(e) = read_err {
            extra.push(Arc::from(e.to_string()));
        }
        let l = Arc::make_mut(&mut b.loaded);
        for p in &extra {
            l.problems.push(Arc::clone(p));
            l.problems_hash ^= fnv1a(p.as_bytes());
        }
        l.profiles = Some(Arc::new(layers));
    }

    async fn read(&self) -> (Option<String>, Option<Diag>) {
        let Some(path) = &self.source.path else {
            return (None, None);
        };
        match read_file(path).await {
            Ok(t) => (t, None),
            Err(e) => (
                None,
                Some(Diag::error("", None, DiagMsg::Other(e.to_string()))),
            ),
        }
    }

    /// Load from disk and publish. `startup` publishes even a bad file (as defaults); later
    /// loads keep the last good settings when the file is bad or unreadable (HR-2).
    async fn reload(&self, st: &mut State, cause: LoadCause) {
        let (text, read_err) = self.read().await;
        let (read, perr) = self.read_profiles().await;
        // A failed or slow read of the directory keeps what was loaded (R-3): the profiles and
        // the unsaved edits of this session survive, and the read is retried.
        let (pfiles, phash) = match read {
            Some(f) => {
                let h = profiles_hash(&f);
                st.last_pfiles.clone_from(&f);
                (f, h)
            }
            None => (st.last_pfiles.clone(), st.profiles_hash),
        };
        st.profiles_failed = perr.is_some();
        let hash = fnv1a(text.as_deref().unwrap_or("").as_bytes());
        // Content, not provenance: the file is reloaded whenever its text differs from what is
        // active. Our own save sets `last_hash`, so its inotify event changes nothing, and a
        // later revert to that same text is a real change (F5). The same holds for the profile
        // files.
        if cause == LoadCause::FileChanged && hash == st.last_hash && phash == st.profiles_hash {
            return;
        }
        let mut b = build(text.as_deref(), &st.facts, cause);
        fill_language(&mut b, &self.env_language);
        self.attach_profiles(&mut b, st, &pfiles, perr);
        let startup = cause == LoadCause::Startup;
        if (read_err.is_some() || !b.file_ok)
            && !startup
            && let Some(good) = &st.last_good
        {
            let mut keep = (**good).clone();
            keep.cause = cause;
            keep.problems.clone_from(&b.loaded.problems);
            if let Some(e) = read_err {
                keep.problems.push(Arc::from(e.to_string()));
            }
            keep.problems_hash = fnv1a(
                keep.problems
                    .iter()
                    .flat_map(|p| p.bytes())
                    .collect::<Vec<_>>()
                    .as_slice(),
            );
            tracing::warn!(
                problems = keep.problems.len(),
                "settings file is bad; keeping the last good settings"
            );
            st.last_hash = hash;
            self.send(SettingsMsg::Loaded(Arc::new(keep))).await;
            return;
        }
        if let Some(e) = read_err {
            let mut l = (*b.loaded).clone();
            l.problems.push(Arc::from(e.to_string()));
            l.problems_hash ^= fnv1a(e.to_string().as_bytes());
            b.loaded = Arc::new(l);
        }
        for p in &b.loaded.problems {
            tracing::warn!(problem = %p, "settings");
        }
        st.last_hash = hash;
        st.profiles_hash = phash;
        if b.file_ok {
            st.last_good = Some(Arc::clone(&b.loaded));
        }
        self.publish(&b);
        self.send(SettingsMsg::Loaded(Arc::clone(&b.loaded))).await;
    }

    /// Write one planned step. A profile file that must not exist yet is created with
    /// `O_CREAT | O_EXCL` (PE-6); any other write first checks that the file is still what the
    /// plan was made from (PE-5) and then replaces it atomically.
    async fn write_step(&self, step: &WriteStep) -> Result<(), StepError> {
        let text = step.text.clone();
        match &step.file {
            FileId::Global => {
                let Some(path) = self.source.path.clone() else {
                    return Err(StepError::Io(std::io::Error::other(
                        "no settings file path",
                    )));
                };
                write_atomic(&path, text).await.map_err(StepError::Io)
            }
            FileId::Profile(id) => {
                let Some(dir) = self.profiles_dir() else {
                    return Err(StepError::Io(std::io::Error::other(
                        "no settings file path",
                    )));
                };
                if !is_valid_file_id(id) {
                    return Err(StepError::Io(std::io::Error::other("invalid profile id")));
                }
                let path = dir.join(format!("{id}.toml"));
                let expect = step.expect_hash;
                let create = step.create;
                let job = tokio::task::spawn_blocking(move || {
                    if create {
                        return match create_exclusive(&path, &text) {
                            Ok(()) => Ok(false),
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                                Err(StepError::Changed)
                            }
                            Err(e) => Err(StepError::Io(e)),
                        };
                    }
                    let now = std::fs::read(&path).map_err(StepError::Io)?;
                    if fnv1a(&now) != expect {
                        return Err(StepError::Changed);
                    }
                    Ok(true)
                });
                let replace = match tokio::time::timeout(WRITE_DEADLINE, job).await {
                    Ok(Ok(r)) => r?,
                    Ok(Err(e)) => return Err(StepError::Io(std::io::Error::other(e.to_string()))),
                    Err(_) => return Err(StepError::Deadline),
                };
                if replace {
                    let p = dir.join(format!("{id}.toml"));
                    write_atomic(&p, step.text.clone())
                        .await
                        .map_err(StepError::Io)?;
                }
                Ok(())
            }
        }
    }

    /// The text of the files a plan is made against.
    async fn disk_view(
        &self,
        snapshot: &gorca_config::profiles::ProfileSnapshot,
    ) -> Option<gorca_config::profiles::DiskView> {
        let global = match &self.source.path {
            Some(p) => read_file(p).await.ok().flatten(),
            None => None,
        };
        // A directory that cannot be read cannot be planned against.
        let (files, _) = self.read_profiles().await;
        let _ = snapshot;
        Some(gorca_config::profiles::DiskView {
            global,
            profiles: files?.into_iter().collect(),
        })
    }

    /// `settings.save`, `profiles.save_global` and its second press: plan against the files on
    /// disk, write in plan order, and tell the Reader how far it got (SG-5).
    async fn save_profiles(&self, st: &mut State, req: SaveProfiles) {
        let SaveProfiles { id, snapshot, kind } = req;
        let reply = |plan: Option<Arc<SavePlan>>, outcome: ProfileSaveOutcome, kind: SaveKind| {
            SettingsMsg::ProfilesSaved(Arc::new(ProfilesSaved {
                id,
                plan,
                outcome,
                kind,
            }))
        };
        if self.source.path.is_none() {
            self.send(reply(None, ProfileSaveOutcome::Failed { done: 0 }, kind))
                .await;
            return;
        }
        let Some(disk) = self.disk_view(&snapshot).await else {
            self.send(reply(None, ProfileSaveOutcome::Failed { done: 0 }, kind))
                .await;
            return;
        };
        let planned = match &kind {
            SaveKind::All => snapshot.plan_save(&disk),
            SaveKind::Global { profile_id, focus } => {
                snapshot.plan_save_global(&focus.facts(), profile_id, false, &disk)
            }
            SaveKind::Everywhere { keys, except } => {
                snapshot.plan_apply_everywhere(keys, except, &disk)
            }
        };
        let plan = match planned {
            Ok(p) => p,
            Err(r) => {
                let outcome = match r {
                    SaveRefusal::Text(gorca_config::SaveError::NewerSchema(_)) => {
                        ProfileSaveOutcome::NewerSchema
                    }
                    other => ProfileSaveOutcome::Refused(other),
                };
                self.send(reply(None, outcome, kind)).await;
                return;
            }
        };
        let total = plan.steps.len();
        let mut done = 0usize;
        let mut failed = false;
        for step in &plan.steps {
            match self.write_step(step).await {
                Ok(()) => {
                    done += 1;
                    if step.file == FileId::Global {
                        st.last_hash = fnv1a(step.text.as_bytes());
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "profile save: a write failed");
                    failed = true;
                    break;
                }
            }
        }
        if done > 0 {
            // What is on disk now is what we wrote: the watcher's event is not a change.
            if let (Some(files), _) = self.read_profiles().await {
                st.profiles_hash = profiles_hash(&files);
                st.last_pfiles = files;
            }
            if plan
                .steps
                .iter()
                .take(done)
                .any(|s| s.file == FileId::Global)
                && let Some(path) = &self.source.path
                && let Ok(Some(text)) = read_file(path).await
            {
                // The lane settings follow the saved global file.
                let mut b = build(Some(&text), &st.facts, LoadCause::Saved);
                fill_language(&mut b, &self.env_language);
                if b.file_ok {
                    self.publish(&b);
                }
            }
        }
        let outcome = if failed {
            ProfileSaveOutcome::Failed { done }
        } else {
            ProfileSaveOutcome::Written { done, total }
        };
        self.send(reply(Some(Arc::new(plan)), outcome, kind)).await;
    }

    async fn save(&self, st: &mut State, overlay: gorca_config::Overlay) {
        let Some(path) = self.source.path.clone() else {
            self.send(SettingsMsg::SaveResult(SaveResult::Failed)).await;
            return;
        };
        let current = match read_file(&path).await {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, "settings.save: cannot read the file");
                self.send(SettingsMsg::SaveResult(SaveResult::Failed)).await;
                return;
            }
        };
        // HR-5: only if the file is still what we loaded.
        if fnv1a(current.as_deref().unwrap_or("").as_bytes()) != st.last_hash {
            self.send(SettingsMsg::SaveResult(SaveResult::FileChanged))
                .await;
            return;
        }
        let new_text = match gorca_config::save(current.as_deref().unwrap_or(""), &overlay) {
            Ok(t) => t,
            Err(gorca_config::SaveError::NewerSchema(_)) => {
                self.send(SettingsMsg::SaveResult(SaveResult::NewerSchema))
                    .await;
                return;
            }
            Err(e) => {
                tracing::warn!(error = %e, "settings.save");
                self.send(SettingsMsg::SaveResult(SaveResult::Failed)).await;
                return;
            }
        };
        if let Err(e) = write_atomic(&path, new_text.clone()).await {
            tracing::warn!(error = %e, "settings.save: write failed");
            self.send(SettingsMsg::SaveResult(SaveResult::Failed)).await;
            return;
        }
        st.last_hash = fnv1a(new_text.as_bytes());
        let mut b = build(Some(&new_text), &st.facts, LoadCause::Saved);
        fill_language(&mut b, &self.env_language);
        if b.file_ok {
            st.last_good = Some(Arc::clone(&b.loaded));
        }
        self.publish(&b);
        self.send(SettingsMsg::Loaded(b.loaded)).await;
    }

    fn watcher(&self) -> Option<(Inotify, OsString, bool)> {
        let path = self.source.path.as_ref()?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path.file_name()?.to_owned();
        let ino = Inotify::init().ok()?;
        let mask = WatchMask::CLOSE_WRITE
            | WatchMask::MOVED_TO
            | WatchMask::MOVED_FROM
            | WatchMask::CREATE
            | WatchMask::DELETE;
        ino.watches().add(dir, mask).ok()?;
        // The profiles directory may not exist yet: the retry tick adds the watch later.
        let watching_profiles = self
            .profiles_dir()
            .is_some_and(|pd| ino.watches().add(pd, mask).is_ok());
        Some((ino, name, watching_profiles))
    }

    /// Open the event stream of the watcher and say whether it covers the profiles directory.
    fn open_events(&self) -> (Option<(EventStream, OsString)>, bool) {
        match self.watcher().and_then(|(ino, name, p)| {
            ino.into_event_stream([0u8; 1024])
                .ok()
                .map(|s| (s, name, p))
        }) {
            Some((s, n, p)) => (Some((s, n)), p),
            None => (None, false),
        }
    }

    /// Is there a profiles directory that a watcher was created without?
    fn profiles_unwatched(&self, watching: bool) -> bool {
        !watching && self.profiles_dir().is_some_and(|d| d.is_dir())
    }

    /// Run until `shutdown`.
    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut st = State {
            facts: self.facts().await,
            last_hash: 0,
            last_good: None,
            layout_ids: self.layout_ids().await.unwrap_or_default(),
            layout_busy: Arc::default(),
            loader: ProfileLoader::new(),
            profiles_hash: 0,
            last_pfiles: Vec::new(),
            profiles_failed: false,
        };
        self.reload(&mut st, LoadCause::Startup).await;

        let (mut events, mut profiles_watched) = self.open_events();
        let mut retry = tokio::time::interval(WATCH_RETRY);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // The input sources can change while we run (K-8b): look again, slowly, off the lanes.
        let mut layouts = tokio::time::interval(LAYOUT_POLL);
        layouts.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        layouts.tick().await;
        loop {
            tokio::select! {
                biased;
                () = shutdown.cancelled() => return,
                req = self.requests.recv() => {
                    let Some(req) = req else { return };
                    match req {
                        ConfigRequest::Reload => {
                            st.facts = self.facts().await;
                            self.reload(&mut st, LoadCause::Explicit).await;
                        }
                        ConfigRequest::Save(o) => self.save(&mut st, o).await,
                        ConfigRequest::SaveProfiles(r) => self.save_profiles(&mut st, *r).await,
                        ConfigRequest::Prewarm(p) => {
                            let entries = gorca_core::prewarm(&p);
                            self.send(SettingsMsg::Prewarm(Arc::new(entries))).await;
                        }
                    }
                }
                ev = next_event(&mut events), if events.is_some() => {
                    match ev {
                        Some(true) => {
                            // Debounce: the latest write wins, one load in flight.
                            if let Some((s, name)) = events.as_mut() {
                                drain_quiet(s, name).await;
                            }
                            self.reload(&mut st, LoadCause::FileChanged).await;
                        }
                        Some(false) => {}
                        None => events = None,
                    }
                }
                _ = layouts.tick(), if !self.source.no_probe => {
                    self.poll_layouts(&mut st).await;
                }
                _ = retry.tick(), if (events.is_none() || self.profiles_unwatched(profiles_watched) || st.profiles_failed) && self.source.path.is_some() => {
                    // The directory did not exist (or the watch broke), or the profiles
                    // directory appeared after the watcher was made, or its last read failed:
                    // look again.
                    let rewatch = events.is_none() || self.profiles_unwatched(profiles_watched);
                    if rewatch {
                        (events, profiles_watched) = self.open_events();
                    }
                    if (rewatch && events.is_some()) || st.profiles_failed {
                        self.reload(&mut st, LoadCause::FileChanged).await;
                    }
                }
            }
        }
    }
}

type EventStream = inotify::EventStream<[u8; 1024]>;

/// `Some(true)`: an event for our file; `Some(false)`: another file; `None`: the stream ended.
async fn next_event(s: &mut Option<(EventStream, OsString)>) -> Option<bool> {
    let (stream, name) = s.as_mut()?;
    match stream.next().await {
        Some(Ok(ev)) => Some(relevant(ev.name.as_deref(), name)),
        Some(Err(e)) => {
            tracing::warn!(error = %e, "settings watcher failed");
            None
        }
        None => None,
    }
}

/// An event for the settings file or for a profile file (`*.toml`); temporary files of our own
/// atomic writes and dot files never count.
#[allow(clippy::case_sensitive_file_extension_comparisons)] // profile files are exactly `*.toml`
fn relevant(event_name: Option<&std::ffi::OsStr>, settings_name: &OsString) -> bool {
    let Some(n) = event_name else { return false };
    if n == settings_name.as_os_str() {
        return true;
    }
    n.to_str()
        .is_some_and(|n| n.ends_with(".toml") && !n.starts_with('.'))
}

/// Swallow events until the file has been quiet for [`DEBOUNCE`].
async fn drain_quiet(s: &mut EventStream, name: &OsString) {
    loop {
        match tokio::time::timeout(DEBOUNCE, s.next()).await {
            Ok(Some(Ok(ev))) => {
                let _ = relevant(ev.name.as_deref(), name);
            }
            Ok(Some(Err(_)) | None) | Err(_) => return,
        }
    }
}

/// `gorca config check|defaults` (CV-3): the text to print and whether the file is clean. This
/// is the only place where a bad settings file is an error.
pub fn run_config_cmd(cmd: &crate::ConfigCmd, default_path: Option<&Path>) -> (String, bool) {
    match cmd {
        crate::ConfigCmd::Defaults => (gorca_config::defaults_toml(), true),
        crate::ConfigCmd::PrintDefault => (gorca_config::defaults_toml_commented(), true),
        crate::ConfigCmd::Check(path) => {
            let Some(path) = path.as_deref().or(default_path) else {
                return ("no settings file path is known\n".to_owned(), false);
            };
            let text = match std::fs::read(path) {
                Ok(b) => Some(String::from_utf8_lossy(&b).into_owned()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return (format!("{}: {e}\n", path.display()), false),
            };
            let b = build(
                text.as_deref(),
                &PlatformFacts::default(),
                LoadCause::Startup,
            );
            if b.loaded.problems.is_empty() {
                let what = if text.is_some() {
                    "no problems"
                } else {
                    "no file, using the defaults"
                };
                return (format!("{}: {what}\n", path.display()), true);
            }
            let mut out = String::new();
            for p in &b.loaded.problems {
                let _ = writeln!(out, "{}: {p}", path.display());
            }
            (out, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_core::PlatformFacts;

    fn facts() -> PlatformFacts {
        PlatformFacts::default()
    }

    #[test]
    fn an_empty_language_is_filled_from_the_environment() {
        let mut b = build(None, &facts(), LoadCause::Startup);
        fill_language(&mut b, "ru");
        assert_eq!(b.loaded.settings.speech.language, "ru");
        let mut b = build(
            Some("[speech]\nlanguage=\"de\"\n"),
            &facts(),
            LoadCause::Startup,
        );
        fill_language(&mut b, "ru");
        assert_eq!(b.loaded.settings.speech.language, "de");
    }

    #[test]
    fn config_check_reports_problems_and_defaults_round_trip() {
        let dir = tmpdir("check");
        let good = dir.join("good.toml");
        let bad = dir.join("bad.toml");
        std::fs::write(&good, "[speech]\nrate = 60\n").unwrap();
        std::fs::write(&bad, "[speech]\nrate = 500\nvolum = 1\n").unwrap();
        let (out, ok) = run_config_cmd(&crate::ConfigCmd::Check(Some(good.clone())), None);
        assert!(ok && out.contains("no problems"), "{out}");
        let (out, ok) = run_config_cmd(&crate::ConfigCmd::Check(Some(bad)), None);
        assert!(
            !ok && out.lines().count() == 2 && out.contains("line 2"),
            "{out}"
        );
        let (out, ok) = run_config_cmd(&crate::ConfigCmd::Check(Some(dir.join("none.toml"))), None);
        assert!(ok && out.contains("using the defaults"));
        let (out, ok) = run_config_cmd(&crate::ConfigCmd::Defaults, None);
        assert!(ok);
        assert!(
            build(Some(&out), &facts(), LoadCause::Startup)
                .loaded
                .problems
                .is_empty()
        );
        assert!(!run_config_cmd(&crate::ConfigCmd::Check(None), None).1);
    }

    #[test]
    fn build_never_fails_and_reports_problems_with_line_numbers() {
        let b = build(
            Some("[speech]\nrate = \"x\"\n"),
            &facts(),
            LoadCause::Startup,
        );
        assert!(b.file_ok);
        assert_eq!(b.loaded.problems.len(), 1);
        assert!(
            b.loaded.problems[0].starts_with("line 2, "),
            "{:?}",
            b.loaded.problems
        );
        let b = build(Some("[speech\n"), &facts(), LoadCause::Startup);
        assert!(!b.file_ok);
        assert_eq!(b.loaded.settings, gorca_config::Settings::default());
        let b = build(None, &facts(), LoadCause::Startup);
        assert!(b.file_ok && b.loaded.problems.is_empty());
        assert!(b.compiled.keymap.len() > 100);
    }

    #[test]
    fn the_voice_setup_prefers_the_setting_then_the_locale_then_english() {
        let mut s = gorca_config::SpeechSettings::default();
        assert_eq!(&*voice_setup(&s, "ru").language, "ru");
        assert_eq!(&*voice_setup(&s, "").language, "en");
        s.language = " de ".to_owned();
        s.output_module = "rhvoice".to_owned();
        let v = voice_setup(&s, "ru");
        assert_eq!(
            (&*v.language, &*v.output_module, &*v.voice),
            ("de", "rhvoice", "")
        );
    }

    #[tokio::test]
    async fn the_backend_comes_from_the_file() {
        let d = tmpdir("backend");
        let p = d.join("gorca.toml");
        assert_eq!(startup_backend(&p).await, None);
        std::fs::write(&p, "[speech]\nbackend = \"ssip\"\n").unwrap();
        assert_eq!(startup_backend(&p).await, Some(gorca_config::Backend::Ssip));
    }

    #[test]
    fn caps_lock_is_dropped_from_the_published_keys() {
        let f = PlatformFacts {
            xkb_options: "grp:caps_toggle".into(),
            ..facts()
        };
        let b = build(None, &f, LoadCause::Startup);
        assert!(b.loaded.caps_lock_dropped);
        assert_eq!(b.loaded.mode_keys.modifiers.len(), 2);
    }

    fn task(
        dir: &Path,
    ) -> (
        ConfigTask,
        mpsc::Receiver<ControlMsg>,
        mpsc::Sender<ConfigRequest>,
    ) {
        let (ctl_tx, ctl_rx) = mpsc::channel(32);
        let (req_tx, req_rx) = mpsc::channel(8);
        (
            ConfigTask {
                source: ConfigSource {
                    path: Some(dir.join("gorca.toml")),
                    overrides: PlatformOverrides::default(),
                    no_probe: true,
                },
                links: LaneLinks::new(),
                control: ctl_tx,
                requests: req_rx,
                speech: None,
                env_language: String::new(),
            },
            ctl_rx,
            req_tx,
        )
    }

    async fn loaded(rx: &mut mpsc::Receiver<ControlMsg>) -> Arc<LoadedSettings> {
        let m = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("a settings message")
            .expect("channel open");
        match m {
            ControlMsg::Settings(SettingsMsg::Loaded(l)) => l,
            other => panic!("unexpected {other:?}"),
        }
    }

    /// A private temporary directory, removed on every exit path, panics included.
    struct TmpDir(PathBuf);

    impl std::ops::Deref for TmpDir {
        type Target = PathBuf;
        fn deref(&self) -> &PathBuf {
            &self.0
        }
    }

    impl AsRef<Path> for TmpDir {
        fn as_ref(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn tmpdir(tag: &str) -> TmpDir {
        let d = std::env::temp_dir().join(format!("gorca-cfg-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        TmpDir(d)
    }

    #[tokio::test]
    async fn startup_hot_reload_explicit_reload_and_save() {
        let dir = tmpdir("flow");
        let path = dir.join("gorca.toml");
        std::fs::write(&path, "[speech]\nrate = 40\n").unwrap();
        let (t, mut rx, req) = task(&dir);
        let links = Arc::clone(&t.links);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));

        let l = loaded(&mut rx).await;
        assert_eq!((l.cause, l.settings.speech.rate), (LoadCause::Startup, 40));
        assert!(
            links.keymap.load().len() > 100,
            "the keymap is published first"
        );

        // Hot reload through inotify (an atomic replace, like editors do).
        std::fs::write(dir.join("new.tmp"), "[speech]\nrate = 70\n").unwrap();
        std::fs::rename(dir.join("new.tmp"), &path).unwrap();
        let l = loaded(&mut rx).await;
        assert_eq!(
            (l.cause, l.settings.speech.rate),
            (LoadCause::FileChanged, 70)
        );

        // A bad edit keeps the last good settings and reports the problem.
        std::fs::write(&path, "[speech\n").unwrap();
        let l = loaded(&mut rx).await;
        assert_eq!(l.settings.speech.rate, 70);
        assert!(!l.problems.is_empty());
        std::fs::write(&path, "[speech]\nrate = 70\n").unwrap();
        let _ = loaded(&mut rx).await;

        // Save merges the overlay, keeps the file's text, and is not reloaded a second time.
        let o = gorca_config::Overlay {
            pitch: Some(33),
            ..gorca_config::Overlay::default()
        };
        req.send(ConfigRequest::Save(o)).await.unwrap();
        let l = loaded(&mut rx).await;
        assert_eq!((l.cause, l.settings.speech.pitch), (LoadCause::Saved, 33));
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(
            text.contains("pitch = 33") && text.contains("rate = 70"),
            "{text}"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .is_err(),
            "our own write is not a reload"
        );

        // HR-5: a file edited behind our back is not overwritten.
        std::fs::write(&path, "[speech]\nrate = 11\n").unwrap();
        let _ = loaded(&mut rx).await;
        std::fs::write(&path, "[speech]\nrate = 12\n").unwrap();
        // Race the save against the watcher: whichever order, the result is never a silent
        // overwrite of rate = 12.
        req.send(ConfigRequest::Save(gorca_config::Overlay {
            volume: Some(1),
            ..gorca_config::Overlay::default()
        }))
        .await
        .unwrap();
        for _ in 0..3 {
            match tokio::time::timeout(Duration::from_secs(5), rx.recv()).await {
                Ok(Some(ControlMsg::Settings(SettingsMsg::SaveResult(
                    SaveResult::FileChanged,
                )))) => break,
                Ok(Some(ControlMsg::Settings(SettingsMsg::Loaded(l))))
                    if l.cause == LoadCause::Saved =>
                {
                    let text = std::fs::read_to_string(&path).unwrap();
                    assert!(
                        text.contains("rate = 12"),
                        "an edit must never be lost: {text}"
                    );
                    break;
                }
                Ok(Some(_)) => {}
                other => panic!("{other:?}"),
            }
        }

        // Explicit reload re-reads even if nothing changed.
        req.send(ConfigRequest::Reload).await.unwrap();
        loop {
            let l = loaded(&mut rx).await;
            if l.cause == LoadCause::Explicit {
                break;
            }
        }
        stop.cancel();
        let _ = h.await;
    }

    async fn next_msg(rx: &mut mpsc::Receiver<ControlMsg>) -> SettingsMsg {
        loop {
            let m = tokio::time::timeout(Duration::from_secs(5), rx.recv())
                .await
                .expect("a settings message")
                .expect("channel open");
            if let ControlMsg::Settings(s) = m {
                return s;
            }
        }
    }

    async fn saved(rx: &mut mpsc::Receiver<ControlMsg>) -> Arc<ProfilesSaved> {
        loop {
            if let SettingsMsg::ProfilesSaved(r) = next_msg(rx).await {
                return r;
            }
        }
    }

    const FF: &str = "schema = 1\n[profile]\nname = \"Firefox\"\ntrigger = [{ app = { name = [\"firefox\"] } }]\n[speech]\nrate = 70\n";

    #[tokio::test]
    async fn profiles_load_reload_on_change_and_report_their_problems() {
        let dir = tmpdir("profiles-load");
        std::fs::write(dir.join("gorca.toml"), "[speech]\nrate = 40\n").unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::write(dir.join("profiles/ff.toml"), FF).unwrap();
        std::fs::write(dir.join("profiles/bad.toml"), "not toml = = =").unwrap();
        std::fs::write(dir.join("profiles/notes.txt"), "ignored").unwrap();
        let (t, mut rx, _req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        let p = l.profiles.as_ref().expect("profile layers");
        assert!(p.set.id_of("ff").is_some());
        assert!(p.set.id_of("gorca/vscode").is_some(), "the product row");
        assert!(p.set.id_of("bad").is_none() && p.set.id_of("notes").is_none());
        assert!(
            l.problems.iter().any(|x| x.starts_with("profile bad:")),
            "{:?}",
            l.problems
        );
        // The global layer is the file's keys only.
        assert_eq!(l.settings.speech.rate, 40);

        // A profile file edited on disk is picked up (inotify on the directory).
        std::fs::write(dir.join("profiles/new.tmp"), FF.replace("70", "33")).unwrap();
        std::fs::rename(dir.join("profiles/new.tmp"), dir.join("profiles/ff.toml")).unwrap();
        let l = loop {
            let l = loaded(&mut rx).await;
            let rate = l
                .profiles
                .as_ref()
                .and_then(|p| p.set.get(p.set.id_of("ff")?))
                .map(|q| q.values.len());
            if rate.is_some() && l.cause == LoadCause::FileChanged {
                break l;
            }
        };
        assert!(l.profiles.is_some());
        // Editing a file that is not a profile does not reload.
        std::fs::write(dir.join("profiles/readme.txt"), "x").unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(400), rx.recv())
                .await
                .is_err()
        );
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn saving_profiles_creates_a_new_file_exclusively_and_never_overwrites_a_changed_one() {
        use gorca_config::profiles::{AppFacts, Edit, FocusFacts, KeyRef, ProfileSnapshot};
        let dir = tmpdir("profiles-save");
        std::fs::write(dir.join("gorca.toml"), "[speech]\nrate = 40\n").unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        let p = l.profiles.clone().expect("profile layers");
        let facts = FocusFacts {
            app: Some(AppFacts {
                name: "Firefox",
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        };
        let rate = gorca_config::KeyId::of("speech", "rate").unwrap();
        let snap = ProfileSnapshot::new(Arc::clone(&p.set), Arc::clone(&p.base));
        let plan = snap
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(rate),
                    value: gorca_config::Value::Int(77),
                },
            )
            .unwrap();
        let snap = snap.apply_edit(&facts, &plan).snapshot;
        let snap = Arc::new(snap);
        req.send(ConfigRequest::SaveProfiles(Box::new(SaveProfiles {
            id: 1,
            snapshot: Arc::clone(&snap),
            kind: SaveKind::All,
        })))
        .await
        .unwrap();
        let r = saved(&mut rx).await;
        assert!(
            matches!(r.outcome, ProfileSaveOutcome::Written { done: 1, total: 1 }),
            "{:?}",
            r.outcome
        );
        let files: Vec<_> = std::fs::read_dir(dir.join("profiles"))
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files.len(), 1, "{files:?}");
        assert!(
            files[0].starts_with("firefox-")
                && Path::new(&files[0])
                    .extension()
                    .is_some_and(|e| e == "toml")
        );
        let written = std::fs::read_to_string(dir.join("profiles").join(&files[0])).unwrap();
        assert!(
            written.contains("rate = 77") && written.contains("Firefox"),
            "{written}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("gorca.toml")).unwrap(),
            "[speech]\nrate = 40\n",
            "the global file is untouched"
        );
        // The write is not a change to reload.
        assert!(
            tokio::time::timeout(Duration::from_millis(500), rx.recv())
                .await
                .is_err(),
            "our own write is not a reload"
        );
        // The same snapshot again (it still believes the profile has no file): the plan sees
        // the file and refuses; nothing is overwritten.
        req.send(ConfigRequest::SaveProfiles(Box::new(SaveProfiles {
            id: 1,
            snapshot: Arc::clone(&snap),
            kind: SaveKind::All,
        })))
        .await
        .unwrap();
        let r = saved(&mut rx).await;
        assert!(
            matches!(
                r.outcome,
                ProfileSaveOutcome::Refused(SaveRefusal::ChangedOnDisk(FileId::Profile(_)))
            ),
            "{:?}",
            r.outcome
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("profiles").join(&files[0])).unwrap(),
            written
        );
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn fifos_and_links_in_the_profiles_directory_are_skipped_without_blocking() {
        let dir = tmpdir("profiles-hostile");
        std::fs::write(dir.join("gorca.toml"), "").unwrap();
        let pd = dir.join("profiles");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("ff.toml"), FF).unwrap();
        let made = std::process::Command::new("mkfifo")
            .arg(pd.join("pipe.toml"))
            .status()
            .is_ok_and(|s| s.success());
        assert!(made, "mkfifo");
        std::os::unix::fs::symlink(pd.join("ff.toml"), pd.join("link.toml")).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let p2 = pd.clone();
        std::thread::spawn(move || {
            let _ = tx.send(read_profile_dir(&p2));
        });
        let files = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the read must not block on a FIFO")
            .unwrap();
        let stems: Vec<_> = files.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(stems, ["ff"]);
    }

    #[tokio::test]
    async fn an_unreadable_profiles_directory_keeps_the_loaded_profiles_and_is_retried() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir("profiles-keep");
        std::fs::write(dir.join("gorca.toml"), "").unwrap();
        let pd = dir.join("profiles");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("ff.toml"), FF).unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        assert!(l.profiles.as_ref().unwrap().set.id_of("ff").is_some());
        std::fs::set_permissions(&pd, std::fs::Permissions::from_mode(0o000)).unwrap();
        req.send(ConfigRequest::Reload).await.unwrap();
        let l = loop {
            let l = loaded(&mut rx).await;
            if l.cause == LoadCause::Explicit {
                break l;
            }
        };
        let kept = l.profiles.as_ref().unwrap().set.id_of("ff").is_some();
        let said = l.problems.iter().any(|p| p.contains("profiles directory"));
        std::fs::set_permissions(&pd, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(kept, "the last good profiles stay");
        assert!(said, "{:?}", l.problems);
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn an_existing_empty_profile_file_can_be_saved() {
        use gorca_config::profiles::{AppFacts, Edit, FocusFacts, KeyRef, ProfileSnapshot};
        let dir = tmpdir("profiles-empty");
        std::fs::write(dir.join("gorca.toml"), "").unwrap();
        let pd = dir.join("profiles");
        std::fs::create_dir_all(&pd).unwrap();
        std::fs::write(pd.join("ff.toml"), "").unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        // An empty file is an existing file: a save never treats it as a create (R-6).
        let p = l.profiles.clone().unwrap();
        let facts = FocusFacts {
            app: Some(AppFacts {
                name: "ff",
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        };
        let snap = ProfileSnapshot::new(Arc::clone(&p.set), Arc::clone(&p.base));
        let rate = gorca_config::KeyId::of("speech", "rate").unwrap();
        let plan = snap
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(rate),
                    value: gorca_config::Value::Int(61),
                },
            )
            .unwrap();
        let snap = Arc::new(snap.apply_edit(&facts, &plan).snapshot);
        req.send(ConfigRequest::SaveProfiles(Box::new(SaveProfiles {
            id: 9,
            snapshot: snap,
            kind: SaveKind::All,
        })))
        .await
        .unwrap();
        let r = saved(&mut rx).await;
        assert_eq!(r.id, 9);
        assert!(
            matches!(r.outcome, ProfileSaveOutcome::Written { done: 1, total: 1 }),
            "{:?}",
            r.outcome
        );
        assert!(
            std::fs::read_to_string(pd.join("ff.toml"))
                .unwrap()
                .is_empty()
        );
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn a_symlink_in_the_way_of_a_new_profile_is_never_followed() {
        let dir = tmpdir("profiles-link");
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        let victim = dir.join("victim.txt");
        std::fs::write(&victim, "keep").unwrap();
        std::os::unix::fs::symlink(&victim, dir.join("profiles/x.toml")).unwrap();
        let e = create_exclusive(&dir.join("profiles/x.toml"), "overwritten").unwrap_err();
        assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep");
    }

    #[tokio::test]
    async fn save_global_moves_the_touched_key_and_prewarm_answers() {
        use gorca_config::profiles::{AppFacts, Edit, FocusFacts, KeyRef, ProfileSnapshot};
        let dir = tmpdir("profiles-global");
        std::fs::write(dir.join("gorca.toml"), "[speech]\nrate = 40\n").unwrap();
        std::fs::create_dir_all(dir.join("profiles")).unwrap();
        std::fs::write(dir.join("profiles/firefox.toml"), FF).unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        let p = l.profiles.clone().expect("profile layers");
        let facts = FocusFacts {
            app: Some(AppFacts {
                name: "firefox",
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        };
        let rate = gorca_config::KeyId::of("speech", "rate").unwrap();
        let snap = ProfileSnapshot::new(Arc::clone(&p.set), Arc::clone(&p.base));
        let plan = snap
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(rate),
                    value: gorca_config::Value::Int(88),
                },
            )
            .unwrap();
        let snap = Arc::new(snap.apply_edit(&facts, &plan).snapshot);
        let focus = gorca_core::ProfileFocus {
            app: Some(Arc::from("firefox")),
            ..gorca_core::ProfileFocus::default()
        };
        req.send(ConfigRequest::Prewarm(Box::new(
            gorca_core::PrewarmRequest {
                snapshot: Arc::clone(&snap),
                focus: focus.clone(),
            },
        )))
        .await
        .unwrap();
        let SettingsMsg::Prewarm(entries) = next_msg(&mut rx).await else {
            panic!("a prewarm answer")
        };
        assert_eq!(entries.generation, snap.generation);
        assert!(!entries.entries.is_empty());
        req.send(ConfigRequest::SaveProfiles(Box::new(SaveProfiles {
            id: 1,
            snapshot: Arc::clone(&snap),
            kind: SaveKind::Global {
                profile_id: "firefox".to_owned(),
                focus,
            },
        })))
        .await
        .unwrap();
        let r = saved(&mut rx).await;
        assert!(
            matches!(r.outcome, ProfileSaveOutcome::Written { done: 2, total: 2 }),
            "{:?}",
            r.outcome
        );
        let global = std::fs::read_to_string(dir.join("gorca.toml")).unwrap();
        assert!(global.contains("rate = 88"), "{global}");
        let prof = std::fs::read_to_string(dir.join("profiles/firefox.toml")).unwrap();
        assert!(!prof.contains("rate"), "the key left the profile: {prof}");
        assert!(
            prof.contains("name = \"Firefox\""),
            "the rest is kept: {prof}"
        );
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn reverting_to_our_own_saved_text_is_reloaded() {
        let dir = tmpdir("revert");
        let path = dir.join("gorca.toml");
        std::fs::write(&path, "[speech]\nrate = 40\n").unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let _ = loaded(&mut rx).await;
        req.send(ConfigRequest::Save(gorca_config::Overlay {
            pitch: Some(33),
            ..gorca_config::Overlay::default()
        }))
        .await
        .unwrap();
        let _ = loaded(&mut rx).await;
        let saved = std::fs::read_to_string(&path).unwrap();
        // The user edits the file, then puts our saved text back.
        std::fs::write(&path, "[speech]\nrate = 99\n").unwrap();
        assert_eq!(loaded(&mut rx).await.settings.speech.rate, 99);
        std::fs::write(&path, &saved).unwrap();
        let l = loaded(&mut rx).await;
        assert_eq!((l.settings.speech.rate, l.settings.speech.pitch), (40, 33));
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn save_writes_through_a_symlinked_settings_file_and_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tmpdir("link");
        let real = dir.join("dotfiles");
        std::fs::create_dir_all(&real).unwrap();
        let target = real.join("gorca.toml");
        std::fs::write(&target, "[speech]\nrate = 40\n").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();
        let link = dir.join("gorca.toml");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        let (t, mut rx, req) = task(&dir);
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let _ = loaded(&mut rx).await;
        req.send(ConfigRequest::Save(gorca_config::Overlay {
            volume: Some(70),
            ..gorca_config::Overlay::default()
        }))
        .await
        .unwrap();
        let _ = loaded(&mut rx).await;
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link is still a link"
        );
        assert!(
            std::fs::read_to_string(&target)
                .unwrap()
                .contains("volume = 70")
        );
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn a_missing_file_means_defaults_and_a_late_file_is_picked_up() {
        let dir = tmpdir("late");
        let sub = dir.join("later");
        let (mut t, mut rx, _req) = task(&dir);
        t.source.path = Some(sub.join("gorca.toml"));
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        assert_eq!(l.settings, gorca_config::Settings::default());
        assert!(l.problems.is_empty());
        // The directory appears later (first run, then the user creates the file).
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(sub.join("gorca.toml"), "[speech]\nrate = 20\n").unwrap();
        let l = loaded(&mut rx).await;
        assert_eq!(l.settings.speech.rate, 20);
        stop.cancel();
        let _ = h.await;
    }

    #[tokio::test]
    async fn no_path_means_defaults_without_io() {
        let (ctl_tx, mut rx) = mpsc::channel(8);
        let (_req_tx, req_rx) = mpsc::channel(8);
        let t = ConfigTask {
            source: ConfigSource {
                path: None,
                overrides: PlatformOverrides::default(),
                no_probe: true,
            },
            links: LaneLinks::new(),
            control: ctl_tx,
            requests: req_rx,
            speech: None,
            env_language: String::new(),
        };
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        assert_eq!(l.cause, LoadCause::Startup);
        stop.cancel();
        let _ = h.await;
    }
}
