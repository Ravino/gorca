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
use gorca_config::{Diag, DiagMsg, Presets, diags_hash, fnv1a};
use gorca_core::{
    Compiled, ConfigRequest, ControlMsg, LoadCause, LoadedSettings, ModeKeys, PlatformFacts,
    SaveResult, SettingsMsg, compile,
};
use gorca_input_api::{LaneLinks, StaticResolver};
use gorca_rt::{CancellationToken, mpsc};
use inotify::{Inotify, WatchMask};

use crate::platform::{PlatformOverrides, probe};

/// Reads of the settings file give up after this (home may be on NFS or FUSE).
pub const READ_DEADLINE: Duration = Duration::from_secs(1);
/// The file must stay quiet this long before it is reloaded.
pub const DEBOUNCE: Duration = Duration::from_millis(100);
const WATCH_RETRY: Duration = Duration::from_secs(3);

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
    let loaded = LoadedSettings {
        cause,
        settings: l.settings.clone(),
        hash: l.hash,
        problems: diags.iter().map(|d| render(d, text)).collect(),
        problems_hash: diags_hash(&diags),
        newer_schema: l.newer_schema,
        mode_keys: ModeKeys {
            modifiers: mods,
            ..ModeKeys::default()
        },
        caps_lock_dropped: c.caps_lock_dropped,
        clock_24h: facts.clock_24h,
    };
    Built {
        loaded: Arc::new(loaded),
        compiled: c,
        file_ok: l.file_ok && !panicked,
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

#[derive(Debug)]
pub struct ConfigTask {
    pub source: ConfigSource,
    pub links: Arc<LaneLinks>,
    pub control: mpsc::Sender<ControlMsg>,
    pub requests: mpsc::Receiver<ConfigRequest>,
}

struct State {
    facts: PlatformFacts,
    /// Hash of the text of the last load (of the empty string for a missing file).
    last_hash: u64,
    last_good: Option<Arc<LoadedSettings>>,
}

impl ConfigTask {
    fn publish(&self, b: &Built) {
        self.links.keymap.store(Arc::new(b.compiled.keymap.clone()));
        self.links.settings.store(Arc::new(b.compiled.lane.clone()));
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
            self.source.overrides.apply(PlatformFacts::default())
        } else {
            probe(&self.source.overrides).await
        }
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
        let hash = fnv1a(text.as_deref().unwrap_or("").as_bytes());
        // Content, not provenance: the file is reloaded whenever its text differs from what is
        // active. Our own save sets `last_hash`, so its inotify event changes nothing, and a
        // later revert to that same text is a real change (F5).
        if cause == LoadCause::FileChanged && hash == st.last_hash {
            return;
        }
        let mut b = build(text.as_deref(), &st.facts, cause);
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
        if b.file_ok {
            st.last_good = Some(Arc::clone(&b.loaded));
        }
        self.publish(&b);
        self.send(SettingsMsg::Loaded(Arc::clone(&b.loaded))).await;
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
        let b = build(Some(&new_text), &st.facts, LoadCause::Saved);
        if b.file_ok {
            st.last_good = Some(Arc::clone(&b.loaded));
        }
        self.publish(&b);
        self.send(SettingsMsg::Loaded(b.loaded)).await;
    }

    fn watcher(&self) -> Option<(Inotify, OsString)> {
        let path = self.source.path.as_ref()?;
        let dir = path
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let name = path.file_name()?.to_owned();
        let ino = Inotify::init().ok()?;
        ino.watches()
            .add(
                dir,
                WatchMask::CLOSE_WRITE
                    | WatchMask::MOVED_TO
                    | WatchMask::MOVED_FROM
                    | WatchMask::CREATE
                    | WatchMask::DELETE,
            )
            .ok()?;
        Some((ino, name))
    }

    /// Run until `shutdown`.
    pub async fn run(mut self, shutdown: CancellationToken) {
        let mut st = State {
            facts: self.facts().await,
            last_hash: 0,
            last_good: None,
        };
        self.reload(&mut st, LoadCause::Startup).await;

        let mut events = self
            .watcher()
            .map(|(ino, name)| (ino.into_event_stream([0u8; 1024]), name))
            .and_then(|(s, n)| s.ok().map(|s| (s, n)));
        let mut retry = tokio::time::interval(WATCH_RETRY);
        retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
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
                _ = retry.tick(), if events.is_none() && self.source.path.is_some() => {
                    // The directory did not exist (or the watch broke): look again.
                    events = self
                        .watcher()
                        .map(|(ino, name)| (ino.into_event_stream([0u8; 1024]), name))
                        .and_then(|(s, n)| s.ok().map(|s| (s, n)));
                    if events.is_some() {
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
        Some(Ok(ev)) => Some(ev.name.as_deref() == Some(name.as_os_str())),
        Some(Err(e)) => {
            tracing::warn!(error = %e, "settings watcher failed");
            None
        }
        None => None,
    }
}

/// Swallow events until the file has been quiet for [`DEBOUNCE`].
async fn drain_quiet(s: &mut EventStream, name: &OsString) {
    loop {
        match tokio::time::timeout(DEBOUNCE, s.next()).await {
            Ok(Some(Ok(ev))) => {
                let _ = ev.name.as_deref() == Some(name.as_os_str());
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
        };
        let stop = CancellationToken::new();
        let h = tokio::spawn(t.run(stop.clone()));
        let l = loaded(&mut rx).await;
        assert_eq!(l.cause, LoadCause::Startup);
        stop.cancel();
        let _ = h.await;
    }
}
