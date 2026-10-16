//! The host: loads manifests and components, matches them to applications, owns the worker that
//! runs commands and flushes settings, and implements the core's [`ExtSource`] hook points.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, RwLock};
use std::thread::JoinHandle;
use std::time::Duration;

use gorca_core::{
    AppExt, ContextMask, ExtCmdSpec, ExtCommands, ExtSink, ExtSource, ExtVerdict, MirrorDirectory,
    PresentContext,
};
use gorca_ext_api as api;
use gorca_model::{AppId, AppInfo, NodeEvent, NodeSnapshot, Segment};

use crate::convert::{event_to_ext, node_to_ext, segments_from_ext};
use crate::instance::{ExtStats, ExtStatsSnapshot, HookKind, InstShared};
use crate::kv::SettingsKv;
use crate::manifest::{ExtBackend, HookSet, Manifest, order_matching};
use crate::manifest_file::MAX_MANIFEST_BYTES;
use crate::native::NativeFactory;
use crate::slot::{Decision, Factory, Job, LoadedExt, Outcome, Slot};
use crate::wasm::{ExtEngine, WasmModule};

/// Largest component the loader reads.
const MAX_MODULE_BYTES: u64 = 32 * 1024 * 1024;
/// Most extensions one host loads.
pub const MAX_EXTENSIONS: usize = 64;
/// Commands queued for the worker before the next one is dropped.
const COMMAND_QUEUE: usize = 256;
/// Worker threads: a command that runs to its limit, or a rebuild, never queues everyone else
/// behind a single thread.
const WORKERS: usize = 2;
/// How often the worker looks for settings to write.
const KV_POLL: Duration = Duration::from_secs(1);

/// What loading one extension came to. A failure here never reaches the core.
#[derive(Debug)]
pub struct LoadReport {
    pub path: PathBuf,
    pub name: Option<String>,
    pub result: Result<(), String>,
}

/// Builds an [`ExtHost`].
#[derive(Debug)]
pub struct ExtHostBuilder {
    nodes: Arc<MirrorDirectory>,
    kv: Arc<SettingsKv>,
    engine: Option<Arc<ExtEngine>>,
    exts: Vec<Arc<LoadedExt>>,
    reports: Vec<LoadReport>,
    bind_errors: Vec<String>,
}

impl ExtHostBuilder {
    fn engine(&mut self) -> Result<Arc<ExtEngine>, String> {
        if let Some(e) = &self.engine {
            return Ok(Arc::clone(e));
        }
        let e = ExtEngine::new().map_err(|e| format!("cannot start wasmtime: {e:#}"))?;
        self.engine = Some(Arc::clone(&e));
        Ok(e)
    }

    /// Persist the settings KV in `path` (default: memory only).
    #[must_use]
    pub fn settings_file(mut self, path: PathBuf) -> Self {
        self.kv = Arc::new(SettingsKv::with_file(path));
        self
    }

    /// Add a WASM extension from its manifest text and component bytes.
    pub fn add_wasm(&mut self, manifest: Manifest, bytes: &[u8]) -> Result<(), String> {
        let engine = self.engine()?;
        let module =
            WasmModule::new(&engine, bytes).map_err(|e| format!("cannot load component: {e:#}"))?;
        self.push(manifest, Factory::Wasm(Arc::new(module)))
    }

    /// Add a native (linked-in) extension. The manifest must say `backend = "native"`.
    pub fn add_native(&mut self, manifest: Manifest, factory: NativeFactory) -> Result<(), String> {
        if manifest.backend != ExtBackend::Native {
            return Err(format!(
                "extension `{}`: a native factory needs `backend = \"native\"`",
                manifest.name
            ));
        }
        self.push(manifest, Factory::Native(factory))
    }

    fn push(&mut self, manifest: Manifest, factory: Factory) -> Result<(), String> {
        if self.exts.iter().any(|e| e.manifest.name == manifest.name) {
            return Err(format!("extension `{}` is already loaded", manifest.name));
        }
        if self.exts.len() >= MAX_EXTENSIONS {
            return Err(format!("more than {MAX_EXTENSIONS} extensions"));
        }
        self.exts.push(Arc::new(LoadedExt::new(manifest, factory)));
        Ok(())
    }

    /// Load every `<dir>/<name>/ext.toml` (and `<dir>/<name>.toml`). Failures are reported, not
    /// raised: one broken extension never stops the others or the core.
    pub fn load_dir(&mut self, dir: &Path) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            tracing::info!(dir = %dir.display(), "no extension directory");
            return;
        };
        let mut paths: Vec<PathBuf> = rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter_map(|p| {
                if p.is_dir() {
                    Some(p.join("ext.toml"))
                } else if p.extension().is_some_and(|e| e == "toml") {
                    Some(p)
                } else {
                    None
                }
            })
            .filter(|p| p.is_file())
            .collect();
        paths.sort();
        for path in paths {
            let report = self.load_one(&path);
            if let Err(e) = &report.result {
                tracing::warn!(path = %path.display(), "extension not loaded: {e}");
            }
            self.reports.push(report);
        }
    }

    fn load_one(&mut self, path: &Path) -> LoadReport {
        let mut report = LoadReport {
            path: path.to_owned(),
            name: None,
            result: Ok(()),
        };
        report.result = (|| {
            let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
            if meta.len() > MAX_MANIFEST_BYTES as u64 {
                return Err("manifest too large".to_owned());
            }
            let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
            let manifest = Manifest::parse(&text).map_err(|e| e.to_string())?;
            report.name = Some(manifest.name.clone());
            match manifest.backend {
                ExtBackend::Native => Err(
                    "native extensions are linked in by the daemon, not loaded from a directory"
                        .to_owned(),
                ),
                ExtBackend::Wasm => {
                    let module = manifest.module.clone().unwrap_or_default();
                    let mpath = path.parent().unwrap_or(Path::new(".")).join(module);
                    let len = std::fs::metadata(&mpath).map_err(|e| e.to_string())?.len();
                    if len > MAX_MODULE_BYTES {
                        return Err("component too large".to_owned());
                    }
                    let bytes = std::fs::read(&mpath).map_err(|e| e.to_string())?;
                    self.add_wasm(manifest, &bytes)
                }
            }
        })();
        report
    }

    pub fn reports(&self) -> &[LoadReport] {
        &self.reports
    }

    /// Finish: start the worker and return the host.
    pub fn build(self) -> ExtHost {
        let (tx, rx) = sync_channel(COMMAND_QUEUE);
        let sink: Arc<OnceLock<Arc<dyn ExtSink>>> = Arc::new(OnceLock::new());
        let stats = Arc::new(ExtStats::default());
        let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let rx = Arc::new(Mutex::new(rx));
        let workers = (0..WORKERS)
            .map(|_| {
                spawn_worker(
                    Arc::clone(&rx),
                    Arc::clone(&self.kv),
                    Arc::clone(&sink),
                    Arc::clone(&stopping),
                )
            })
            .collect();
        let manifests = self.exts.iter().map(|e| e.manifest.clone()).collect();
        ExtHost {
            exts: self.exts,
            manifests,
            nodes: self.nodes,
            sink,
            kv: self.kv,
            stats,
            apps: RwLock::new(HashMap::new()),
            tx,
            workers: Mutex::new(workers),
            stopping,
            _engine: self.engine,
            reports: self.reports,
            bind_errors: self.bind_errors,
        }
    }
}

fn spawn_worker(
    rx: Arc<Mutex<Receiver<Job>>>,
    kv: Arc<SettingsKv>,
    sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
) -> JoinHandle<()> {
    // A dedicated OS thread: commands may run for their 20 ms limit and must never occupy a
    // tokio worker or the Reader.
    #[allow(clippy::disallowed_methods)]
    std::thread::Builder::new()
        .name("gorca-ext-worker".to_owned())
        .spawn(move || {
            while !stopping.load(std::sync::atomic::Ordering::Relaxed) {
                let next = rx
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .recv_timeout(KV_POLL);
                match next {
                    Ok(Job::Rebuild { slot, old }) => slot.rebuild(old),
                    Ok(Job::Command { slot, local }) => {
                        let r = slot.call(HookKind::Command, true, |i| i.command(local));
                        slot.end_command();
                        if let Some(api::CommandResult::Speak(segs)) = r {
                            let text = gorca_core::segments_text(&segments_from_ext(segs));
                            if !text.is_empty()
                                && slot.allow_speech()
                                && let Some(s) = sink.get()
                            {
                                s.speak(text, true);
                            }
                        }
                    }
                    Ok(Job::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
                if kv.take_dirty()
                    && let Err(e) = kv.flush()
                {
                    tracing::warn!("cannot write the extension settings: {e}");
                }
            }
            let _ = kv.flush();
        })
        .unwrap_or_else(|e| {
            tracing::error!("cannot start the extension worker: {e}");
            std::thread::spawn(|| {})
        })
}

/// The extension host. Implements the core's [`ExtSource`].
pub struct ExtHost {
    exts: Vec<Arc<LoadedExt>>,
    manifests: Vec<Manifest>,
    nodes: Arc<MirrorDirectory>,
    sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
    kv: Arc<SettingsKv>,
    stats: Arc<ExtStats>,
    apps: RwLock<HashMap<AppId, Arc<AppExts>>>,
    tx: SyncSender<Job>,
    workers: Mutex<Vec<JoinHandle<()>>>,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    _engine: Option<Arc<ExtEngine>>,
    reports: Vec<LoadReport>,
    bind_errors: Vec<String>,
}

impl std::fmt::Debug for ExtHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtHost")
            .field("extensions", &self.exts.len())
            .finish_non_exhaustive()
    }
}

impl ExtHost {
    /// Start building a host that reads nodes from `nodes` (the core's mirror directory).
    pub fn builder(nodes: Arc<MirrorDirectory>) -> ExtHostBuilder {
        ExtHostBuilder {
            nodes,
            kv: Arc::new(SettingsKv::memory()),
            engine: None,
            exts: Vec::new(),
            reports: Vec::new(),
            bind_errors: Vec::new(),
        }
    }

    pub fn stats(&self) -> ExtStatsSnapshot {
        self.stats.snapshot()
    }

    /// Epoch ticks made by the backstop clock so far; it only runs while a WASM call is in
    /// flight, so an idle host does not advance it (tests).
    pub fn epoch_ticks(&self) -> u64 {
        self._engine.as_ref().map_or(0, |e| e.ticks_done())
    }

    pub fn reports(&self) -> &[LoadReport] {
        &self.reports
    }

    /// No slot is waiting for a replacement instance (tests and the debug probe).
    pub fn settled(&self) -> bool {
        self.read_apps()
            .values()
            .all(|a| a.slots.iter().all(|s| !s.is_rebuilding()))
    }

    /// The slots of one application (tests and the debug probe).
    pub fn slots_of(&self, app: AppId) -> Vec<Arc<Slot>> {
        self.read_apps()
            .get(&app)
            .map(|a| a.slots.clone())
            .unwrap_or_default()
    }

    pub fn extension_names(&self) -> impl Iterator<Item = &str> {
        self.manifests.iter().map(|m| m.name.as_str())
    }

    pub fn settings(&self) -> &Arc<SettingsKv> {
        &self.kv
    }

    pub fn bind_errors(&self) -> &[String] {
        &self.bind_errors
    }

    /// The extension commands and their default chords, to merge into the keymap compiler
    /// (`ExtCommands::install`). Registration problems (a bad default chord) are returned with
    /// the registry: the command still exists, only its default binding is lost.
    pub fn commands(&self) -> (ExtCommands, Vec<String>) {
        let mut reg = ExtCommands::new();
        let mut problems = self.bind_errors.clone();
        for m in &self.manifests {
            let prefix = m.name.replace('-', "_");
            for k in &m.keymap {
                let name = format!("ext.{prefix}.{}", k.command.replace('-', "_"));
                let spec = ExtCmdSpec {
                    name: name.clone(),
                    ext: m.name.clone(),
                    local: k.local_id,
                    help: k.help.clone(),
                    contexts: ContextMask::of(&k.contexts),
                    max_press: k.max_press,
                    interrupts: k.interrupts,
                };
                if let Err(e) = reg.register(spec) {
                    problems.push(format!("{}: {e}", m.name));
                    continue;
                }
                if let (Some(chord), Some(ctx)) = (&k.chord, k.contexts.first())
                    && let Err(e) = reg.bind(*ctx, chord, &name)
                {
                    problems.push(format!("{}: {e}", m.name));
                }
            }
        }
        (reg, problems)
    }

    fn read_apps(&self) -> std::sync::RwLockReadGuard<'_, HashMap<AppId, Arc<AppExts>>> {
        self.apps.read().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Drop for ExtHost {
    fn drop(&mut self) {
        let st = self.stats.snapshot();
        if st.busy_skips > 0 || st.faults > 0 || st.commands_dropped > 0 {
            tracing::info!(
                busy_skips = st.busy_skips,
                busy_suppressed = st.busy_suppressed,
                faults = st.faults,
                commands_dropped = st.commands_dropped,
                "extension host stopped"
            );
        }
        // The apps hold clones of the sender, so the queue cannot close by itself: ask the worker
        // to stop. It flushes the settings on the way out.
        self.stopping
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..WORKERS {
            let _ = self.tx.try_send(Job::Stop);
        }
        let handles =
            std::mem::take(&mut *self.workers.lock().unwrap_or_else(PoisonError::into_inner));
        for h in handles {
            let _ = h.join();
        }
    }
}

impl ExtSource for ExtHost {
    fn attach(&self, app: AppId, info: &AppInfo) -> Option<Arc<dyn AppExt>> {
        let matching = order_matching(&self.manifests, info, None);
        let previous = self.read_apps().get(&app).cloned();
        let mut slots = Vec::new();
        for m in matching {
            let Some(idx) = self.manifests.iter().position(|x| std::ptr::eq(x, m)) else {
                continue;
            };
            let ext = Arc::clone(&self.exts[idx]);
            // A re-announced application keeps its slot, and with it its strikes: re-attaching
            // must not be a way to reset the fault accounting.
            if let Some(old) = &previous
                && let Some(s) = old.slots.iter().find(|s| s.name() == m.name)
            {
                slots.push(Arc::clone(s));
                continue;
            }
            if ext.is_disabled() {
                self.stats
                    .attach_skipped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                continue;
            }
            let shared = Arc::new(InstShared::new(
                Arc::from(m.name.as_str()),
                app,
                m.capabilities,
                Arc::clone(&self.nodes),
                Arc::clone(&self.sink),
                Arc::clone(&self.kv),
            ));
            let label: Arc<str> = Arc::from(&*info.app_name);
            match Slot::new(
                ext,
                shared,
                label,
                Arc::clone(&self.stats),
                Arc::clone(&self.sink),
                self.tx.clone(),
            ) {
                Ok(s) => slots.push(s),
                Err(e) => tracing::warn!(ext = %m.name, "not attached: {e}"),
            }
        }
        if slots.is_empty() {
            self.apps
                .write()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&app);
            return None;
        }
        self.stats
            .attached
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let ae = Arc::new(AppExts {
            slots,
            tx: self.tx.clone(),
            stats: Arc::clone(&self.stats),
        });
        self.apps
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(app, Arc::clone(&ae));
        Some(ae)
    }

    fn detach(&self, app: AppId) {
        self.apps
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&app);
    }

    fn for_app(&self, app: AppId) -> Option<Arc<dyn AppExt>> {
        let g = self.read_apps();
        let ae = g.get(&app)?;
        Some(Arc::clone(ae) as Arc<dyn AppExt>)
    }

    fn set_sink(&self, sink: Arc<dyn ExtSink>) {
        let _ = self.sink.set(sink);
    }
}

/// The extensions that matched one application.
pub struct AppExts {
    slots: Vec<Arc<Slot>>,
    tx: SyncSender<Job>,
    stats: Arc<ExtStats>,
}

impl std::fmt::Debug for AppExts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppExts")
            .field("slots", &self.slots)
            .finish_non_exhaustive()
    }
}

impl AppExts {
    /// The slots, in manifest order (tests and the debug probe).
    pub fn slots(&self) -> &[Arc<Slot>] {
        &self.slots
    }
}

fn present_reason(ctx: PresentContext) -> api::PresentReason {
    match ctx {
        PresentContext::Focus | PresentContext::SelectedItem => api::PresentReason::Focus,
    }
}

/// Whether a busy slot may repeat its last decision for events of `kind`. Only the high-rate
/// kinds (properties, children, text, caret, selection) qualify: one decision stands for a
/// burst of near-identical events. Focus, window, announcement and removal events are discrete
/// and unrelated to the previous one, so a repeated suppress would silence an unrelated change
/// (or turn a rewrite into silence); they pass through to the core's own speech.
pub(crate) fn repeats_when_busy(kind: usize) -> bool {
    matches!(kind, 1..=5)
}

/// Index of the event's kind, for the slot's per-kind last decision.
fn event_kind(e: &api::Event) -> usize {
    match e {
        api::Event::FocusChanged(_) => 0,
        api::Event::PropertiesChanged(..) => 1,
        api::Event::ChildrenChanged(_) => 2,
        api::Event::TextChanged(_) => 3,
        api::Event::CaretMoved(..) => 4,
        api::Event::SelectionChanged(_) => 5,
        api::Event::Announcement(_) => 6,
        api::Event::WindowActivated(_) => 7,
        api::Event::NodeRemoved(_) => 8,
    }
}

impl AppExt for AppExts {
    fn on_event(&self, ev: &NodeEvent) -> ExtVerdict {
        let Some(e) = event_to_ext(ev) else {
            return ExtVerdict::Pass;
        };
        let mut verdict = ExtVerdict::Pass;
        for slot in &self.slots {
            if !slot.manifest().hooks.contains(HookSet::ON_EVENT) {
                continue;
            }
            let kind = event_kind(&e);
            let v = match slot.call_outcome(HookKind::OnEvent, false, |i| i.on_event(&e)) {
                Outcome::Done(api::EventVerdict::Suppress) => {
                    slot.record_decision(kind, Decision::Consume);
                    ExtVerdict::Suppress
                }
                Outcome::Done(api::EventVerdict::Rewrite(s)) => {
                    slot.record_decision(kind, Decision::Consume);
                    // Over its speech budget the rewrite still consumes the event, silently.
                    if slot.allow_speech() {
                        ExtVerdict::Rewrite(segments_from_ext(s))
                    } else {
                        ExtVerdict::Suppress
                    }
                }
                Outcome::Done(api::EventVerdict::ReEvaluate) => ExtVerdict::ReEvaluate,
                Outcome::Done(api::EventVerdict::Pass) => {
                    slot.record_decision(kind, Decision::Pass);
                    ExtVerdict::Pass
                }
                Outcome::Busy => {
                    // See `Decision`: repeat the last decision, for high-rate kinds only.
                    if repeats_when_busy(kind) && slot.last_decision(kind) == Decision::Consume {
                        self.stats
                            .busy_suppressed
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        ExtVerdict::Suppress
                    } else {
                        ExtVerdict::Pass
                    }
                }
                Outcome::Unavailable => ExtVerdict::Pass,
            };
            verdict = verdict.then(v);
            if verdict != ExtVerdict::Pass {
                break;
            }
        }
        verdict
    }

    fn present(
        &self,
        snap: &NodeSnapshot,
        ctx: PresentContext,
        epoch: u64,
    ) -> Option<Vec<Segment>> {
        let mut node = None;
        for slot in &self.slots {
            if !slot.manifest().hooks.contains(HookSet::PRESENT) {
                continue;
            }
            let n = node.get_or_insert_with(|| node_to_ext(snap));
            let pctx = api::PresentCtx {
                epoch,
                reason: present_reason(ctx),
            };
            // Never block the Reader: a busy slot is "no opinion".
            if let Some(Some(segs)) = slot.call(HookKind::Present, false, |i| i.present(n, pctx)) {
                let segs = segments_from_ext(segs);
                if !segs.is_empty() {
                    return Some(segs);
                }
            }
        }
        None
    }

    fn command(&self, ext: &str, local: u32) {
        let Some(slot) = self
            .slots
            .iter()
            .find(|s| s.name() == ext && s.manifest().hooks.contains(HookSet::COMMAND))
        else {
            return;
        };
        if !slot.begin_command() {
            self.stats
                .commands_dropped
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        match self.tx.try_send(Job::Command {
            slot: Arc::clone(slot),
            local,
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                slot.end_command();
                self.stats
                    .commands_dropped
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
}

#[cfg(test)]
mod busy_tests {
    use super::{event_kind, repeats_when_busy};
    use gorca_ext_api as api;

    #[test]
    fn only_high_rate_kinds_repeat_a_decision_while_busy() {
        let k = (1, 1);
        for (e, repeats) in [
            (api::Event::FocusChanged(k), false),
            (api::Event::PropertiesChanged(k, 0), true),
            (api::Event::ChildrenChanged(k), true),
            (api::Event::TextChanged(k), true),
            (api::Event::CaretMoved(k, 0), true),
            (api::Event::SelectionChanged(k), true),
            (api::Event::Announcement(String::new()), false),
            (api::Event::WindowActivated(k), false),
            (api::Event::NodeRemoved(k), false),
        ] {
            assert_eq!(repeats_when_busy(event_kind(&e)), repeats, "{e:?}");
        }
    }
}
