//! What every instance of an extension has in common: its shared host context, the host functions
//! (written once over the API types, used by both backends), faults and the hook trait the slot
//! drives.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gorca_core::{ExtSink, MirrorDirectory};
use gorca_ext_api as api;
use gorca_ext_api::{LIMIT_COMMAND_MS, LIMIT_HOOK_MS};
use gorca_model::{AppId, Lookup};

use crate::convert::{key_from_ext, node_to_ext};
use crate::kv::SettingsKv;
use crate::manifest::Capabilities;

/// The hooks the slot drives, for choosing a CPU limit.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HookKind {
    OnEvent,
    Present,
    Policy,
    NavUnits,
    NavMatch,
    Command,
}

impl HookKind {
    /// CPU limit of the hook (ADR-0001 section 10, WE-5).
    pub fn limit(self) -> Duration {
        match self {
            HookKind::OnEvent
            | HookKind::Present
            | HookKind::Policy
            | HookKind::NavUnits
            | HookKind::NavMatch => Duration::from_millis(u64::from(LIMIT_HOOK_MS)),
            HookKind::Command => Duration::from_millis(u64::from(LIMIT_COMMAND_MS)),
        }
    }
}

/// Why an instance was reset (ADR-0008 section 3.3: a trap, a panic, an epoch/fuel interrupt, or
/// a native call over its CPU limit).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fault {
    /// Interrupted by the epoch deadline (a loop or an overlong call).
    Timeout,
    /// Hit the 32 MiB cap.
    Memory,
    /// Any other trap (`unreachable` from a panic, bad lifting, a trapped import).
    Trap(String),
    /// A native hook panicked.
    Panic(String),
    /// A native hook returned after its limit (it cannot be pre-empted, only detected).
    Overrun(Duration),
}

impl fmt::Display for Fault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Fault::Timeout => f.write_str("interrupted at the CPU limit"),
            Fault::Memory => f.write_str("memory limit exceeded"),
            Fault::Trap(m) => write!(f, "trap: {m}"),
            Fault::Panic(m) => write!(f, "panic: {m}"),
            Fault::Overrun(d) => write!(f, "native hook ran {d:?}, over its limit"),
        }
    }
}

/// One instance of an extension for one application.
pub trait HookInstance: Send {
    fn on_event(&mut self, ev: &api::Event) -> Result<api::EventVerdict, Fault>;
    fn present(
        &mut self,
        n: &api::Node,
        ctx: api::PresentCtx,
    ) -> Result<Option<Vec<api::Segment>>, Fault>;
    fn policy(&mut self, f: &api::PolicyFacts) -> Result<api::PolicyVerdict, Fault>;
    fn nav_units(&mut self) -> Result<Vec<api::NavUnit>, Fault>;
    fn nav_match(&mut self, unit: u32, n: &api::Node) -> Result<bool, Fault>;
    fn command(&mut self, id: u32) -> Result<api::CommandResult, Fault>;
}

/// Counters, readable from any thread.
#[derive(Debug, Default)]
pub struct ExtStats {
    pub attached: AtomicU64,
    pub hook_calls: AtomicU64,
    pub faults: AtomicU64,
    pub resets: AtomicU64,
    pub disabled: AtomicU64,
    /// Non-blocking hook calls (`present`, `on_event`) that found their slot busy (a command or
    /// a rebuild held it) and did not run.
    pub busy_skips: AtomicU64,
    /// Of those, events consumed anyway because the extension's last decision for that kind of
    /// event was suppress or rewrite (a busy slot never leaks the core's duplicate speech).
    pub busy_suppressed: AtomicU64,
    /// Slowest reset (detection excluded): fault to a working instance, in nanoseconds.
    pub max_reset_ns: AtomicU64,
    /// Commands dropped because the worker queue was full.
    pub commands_dropped: AtomicU64,
    /// Attaches refused because the extension is disabled as a whole.
    pub attach_skipped: AtomicU64,
    /// Instances that could not be built (attach or reset).
    pub instantiate_failures: AtomicU64,
    /// Replacement instances built by the workers after a fault.
    pub rebuilds: AtomicU64,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ExtStatsSnapshot {
    pub attached: u64,
    pub hook_calls: u64,
    pub faults: u64,
    pub resets: u64,
    pub disabled: u64,
    pub busy_skips: u64,
    pub busy_suppressed: u64,
    pub max_reset_ns: u64,
    pub commands_dropped: u64,
    pub attach_skipped: u64,
    pub instantiate_failures: u64,
    pub rebuilds: u64,
}

impl ExtStats {
    pub fn snapshot(&self) -> ExtStatsSnapshot {
        let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
        ExtStatsSnapshot {
            attached: g(&self.attached),
            hook_calls: g(&self.hook_calls),
            faults: g(&self.faults),
            resets: g(&self.resets),
            disabled: g(&self.disabled),
            busy_skips: g(&self.busy_skips),
            busy_suppressed: g(&self.busy_suppressed),
            max_reset_ns: g(&self.max_reset_ns),
            commands_dropped: g(&self.commands_dropped),
            attach_skipped: g(&self.attach_skipped),
            instantiate_failures: g(&self.instantiate_failures),
            rebuilds: g(&self.rebuilds),
        }
    }
}

/// Per hook call budgets of the host functions: a loop of host calls is bounded by fuel, but each
/// call costs the host more than a guest instruction, so each is capped on its own.
const MAX_SPEAKS_PER_CALL: u32 = 8;
const MAX_LOGS_PER_CALL: u32 = 8;
const MAX_SETS_PER_CALL: u32 = 16;
const MAX_NODE_READS_PER_CALL: u32 = 256;
const MAX_LOG_CHARS: usize = 512;
/// Spoken messages an (extension, application) may emit per second, burst included. A hook that
/// speaks on every event (a storm) would otherwise queue speech without bound.
const SPEECH_BURST: f64 = 10.0;
const SPEECH_PER_SEC: f64 = 10.0;

/// Everything a host function needs: shared by the store (or the native thread-local host) of one
/// (extension, application).
pub struct InstShared {
    pub ext: Arc<str>,
    pub app: AppId,
    pub caps: Capabilities,
    pub nodes: Arc<MirrorDirectory>,
    pub sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
    pub kv: Arc<SettingsKv>,
    speaks: std::sync::atomic::AtomicU32,
    logs: std::sync::atomic::AtomicU32,
    sets: std::sync::atomic::AtomicU32,
    reads: std::sync::atomic::AtomicU32,
    bucket: std::sync::Mutex<(f64, std::time::Instant)>,
}

impl fmt::Debug for InstShared {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstShared")
            .field("ext", &self.ext)
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

impl InstShared {
    pub fn new(
        ext: Arc<str>,
        app: AppId,
        caps: Capabilities,
        nodes: Arc<MirrorDirectory>,
        sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
        kv: Arc<SettingsKv>,
    ) -> Self {
        Self {
            ext,
            app,
            caps,
            nodes,
            sink,
            kv,
            speaks: std::sync::atomic::AtomicU32::new(0),
            logs: std::sync::atomic::AtomicU32::new(0),
            sets: std::sync::atomic::AtomicU32::new(0),
            reads: std::sync::atomic::AtomicU32::new(0),
            bucket: std::sync::Mutex::new((SPEECH_BURST, std::time::Instant::now())),
        }
    }

    /// Called before each hook call: the per-call budgets start over.
    pub fn begin_call(&self) {
        self.speaks.store(0, Ordering::Relaxed);
        self.logs.store(0, Ordering::Relaxed);
        self.sets.store(0, Ordering::Relaxed);
        self.reads.store(0, Ordering::Relaxed);
    }

    /// Token bucket for everything this extension makes the screen reader say, whatever the
    /// path (`speak`, a rewrite verdict, a command answer). False: drop it.
    pub fn allow_speech(&self) -> bool {
        let mut b = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = std::time::Instant::now();
        let dt = now.saturating_duration_since(b.1).as_secs_f64();
        b.0 = (b.0 + dt * SPEECH_PER_SEC).min(SPEECH_BURST);
        b.1 = now;
        if b.0 >= 1.0 {
            b.0 -= 1.0;
            true
        } else {
            false
        }
    }

    /// Mirror only, never blocks. Another application's nodes are invisible.
    pub fn get_node(&self, key: api::NodeKey) -> Option<api::Node> {
        if self.reads.fetch_add(1, Ordering::Relaxed) >= MAX_NODE_READS_PER_CALL {
            return None;
        }
        let k = key_from_ext(key)?;
        if k.app != self.app {
            return None;
        }
        match self.nodes.lookup(k) {
            Lookup::Hit(s) => Some(node_to_ext(&s)),
            Lookup::Miss | Lookup::Gone => None,
        }
    }

    /// `fetch` and `text` need the connector's request path, which a synchronous hook cannot
    /// use without blocking (M6 adds an asynchronous prefetch path for `ext-vte`). Declared
    /// capability: not supported yet; undeclared: denied.
    pub fn fetch_error(&self) -> api::QueryError {
        if self
            .caps
            .intersects(Capabilities::FETCH_PREFETCH | Capabilities::FETCH_BACKGROUND)
        {
            api::QueryError::NotSupported
        } else {
            api::QueryError::Denied
        }
    }

    pub fn speak(&self, segments: &[api::Segment], prio: api::Prio) {
        if !self.caps.contains(Capabilities::SPEAK) {
            return;
        }
        if self.speaks.fetch_add(1, Ordering::Relaxed) >= MAX_SPEAKS_PER_CALL {
            return;
        }
        // The WASM backend refuses oversize lifts before copying (hostcall fuel); the clip here
        // bounds what a native extension can pass.
        let segs = crate::convert::segments_from_iter(segments.iter().take(64).cloned());
        let text = gorca_core::segments_text(&segs);
        if text.is_empty() || !self.allow_speech() {
            return;
        }
        if let Some(sink) = self.sink.get() {
            sink.speak(text, prio == api::Prio::Interrupt);
        }
    }

    pub fn get_setting(&self, k: &str) -> Option<String> {
        if !self.caps.contains(Capabilities::SETTINGS) {
            return None;
        }
        self.kv.get(&self.ext, k)
    }

    pub fn set_setting(&self, k: &str, v: &str) {
        if self.caps.contains(Capabilities::SETTINGS)
            && self.sets.fetch_add(1, Ordering::Relaxed) < MAX_SETS_PER_CALL
        {
            self.kv.set(&self.ext, k, v);
        }
    }

    pub fn log(&self, level: u8, msg: &str) {
        if self.logs.fetch_add(1, Ordering::Relaxed) >= MAX_LOGS_PER_CALL {
            return;
        }
        let msg: String = msg.chars().take(MAX_LOG_CHARS).collect();
        let ext = &*self.ext;
        match level {
            0 => tracing::error!(ext, "{msg}"),
            1 => tracing::warn!(ext, "{msg}"),
            2 => tracing::info!(ext, "{msg}"),
            _ => tracing::debug!(ext, "{msg}"),
        }
    }
}

/// [`InstShared`] as the native build's `Host`.
pub struct NativeHost(pub Arc<InstShared>);

impl api::Host for NativeHost {
    fn get_node(&self, key: api::NodeKey) -> Option<api::Node> {
        self.0.get_node(key)
    }
    fn fetch(
        &self,
        _key: api::NodeKey,
        _mask: u32,
        _b: api::Budget,
    ) -> Result<api::Node, api::QueryError> {
        Err(self.0.fetch_error())
    }
    fn text(
        &self,
        _key: api::NodeKey,
        _start: u32,
        _end: u32,
        _b: api::Budget,
    ) -> Result<String, api::QueryError> {
        Err(self.0.fetch_error())
    }
    fn speak(&self, segments: Vec<api::Segment>, prio: api::Prio) {
        self.0.speak(&segments, prio);
    }
    fn get_setting(&self, k: &str) -> Option<String> {
        self.0.get_setting(k)
    }
    fn set_setting(&self, k: &str, v: &str) {
        self.0.set_setting(k, v);
    }
    fn log(&self, level: u8, msg: &str) {
        self.0.log(level, msg);
    }
}
