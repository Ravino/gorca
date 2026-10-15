//! One extension for one application: the instance, its health and the fault policy.
//!
//! Every fault (trap, panic, epoch interrupt, native overrun) drops the instance and builds a
//! fresh one; the third reset within 60 seconds disables the slot for good, with one spoken
//! notice (ADR-0001 section 10, ADR-0008 section 3.3). A slot never lets a fault out: callers see
//! `None`, which is the same as "this extension has no opinion".

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError, Weak};
use std::time::Instant;

use gorca_core::ExtSink;

use crate::health::{InstanceGuard, InstanceHealth};
use crate::instance::{ExtStats, Fault, HookInstance, HookKind, InstShared};
use crate::manifest::Manifest;
use crate::native::{NativeFactory, NativeInstance};
use crate::wasm::WasmModule;

/// How a loaded extension makes instances.
pub enum Factory {
    Wasm(Arc<WasmModule>),
    Native(NativeFactory),
}

impl std::fmt::Debug for Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Factory::Wasm(_) => f.write_str("Factory::Wasm"),
            Factory::Native(_) => f.write_str("Factory::Native"),
        }
    }
}

/// A loaded extension: manifest, instance factory and the extension-wide health.
///
/// Per-slot health (`InstanceGuard`) stops one application's faults. This guard is the second
/// line: three *strikes* within 60 s, where a strike is an instance that could not be built or a
/// slot that was disabled, disable the extension for every new application, so a hostile
/// extension or a hostile stream of applications cannot make the host rebuild it forever.
#[derive(Debug)]
pub struct LoadedExt {
    pub manifest: Manifest,
    pub factory: Factory,
    guard: Mutex<InstanceGuard>,
    disabled: AtomicBool,
}

impl LoadedExt {
    pub fn new(manifest: Manifest, factory: Factory) -> Self {
        Self {
            manifest,
            factory,
            guard: Mutex::new(InstanceGuard::new()),
            disabled: AtomicBool::new(false),
        }
    }

    /// Disabled as a whole: no new application gets an instance.
    pub fn is_disabled(&self) -> bool {
        self.disabled.load(Ordering::Acquire)
    }

    /// Record a strike (see the type documentation). Returns true when this one disabled the
    /// extension; the user is told once.
    pub fn strike(&self, sink: &OnceLock<Arc<dyn ExtSink>>) -> bool {
        let mut g = self.guard.lock().unwrap_or_else(PoisonError::into_inner);
        if g.on_reset(Instant::now()) != InstanceHealth::Disabled {
            return false;
        }
        drop(g);
        if self.disabled.swap(true, Ordering::AcqRel) {
            return false;
        }
        tracing::error!(
            ext = %self.manifest.name,
            "extension disabled for every application after repeated failures"
        );
        if let Some(s) = sink.get() {
            s.notice(format!(
                "Extension {} disabled",
                self.manifest.name.replace(['-', '_'], " ")
            ));
        }
        true
    }

    pub fn create(&self, shared: &Arc<InstShared>) -> Result<Box<dyn HookInstance>, String> {
        match &self.factory {
            Factory::Wasm(m) => m
                .instantiate(Arc::clone(shared))
                .map(|i| Box::new(i) as Box<dyn HookInstance>)
                .map_err(|e| format!("{e:#}")),
            Factory::Native(f) => Ok(Box::new(NativeInstance::new(f, Arc::clone(shared)))),
        }
    }
}

/// Work for the host's worker threads. Everything slow lives here, never on a caller's thread.
pub(crate) enum Job {
    /// Run `command(local)` of the slot's extension.
    Command {
        slot: Arc<Slot>,
        local: u32,
    },
    /// Drop a faulted instance and build its replacement.
    Rebuild {
        slot: Arc<Slot>,
        old: Option<Box<dyn HookInstance>>,
    },
    Stop,
}

/// Event kinds the slot remembers a decision for (see `host::event_kind`).
pub(crate) const EVENT_KINDS: usize = 9;
/// The busy warning is written at most this often per slot.
const BUSY_LOG_EVERY_MS: u64 = 10_000;

/// Milliseconds since the first call in the process (never 0).
fn busy_clock_ms() -> u64 {
    static T0: OnceLock<Instant> = OnceLock::new();
    let t0 = T0.get_or_init(Instant::now);
    u64::try_from(t0.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .saturating_add(1)
}

/// What a non-blocking call found.
pub(crate) enum Outcome<R> {
    Done(R),
    /// The slot's lock was held (a command or a rebuild is running).
    Busy,
    /// Disabled, being rebuilt, or the call faulted.
    Unavailable,
}

/// What the extension last did with an event of one kind.
///
/// While a slot is busy its hook cannot run, and falling back to the core's own speech would
/// say what the extension has been replacing (double speech next to the extension's own
/// output). So a busy slot repeats its last decision for that kind of event: it stays silent
/// where the extension had suppressed or rewritten (the rewrite text is not reproducible), and
/// passes where the extension passed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Decision {
    Pass = 0,
    Consume = 1,
}

impl Decision {
    fn from_u8(v: u8) -> Self {
        if v == Decision::Consume as u8 {
            Decision::Consume
        } else {
            Decision::Pass
        }
    }
}

/// Commands one slot may have queued or running; more are dropped (a command is a user key press,
/// four in flight is already a user mashing a key).
const MAX_PENDING_COMMANDS: u32 = 4;

struct SlotInner {
    inst: Option<Box<dyn HookInstance>>,
    guard: InstanceGuard,
    /// When the pending rebuild began (for the reset-time statistic).
    faulted_at: Option<Instant>,
}

/// One extension for one application.
///
/// # Nothing here can delay the core
///
/// - Callers on hot paths (`present` in the Reader, `on_event` in the app actor) only `try_lock`:
///   a slot that is busy answers "no opinion" at once.
/// - A call is bounded by fuel (CPU) and the epoch backstop (wall time).
/// - A fault never rebuilds on the caller's thread: the dead instance is dropped and its
///   replacement built by a worker. Until it is ready the slot answers "no opinion".
/// - Commands are queued to the workers, at most four per slot, over a bounded queue.
pub struct Slot {
    ext: Arc<LoadedExt>,
    shared: Arc<InstShared>,
    app_label: Arc<str>,
    inner: Mutex<SlotInner>,
    disabled: AtomicBool,
    rebuilding: AtomicBool,
    pending_commands: AtomicU32,
    /// Last decision per event kind (a verdict class), used when the slot is busy.
    decisions: [AtomicU8; EVENT_KINDS],
    /// When the busy log line was last written, in ms of [`busy_clock_ms`] (0: never).
    busy_logged_ms: AtomicU64,
    me: Weak<Slot>,
    jobs: SyncSender<Job>,
    stats: Arc<ExtStats>,
    sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("ext", &self.ext.manifest.name)
            .field("app", &self.app_label)
            .field("disabled", &self.disabled.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Slot {
    /// Instantiate `ext` for one application. `Err` when the instance cannot be built (the pool
    /// is full, the component traps in its start function): the caller skips the slot. Runs on the
    /// app actor at attach, which is the one place a build happens off the workers.
    pub(crate) fn new(
        ext: Arc<LoadedExt>,
        shared: Arc<InstShared>,
        app_label: Arc<str>,
        stats: Arc<ExtStats>,
        sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
        jobs: SyncSender<Job>,
    ) -> Result<Arc<Self>, String> {
        let inst = match ext.create(&shared) {
            Ok(i) => i,
            Err(e) => {
                stats.instantiate_failures.fetch_add(1, Ordering::Relaxed);
                ext.strike(&sink);
                return Err(e);
            }
        };
        Ok(Arc::new_cyclic(|me| Self {
            ext,
            shared,
            app_label,
            inner: Mutex::new(SlotInner {
                inst: Some(inst),
                guard: InstanceGuard::new(),
                faulted_at: None,
            }),
            disabled: AtomicBool::new(false),
            rebuilding: AtomicBool::new(false),
            pending_commands: AtomicU32::new(0),
            decisions: std::array::from_fn(|_| AtomicU8::new(Decision::Pass as u8)),
            busy_logged_ms: AtomicU64::new(0),
            me: me.clone(),
            jobs,
            stats,
            sink,
        }))
    }

    /// Speech budget of this slot (see `InstShared::allow_speech`).
    pub fn allow_speech(&self) -> bool {
        self.shared.allow_speech()
    }

    pub fn name(&self) -> &str {
        &self.ext.manifest.name
    }

    pub fn manifest(&self) -> &Manifest {
        &self.ext.manifest
    }

    pub fn health(&self) -> InstanceHealth {
        if self.disabled.load(Ordering::Acquire) || self.ext.is_disabled() {
            InstanceHealth::Disabled
        } else {
            InstanceHealth::Active
        }
    }

    /// A replacement instance is being built (the slot answers "no opinion" meanwhile).
    pub fn is_rebuilding(&self) -> bool {
        self.rebuilding.load(Ordering::Acquire)
    }

    /// Claim a command slot; false: too many are queued for this extension, drop the press.
    pub(crate) fn begin_command(&self) -> bool {
        let mut cur = self.pending_commands.load(Ordering::Relaxed);
        loop {
            if cur >= MAX_PENDING_COMMANDS {
                return false;
            }
            match self.pending_commands.compare_exchange_weak(
                cur,
                cur + 1,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(now) => cur = now,
            }
        }
    }

    pub(crate) fn end_command(&self) {
        self.pending_commands.fetch_sub(1, Ordering::AcqRel);
    }

    /// Hold the slot's lock, as a running command does (tests).
    #[doc(hidden)]
    pub fn hold_for_test(&self) -> MutexGuard<'_, impl Sized> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn lock(&self, blocking: bool) -> Option<MutexGuard<'_, SlotInner>> {
        if blocking {
            return Some(self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        }
        match self.inner.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                self.note_busy();
                None
            }
        }
    }

    /// A non-blocking call found the slot busy (a command or a rebuild holds it). Counted, and
    /// logged at most once per [`BUSY_LOG_EVERY`] per slot so a storm cannot flood the log.
    fn note_busy(&self) {
        let total = self.stats.busy_skips.fetch_add(1, Ordering::Relaxed) + 1;
        let now = busy_clock_ms();
        let last = self.busy_logged_ms.load(Ordering::Relaxed);
        if (last == 0 || now.saturating_sub(last) >= BUSY_LOG_EVERY_MS)
            && self
                .busy_logged_ms
                .compare_exchange(last, now.max(1), Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
        {
            tracing::warn!(
                ext = self.name(),
                app = &*self.app_label,
                busy_skips_total = total,
                "extension slot busy: hook calls skipped while a command or rebuild holds it"
            );
        }
    }

    /// The last decision the extension made for events of `kind` (see [`Decision`]).
    pub(crate) fn last_decision(&self, kind: usize) -> Decision {
        self.decisions.get(kind).map_or(Decision::Pass, |d| {
            Decision::from_u8(d.load(Ordering::Relaxed))
        })
    }

    pub(crate) fn record_decision(&self, kind: usize, d: Decision) {
        if let Some(a) = self.decisions.get(kind) {
            a.store(d as u8, Ordering::Relaxed);
        }
    }

    /// Run `f` on the instance. `None` when the slot is disabled, busy (non-blocking calls only),
    /// being rebuilt, or the call faulted. Only the worker threads pass `blocking = true`.
    pub fn call<R>(
        &self,
        kind: HookKind,
        blocking: bool,
        f: impl FnOnce(&mut dyn HookInstance) -> Result<R, Fault>,
    ) -> Option<R> {
        match self.call_outcome(kind, blocking, f) {
            Outcome::Done(v) => Some(v),
            Outcome::Busy | Outcome::Unavailable => None,
        }
    }

    /// [`call`](Self::call), telling a busy slot from one that has no answer to give.
    pub(crate) fn call_outcome<R>(
        &self,
        kind: HookKind,
        blocking: bool,
        f: impl FnOnce(&mut dyn HookInstance) -> Result<R, Fault>,
    ) -> Outcome<R> {
        // An extension disabled as a whole stops in every application it is attached to, at once:
        // slots that were already attached do not get a fresh allowance of faults.
        if self.disabled.load(Ordering::Acquire) || self.ext.is_disabled() {
            return Outcome::Unavailable;
        }
        let Some(mut inner) = self.lock(blocking) else {
            return Outcome::Busy;
        };
        if inner.inst.is_none() {
            if blocking {
                // A worker thread: building here is allowed (and is what the pending rebuild
                // job would do next).
                if !self.ensure_instance(&mut inner) {
                    return Outcome::Unavailable;
                }
            } else {
                drop(inner);
                // A rebuild that could not be queued (the queue was full) is asked for again.
                if !self.rebuilding.load(Ordering::Acquire) {
                    self.schedule_rebuild(None, Instant::now());
                }
                return Outcome::Unavailable;
            }
        }
        let Some(inst) = inner.inst.as_mut() else {
            return Outcome::Unavailable;
        };
        self.stats.hook_calls.fetch_add(1, Ordering::Relaxed);
        match f(inst.as_mut()) {
            Ok(v) => Outcome::Done(v),
            Err(fault) => {
                self.on_fault(&mut inner, &fault, kind);
                Outcome::Unavailable
            }
        }
    }

    /// Account for a fault and hand the rebuild to a worker. Cheap and bounded: no instantiation
    /// and no pool operation happen here beyond moving the dead instance into the job.
    fn on_fault(&self, inner: &mut SlotInner, fault: &Fault, kind: HookKind) {
        let t0 = Instant::now();
        self.stats.faults.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            ext = self.name(),
            app = &*self.app_label,
            ?kind,
            "extension fault: {fault}; the instance is reset"
        );
        let old = inner.inst.take();
        self.stats.resets.fetch_add(1, Ordering::Relaxed);
        if inner.guard.on_reset(t0) == InstanceHealth::Disabled {
            // Dropped on a worker too: tearing down a store is not for the caller's thread.
            self.schedule_drop(old);
            self.disable();
            return;
        }
        inner.faulted_at = Some(t0);
        self.schedule_rebuild(old, t0);
    }

    fn schedule_drop(&self, old: Option<Box<dyn HookInstance>>) {
        if let (Some(old), Some(me)) = (old, self.me.upgrade()) {
            // Rebuild job on a disabled slot only drops `old`.
            let _ = self.jobs.try_send(Job::Rebuild {
                slot: me,
                old: Some(old),
            });
        }
    }

    fn schedule_rebuild(&self, old: Option<Box<dyn HookInstance>>, at: Instant) {
        if self.rebuilding.swap(true, Ordering::AcqRel) {
            return;
        }
        let Some(me) = self.me.upgrade() else {
            self.rebuilding.store(false, Ordering::Release);
            return;
        };
        if self.jobs.try_send(Job::Rebuild { slot: me, old }).is_err() {
            // Queue full or workers gone: leave the slot empty; the next call asks again.
            self.rebuilding.store(false, Ordering::Release);
            let _ = at;
        }
    }

    /// Worker side of [`Job::Rebuild`]: drop the dead instance, build the next one.
    pub(crate) fn rebuild(&self, old: Option<Box<dyn HookInstance>>) {
        drop(old);
        if !self.disabled.load(Ordering::Acquire) {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            self.ensure_instance(&mut inner);
        }
        self.rebuilding.store(false, Ordering::Release);
    }

    /// Build the instance if there is none. Worker threads only. False: the slot cannot run
    /// (disabled, or the build failed and that disabled it).
    fn ensure_instance(&self, inner: &mut SlotInner) -> bool {
        if self.disabled.load(Ordering::Acquire) {
            return false;
        }
        if inner.inst.is_some() {
            return true;
        }
        match self.ext.create(&self.shared) {
            Ok(i) => {
                inner.inst = Some(i);
                self.stats.rebuilds.fetch_add(1, Ordering::Relaxed);
                if let Some(t0) = inner.faulted_at.take() {
                    let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
                    self.stats.max_reset_ns.fetch_max(ns, Ordering::Relaxed);
                }
                true
            }
            Err(e) => {
                // An instance that cannot be rebuilt ends the slot at once (stricter than three
                // strikes) and counts against the extension as a whole.
                tracing::error!(ext = self.name(), "cannot rebuild the instance: {e}");
                self.stats
                    .instantiate_failures
                    .fetch_add(1, Ordering::Relaxed);
                self.disable();
                false
            }
        }
    }

    fn disable(&self) {
        if self.disabled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stats.disabled.fetch_add(1, Ordering::Relaxed);
        self.ext.strike(&self.sink);
        tracing::error!(
            ext = self.name(),
            app = &*self.app_label,
            "extension disabled for this application after repeated faults"
        );
        // The user is told once.
        if let Some(sink) = self.sink.get() {
            sink.notice(format!(
                "Extension {} disabled for {}",
                self.name().replace(['-', '_'], " "),
                self.app_label
            ));
        }
    }
}
