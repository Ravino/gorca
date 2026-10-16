//! The WASM backend: wasmtime component model, one `Store` per (extension, application), the
//! 32 MiB memory cap, a pooling allocator so a reset is a fresh `Store` from an `InstancePre`
//! (measured 9 to 31 microseconds, ADR-0008 section 2).
//!
//! # CPU limits: fuel first, epoch as the wall-clock backstop
//!
//! The 2 ms / 20 ms limits (ADR-0001 section 10, WE-5) are **CPU** limits, so they are enforced
//! with *fuel*: a deterministic count of executed WASM operations, which a descheduled thread
//! does not consume. An epoch deadline alone is wall-clock time; on a loaded workstation a healthy
//! hook that happens to be preempted for 2 ms would be interrupted and counted as a fault (three
//! of those disable the extension). An early epoch-only run of the storm benchmark in
//! `examples/x1.rs` (host load average 25) showed per-call medians near 35 ns, which is a disabled
//! slot answering "no opinion": consistent with this failure mode.
//!
//! The epoch deadline stays as the backstop against what fuel cannot see (host work, page faults),
//! but it is no longer a wall-clock limit. When the deadline fires, the epoch callback
//! ([`backstop`]) reads the calling thread's **CPU time** (`CLOCK_THREAD_CPUTIME_ID`) and
//! traps only when the call has consumed its CPU backstop (3x the limit for `present`/`policy`,
//! 10x for the rest), or hit a hard wall-clock ceiling ([`WALL_CEILING_FACTOR`] times the limit:
//! 100 ms for the hooks, 1 s for commands) that no scheduling delay should reach. Otherwise it
//! extends the deadline by one tick. A healthy hook that was descheduled therefore never faults;
//! a looping hook is cut by fuel at about 2 ms; a hook that stalls without CPU (a stuck host call)
//! is cut by the ceiling at its next return into the guest.
//!
//! # Worst-case Reader hold (`present`, `policy` run inline)
//!
//! - CPU the Reader spends in one call: bounded by fuel (2 ms) and the host-work meter; at most
//!   6 ms by the CPU backstop.
//! - Wall time: unchanged on an unloaded host (about 2 ms). On a loaded host the call can be as
//!   long as the Reader thread is descheduled, which no code of ours can shorten (the same delay
//!   hits every other Reader instruction); the hard ceiling bounds it at 100 ms of wall time, after
//!   which the call faults (a strike). The old 6 ms wall cut did not help the Reader here either:
//!   it ended the call but a descheduled thread resumes only when it is scheduled again.
//! - The host functions never block (they only touch in-memory state), so a host call cannot stall
//!   a call beyond the ceiling plus its own duration.
//!
//! `FUEL_PER_MS` converts the limit to fuel; see the constant for the calibration.
//!
//! The sandbox grants only the WIT `host` world. WASI imports that the Rust standard library of a
//! `wasm32-wasip2` extension pulls in are linked as traps, except the pure ones (clocks and
//! random), which are implemented here from the host's own clock: an extension can use
//! `HashMap` and `Instant`, and nothing else ambient.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::thread::{JoinHandle, Thread};
use std::time::{Duration, Instant};

use gorca_ext_api as api;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, ResourceLimiter, Store,
    Trap, UpdateDeadline,
};

use crate::instance::{Fault, HookInstance, HookKind, InstShared};

#[allow(
    clippy::all,
    clippy::pedantic,
    missing_docs,
    missing_debug_implementations,
    unused_qualifications
)]
mod bindings {
    wasmtime::component::bindgen!({
        world: "extension",
        path: "../gorca-ext-api/wit",
        // A host function that has exhausted its call's work budget traps the guest.
        imports: { default: trappable },
    });
}

use bindings::exports::gorca::ext::hooks as wk;
use bindings::exports::gorca::ext::terminal as wtm;
use bindings::gorca::ext::{host as wh, types as wt};
use bindings::{Extension, ExtensionPre};

/// Epoch tick of the wall-clock backstop.
pub const TICK: Duration = Duration::from_millis(1);
/// The backstop is this many times the CPU limit, measured in **thread CPU time** (see the module
/// documentation). `present` runs inline on the Reader, so its backstop is tight (6 ms of CPU
/// against the 2 ms limit; with fuel and the host-work budget below it, the backstop only catches
/// what neither can see, such as a slow page fault). The other hooks run on workers or the app
/// actor and keep a wide margin.
const BACKSTOP_FACTOR: u128 = 10;
const BACKSTOP_FACTOR_PRESENT: u128 = 3;
/// Hard wall-clock ceiling, as a multiple of the CPU limit: 100 ms for the 2 ms hooks, 1 s for
/// the 20 ms commands. Far above any scheduling delay a loaded workstation produces, so reaching
/// it means the call is stuck, not slow.
pub const WALL_CEILING_FACTOR: u32 = 50;
/// Most bytes (and bytes' worth of list elements) the host lifts out of guest memory in one
/// call into the host or one hook result. wasmtime refuses the lift before copying when a string
/// or list would exceed it ("hostcall fuel"), so a hostile length costs the host nothing.
pub const HOSTCALL_LIFT_BYTES: usize = 64 << 10;
/// Host-side work, in units of roughly one nanosecond (one byte copied or validated), a hook may
/// cause per millisecond of its CPU limit. Guest fuel does not see the host's time, so every host
/// function charges this meter and traps the guest when it runs out.
const HOST_UNITS_PER_MS: u64 = 1_000_000;
/// Fixed cost charged for every host call, on top of the bytes it moves.
const HOST_CALL_UNITS: u64 = 300;
/// Fuel per millisecond of CPU limit (see the module documentation). Calibrated on the 5.4 GHz
/// P-cores of the dev host: a bare counting loop (about 6 operations per ns, the fastest code
/// there is) is cut near 1 ms, typical hook code (1 to 3 operations per ns) near 2 to 6 ms.
pub const FUEL_PER_MS: u64 = 3_000_000;
/// Concurrent instances the pooling allocator reserves room for (extensions times applications).
pub const POOL_INSTANCES: u32 = 512;

/// Epoch ticks of the wall-clock backstop for `kind`.
fn ticks(kind: HookKind) -> u64 {
    let t = TICK.as_micros().max(1);
    let factor = if matches!(kind, HookKind::Present | HookKind::Policy | HookKind::Terminal) {
        BACKSTOP_FACTOR_PRESENT
    } else {
        BACKSTOP_FACTOR
    };
    u64::try_from((kind.limit().as_micros() * factor).div_ceil(t)).unwrap_or(u64::MAX)
}

/// Hard wall-clock ceiling of one call of `kind`.
pub fn wall_ceiling(kind: HookKind) -> Duration {
    kind.limit() * WALL_CEILING_FACTOR
}

/// CPU time a call of `kind` may consume before the backstop cuts it (the backstop ticks).
fn cpu_backstop(kind: HookKind) -> Duration {
    TICK * u32::try_from(ticks(kind)).unwrap_or(u32::MAX)
}

/// What the epoch callback does when a call's deadline fires.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Backstop {
    /// The call is healthy (under its CPU backstop, inside the ceiling): keep going one tick.
    Extend,
    /// The call consumed its CPU backstop: interrupt.
    CpuExhausted,
    /// The call is stuck: it hit the wall-clock ceiling without spending the CPU.
    Stuck,
}

/// Pure decision of the epoch callback, from the call's CPU and wall time so far.
fn backstop(clock: &CallClock, cpu_used: Duration, wall_used: Duration) -> Backstop {
    if cpu_used >= clock.cpu_backstop {
        Backstop::CpuExhausted
    } else if wall_used >= clock.wall_ceiling {
        Backstop::Stuck
    } else {
        Backstop::Extend
    }
}

/// Clock of the running call, read by the epoch callback.
#[derive(Debug)]
struct CallClock {
    cpu_start: Duration,
    wall_start: Instant,
    cpu_backstop: Duration,
    wall_ceiling: Duration,
}

impl CallClock {
    fn start(kind: HookKind) -> Self {
        Self {
            cpu_start: gorca_rt::sched::thread_cpu_time(),
            wall_start: Instant::now(),
            cpu_backstop: cpu_backstop(kind),
            wall_ceiling: wall_ceiling(kind),
        }
    }
}

/// Fuel for one call of `kind`.
fn fuel(kind: HookKind) -> u64 {
    u64::try_from(kind.limit().as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(FUEL_PER_MS)
}

/// Host work budget of one call of `kind`.
fn host_units(kind: HookKind) -> u64 {
    u64::try_from(kind.limit().as_millis())
        .unwrap_or(u64::MAX)
        .saturating_mul(HOST_UNITS_PER_MS)
}

/// The wasmtime engine and its epoch ticker.
///
/// The ticker only runs while a guest call is in flight (see [`InFlight`]); idle, it is parked and
/// costs nothing, so a host with extensions loaded does not wake the CPU 1000 times a second for
/// nothing. Epoch deadlines are relative to the current epoch, so a paused clock changes nothing
/// for the next call.
pub struct ExtEngine {
    engine: Engine,
    stop: Arc<AtomicBool>,
    in_flight: Arc<AtomicUsize>,
    ticks_done: Arc<AtomicU64>,
    ticker: OnceLock<JoinHandle<()>>,
    ticker_thread: OnceLock<Thread>,
}

/// Marks a guest call (or instantiation) in flight: the epoch ticker runs while any exists.
struct InFlight<'a>(&'a ExtEngine);

impl ExtEngine {
    fn enter(&self) -> InFlight<'_> {
        if self.in_flight.fetch_add(1, Ordering::AcqRel) == 0 {
            // A token left by an unpark that races the park is consumed by the next park.
            if let Some(t) = self.ticker_thread.get() {
                t.unpark();
            }
        }
        InFlight(self)
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl std::fmt::Debug for ExtEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtEngine").finish_non_exhaustive()
    }
}

impl ExtEngine {
    pub fn new() -> wasmtime::Result<Arc<Self>> {
        let mut c = Config::new();
        c.wasm_component_model(true);
        c.epoch_interruption(true);
        c.consume_fuel(true);
        // A trap is data to the host; the backtrace is not worth the reset latency.
        c.wasm_backtrace_max_frames(None);
        c.parallel_compilation(true);
        let mut p = PoolingAllocationConfig::default();
        p.total_component_instances(POOL_INSTANCES);
        p.total_core_instances(POOL_INSTANCES * 4);
        p.total_memories(POOL_INSTANCES * 2);
        p.total_tables(POOL_INSTANCES * 2);
        p.max_memory_size(api::LIMIT_MEMORY_BYTES);
        // One linear memory per component: the 32 MiB cap is per instance, not per memory.
        p.max_memories_per_component(1);
        p.max_tables_per_component(4);
        p.max_core_instances_per_component(8);
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(p));
        Ok(Arc::new(Self {
            engine: Engine::new(&c)?,
            stop: Arc::new(AtomicBool::new(false)),
            in_flight: Arc::new(AtomicUsize::new(0)),
            ticks_done: Arc::new(AtomicU64::new(0)),
            ticker: OnceLock::new(),
            ticker_thread: OnceLock::new(),
        }))
    }

    /// Epoch ticks the backstop clock has made so far (tests: an idle engine makes none).
    pub fn ticks_done(&self) -> u64 {
        self.ticks_done.load(Ordering::Relaxed)
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Start the epoch ticker (once, on first use: a host with no WASM extension runs no thread).
    fn ensure_ticker(&self) {
        let handle = self.ticker.get_or_init(|| {
            let engine = self.engine.clone();
            let stop = Arc::clone(&self.stop);
            let in_flight = Arc::clone(&self.in_flight);
            let ticks_done = Arc::clone(&self.ticks_done);
            // A dedicated OS thread: its sleep is the clock that interrupts hung extensions.
            #[allow(clippy::disallowed_methods)]
            std::thread::Builder::new()
                .name("gorca-ext-epoch".to_owned())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        if in_flight.load(Ordering::Acquire) == 0 {
                            // Idle: sleep until a call starts (or the engine is dropped).
                            std::thread::park();
                            continue;
                        }
                        std::thread::sleep(TICK);
                        engine.increment_epoch();
                        ticks_done.fetch_add(1, Ordering::Relaxed);
                    }
                })
                .unwrap_or_else(|e| {
                    // Without a ticker there is no hang protection: say so loudly, and let the
                    // caller's fault accounting disable what it cannot interrupt.
                    tracing::error!("cannot start the extension epoch ticker: {e}");
                    std::thread::spawn(|| {})
                })
        });
        let _ = self.ticker_thread.get_or_init(|| handle.thread().clone());
    }
}

impl Drop for ExtEngine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.ticker_thread.get() {
            t.unpark();
        }
        if let Some(t) = self.ticker.take() {
            let _ = t.join();
        }
    }
}

/// A compiled extension, ready to instantiate for any number of applications.
pub struct WasmModule {
    engine: Arc<ExtEngine>,
    pre: ExtensionPre<StoreData>,
}

impl std::fmt::Debug for WasmModule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmModule").finish_non_exhaustive()
    }
}

impl WasmModule {
    /// Compile `bytes` (a component) and link it against the host world.
    pub fn new(engine: &Arc<ExtEngine>, bytes: &[u8]) -> wasmtime::Result<Self> {
        let component = Component::new(&engine.engine, bytes)?;
        let mut linker: Linker<StoreData> = Linker::new(&engine.engine);
        Extension::add_to_linker::<StoreData, HasSelf<StoreData>>(&mut linker, |d| d)?;
        crate::wasi_pure::add(&mut linker, &component, &engine.engine)?;
        linker.define_unknown_imports_as_traps(&component)?;
        let pre = ExtensionPre::new(linker.instantiate_pre(&component)?)?;
        Ok(Self {
            engine: Arc::clone(engine),
            pre,
        })
    }

    /// A fresh instance with its own store and memory.
    pub fn instantiate(&self, shared: Arc<InstShared>) -> wasmtime::Result<WasmInstance> {
        self.engine.ensure_ticker();
        let mut store = Store::new(
            &self.engine.engine,
            StoreData {
                shared,
                limiter: Limiter {
                    denied: false,
                    allowance: api::LIMIT_MEMORY_BYTES,
                },
                work: HostWork::new(host_units(HookKind::Command)),
                clock: CallClock::start(HookKind::Command),
            },
        );
        store.epoch_deadline_callback(|ctx| {
            let c = &ctx.data().clock;
            let cpu = gorca_rt::sched::thread_cpu_time().saturating_sub(c.cpu_start);
            match backstop(c, cpu, c.wall_start.elapsed()) {
                Backstop::Extend => Ok(UpdateDeadline::Continue(1)),
                Backstop::CpuExhausted | Backstop::Stuck => Ok(UpdateDeadline::Interrupt),
            }
        });
        store.limiter(|d| &mut d.limiter);
        store.set_hostcall_fuel(HOSTCALL_LIFT_BYTES);
        // Instantiation runs the start function: bound it like a command.
        store.set_epoch_deadline(ticks(HookKind::Command));
        store.set_fuel(fuel(HookKind::Command))?;
        let _running = self.engine.enter();
        let bindings = self.pre.instantiate(&mut store)?;
        Ok(WasmInstance {
            store,
            bindings,
            engine: Arc::clone(&self.engine),
        })
    }
}

/// Memory and table caps of one store. Growth past the cap is refused and remembered, so the
/// resulting trap is classified as a memory fault.
#[derive(Debug, Default)]
struct Limiter {
    denied: bool,
    /// Bytes of growth this call may still ask for (see [`growth_allowance`]).
    allowance: usize,
}

/// Fresh linear memory a hook call may claim. Fuel does not see the page faults of touching new
/// memory (16 MiB costs the calling thread about 5 ms), so a hook that is supposed to take
/// microseconds may grow by 1 MiB per call; commands and instantiation may use the whole cap.
fn growth_allowance(kind: HookKind) -> usize {
    match kind {
        HookKind::Command => api::LIMIT_MEMORY_BYTES,
        // A screen of up to 256 KiB is copied, indexed and diffed (a few copies of it).
        HookKind::Terminal => 8 << 20,
        _ => 1 << 20,
    }
}

/// Most bytes the host lifts out of the guest in one terminal result: the snapshot of a screen
/// (up to 256 KiB of text, four bytes a scalar at most) is the biggest.
pub const HOSTCALL_LIFT_TERMINAL: usize = 1 << 20;

fn hostcall_fuel(kind: HookKind) -> usize {
    if kind == HookKind::Terminal {
        HOSTCALL_LIFT_TERMINAL
    } else {
        HOSTCALL_LIFT_BYTES
    }
}

const MAX_TABLE_ELEMENTS: usize = 10_000;

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        let delta = desired.saturating_sub(current);
        if desired > api::LIMIT_MEMORY_BYTES || delta > self.allowance {
            self.denied = true;
            return Ok(false);
        }
        self.allowance -= delta;
        Ok(true)
    }

    fn table_growing(
        &mut self,
        _current: usize,
        desired: usize,
        _maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        Ok(desired <= MAX_TABLE_ELEMENTS)
    }

    fn instances(&self) -> usize {
        8
    }
    fn tables(&self) -> usize {
        8
    }
    fn memories(&self) -> usize {
        1
    }
}

pub struct StoreData {
    shared: Arc<InstShared>,
    limiter: Limiter,
    work: HostWork,
    clock: CallClock,
}

/// The host-side work meter of the running call (see [`HOST_UNITS_PER_MS`]).
#[derive(Debug)]
struct HostWork {
    used: u64,
    limit: u64,
    exhausted: bool,
}

impl HostWork {
    fn new(limit: u64) -> Self {
        Self {
            used: 0,
            limit,
            exhausted: false,
        }
    }

    /// Charge one host call that moves `bytes`; an error traps the guest.
    fn charge(&mut self, bytes: usize) -> wasmtime::Result<()> {
        self.used = self
            .used
            .saturating_add(HOST_CALL_UNITS)
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if self.used > self.limit {
            self.exhausted = true;
            wasmtime::bail!("host work budget exhausted");
        }
        Ok(())
    }
}

impl crate::wasi_pure::HostMeter for StoreData {
    fn charge(&mut self, bytes: usize) -> wasmtime::Result<()> {
        self.work.charge(bytes)
    }
}

pub struct WasmInstance {
    store: Store<StoreData>,
    bindings: Extension,
    engine: Arc<ExtEngine>,
}

impl std::fmt::Debug for WasmInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WasmInstance")
            .field("ext", &self.store.data().shared.ext)
            .finish_non_exhaustive()
    }
}

fn classify(store: &Store<StoreData>, e: &wasmtime::Error) -> Fault {
    if store.data().limiter.denied {
        return Fault::Memory;
    }
    if store.data().work.exhausted {
        return Fault::Timeout;
    }
    match e.downcast_ref::<Trap>() {
        Some(Trap::OutOfFuel | Trap::Interrupt) => Fault::Timeout,
        Some(t) => Fault::Trap(t.to_string()),
        None => Fault::Trap(format!("{e:#}").chars().take(300).collect()),
    }
}

impl WasmInstance {
    /// Arm the limits of one call of `kind`: fuel, epoch deadline, host-work meter, growth.
    fn arm(store: &mut Store<StoreData>, kind: HookKind) -> Result<(), Fault> {
        store.data().shared.begin_call();
        store.data_mut().work = HostWork::new(host_units(kind));
        store.data_mut().limiter.allowance = growth_allowance(kind);
        store.data_mut().clock = CallClock::start(kind);
        store.set_epoch_deadline(ticks(kind));
        store.set_hostcall_fuel(hostcall_fuel(kind));
        // A store that cannot be refuelled is broken: report it as a fault, never panic.
        if let Err(e) = store.set_fuel(fuel(kind)) {
            return Err(Fault::Trap(format!("cannot refuel: {e:#}")));
        }
        Ok(())
    }

    fn call<R>(
        &mut self,
        kind: HookKind,
        f: impl FnOnce(&wk::Guest, &mut Store<StoreData>) -> wasmtime::Result<R>,
    ) -> Result<R, Fault> {
        let _running = self.engine.enter();
        Self::arm(&mut self.store, kind)?;
        let hooks = self.bindings.gorca_ext_hooks();
        f(hooks, &mut self.store).map_err(|e| classify(&self.store, &e))
    }

    /// A call into the `terminal` export.
    fn call_term<R>(
        &mut self,
        f: impl FnOnce(&wtm::Guest, &mut Store<StoreData>) -> wasmtime::Result<R>,
    ) -> Result<R, Fault> {
        let _running = self.engine.enter();
        Self::arm(&mut self.store, HookKind::Terminal)?;
        let term = self.bindings.gorca_ext_terminal();
        f(term, &mut self.store).map_err(|e| classify(&self.store, &e))
    }
}

fn term_key_to_wit(k: api::terminal::TermKey) -> wtm::TermKey {
    use api::terminal::TermKey as K;
    match k {
        K::Typed(c) => wtm::TermKey::Typed(c),
        K::Enter => wtm::TermKey::Enter,
        K::Backspace => wtm::TermKey::Backspace,
        K::Delete => wtm::TermKey::Delete,
        K::Tab => wtm::TermKey::Tab,
        K::CharLeft => wtm::TermKey::CharLeft,
        K::CharRight => wtm::TermKey::CharRight,
        K::WordLeft => wtm::TermKey::WordLeft,
        K::WordRight => wtm::TermKey::WordRight,
        K::LineStart => wtm::TermKey::LineStart,
        K::LineEnd => wtm::TermKey::LineEnd,
        K::LineUp => wtm::TermKey::LineUp,
        K::LineDown => wtm::TermKey::LineDown,
        K::Interrupt => wtm::TermKey::Interrupt,
        K::Other => wtm::TermKey::Other,
    }
}

fn term_input_to_wit(i: api::terminal::TermInput) -> wtm::TermInput {
    use api::terminal::{TermEdit as E, TermInput as I};
    match i {
        I::Key(k) => wtm::TermInput::Key(term_key_to_wit(k)),
        I::Raw(k) => wtm::TermInput::Raw(wtm::RawKey {
            keysym: k.keysym,
            unichar: k.unichar,
            phys: k.phys,
            shift: k.shift,
            ctrl: k.ctrl,
            alt: k.alt,
            logo: k.logo,
        }),
        I::Edit(E::Insert { offset, len, text }) => {
            wtm::TermInput::Edit(wtm::TermEdit::Insert((offset, len, text)))
        }
        I::Edit(E::Delete { offset, len }) => {
            wtm::TermInput::Edit(wtm::TermEdit::Delete((offset, len)))
        }
        I::Edit(E::Replaced) => wtm::TermInput::Edit(wtm::TermEdit::Replaced),
        I::Caret(o) => wtm::TermInput::Caret(o),
        I::Screen { text, caret } => wtm::TermInput::Screen((text, caret)),
        I::ScreenFailed => wtm::TermInput::ScreenFailed,
        I::Tick => wtm::TermInput::Tick,
        I::Focus => wtm::TermInput::Focus,
        I::Blur => wtm::TermInput::Blur,
    }
}

fn term_action_to_api(a: wtm::TermAction) -> api::terminal::TermAction {
    use api::terminal::{TermAction as A, Unit, Urgency};
    match a {
        wtm::TermAction::Say((text, unit, urgency, sensitive)) => A::Say {
            text,
            unit: match unit {
                wtm::Unit::Char => Unit::Char,
                wtm::Unit::Word => Unit::Word,
                wtm::Unit::Line => Unit::Line,
                wtm::Unit::Text => Unit::Text,
            },
            urgency: match urgency {
                wtm::Urgency::Key => Urgency::Key,
                wtm::Urgency::Queued => Urgency::Queued,
                wtm::Urgency::Output => Urgency::Output,
            },
            sensitive,
        },
        wtm::TermAction::FetchScreen => A::FetchScreen,
        wtm::TermAction::ArmTimer(t) => A::ArmTimer(t),
        wtm::TermAction::ClaimKey => A::ClaimKey,
    }
}

macro_rules! to_wit_role {
    ($($v:ident),*) => {
        fn role_to_wit(r: api::Role) -> wt::Role {
            match r { $(api::Role::$v => wt::Role::$v),* }
        }
    };
}
api::for_each_role!(to_wit_role);

fn node_to_wit(n: &api::Node) -> wt::Node {
    wt::Node {
        key: n.key,
        role: role_to_wit(n.role),
        states: n.states,
        name: n.name.clone(),
        description: n.description.clone(),
        range: n.range.map(|r| wt::Range {
            current: r.current,
            min: r.min,
            max: r.max,
        }),
    }
}

fn segment_to_api(s: wt::Segment) -> api::Segment {
    api::Segment {
        text: s.text,
        kind: match s.kind {
            wt::SegmentKind::Text => api::SegmentKind::Text,
            wt::SegmentKind::Role => api::SegmentKind::Role,
            wt::SegmentKind::State => api::SegmentKind::State,
            wt::SegmentKind::Value => api::SegmentKind::Value,
        },
    }
}

fn event_to_wit(e: &api::Event) -> wt::Event {
    match e {
        api::Event::FocusChanged(k) => wt::Event::FocusChanged(*k),
        api::Event::PropertiesChanged(k, m) => wt::Event::PropertiesChanged((*k, *m)),
        api::Event::ChildrenChanged(k) => wt::Event::ChildrenChanged(*k),
        api::Event::TextChanged(k) => wt::Event::TextChanged(*k),
        api::Event::CaretMoved(k, o) => wt::Event::CaretMoved((*k, *o)),
        api::Event::SelectionChanged(k) => wt::Event::SelectionChanged(*k),
        api::Event::Announcement(s) => wt::Event::Announcement(s.clone()),
        api::Event::WindowActivated(k) => wt::Event::WindowActivated(*k),
        api::Event::NodeRemoved(k) => wt::Event::NodeRemoved(*k),
    }
}

impl HookInstance for WasmInstance {
    fn on_event(&mut self, ev: &api::Event) -> Result<api::EventVerdict, Fault> {
        let ev = event_to_wit(ev);
        let v = self.call(HookKind::OnEvent, |h, s| h.call_on_event(s, &ev))?;
        Ok(match v {
            wt::EventVerdict::Pass => api::EventVerdict::Pass,
            wt::EventVerdict::Suppress => api::EventVerdict::Suppress,
            wt::EventVerdict::Rewrite(s) => {
                api::EventVerdict::Rewrite(s.into_iter().map(segment_to_api).collect())
            }
            wt::EventVerdict::ReEvaluate => api::EventVerdict::ReEvaluate,
        })
    }

    fn present(
        &mut self,
        n: &api::Node,
        ctx: api::PresentCtx,
    ) -> Result<Option<Vec<api::Segment>>, Fault> {
        let node = node_to_wit(n);
        let ctx = wt::PresentCtx {
            epoch: ctx.epoch,
            reason: match ctx.reason {
                api::PresentReason::Focus => wt::PresentReason::Focus,
                api::PresentReason::Navigation => wt::PresentReason::Navigation,
                api::PresentReason::SayAll => wt::PresentReason::SayAll,
                api::PresentReason::Review => wt::PresentReason::Review,
            },
        };
        let r = self.call(HookKind::Present, |h, s| h.call_present(s, &node, ctx))?;
        Ok(r.map(|v| v.into_iter().map(segment_to_api).collect()))
    }

    fn policy(&mut self, f: &api::PolicyFacts) -> Result<api::PolicyVerdict, Fault> {
        let facts = wt::PolicyFacts {
            moment: match f.moment {
                api::PolicyMoment::Focus => wt::PolicyMoment::Focus,
                api::PolicyMoment::Value => wt::PolicyMoment::Value,
                api::PolicyMoment::Activate => wt::PolicyMoment::Activate,
                api::PolicyMoment::Showing => wt::PolicyMoment::Showing,
            },
            role: role_to_wit(f.role),
            states: f.states,
            name: f.name.clone(),
            description: f.description.clone(),
            range: f.range.map(|r| wt::Range {
                current: r.current,
                min: r.min,
                max: r.max,
            }),
        };
        let v = self.call(HookKind::Policy, |h, s| h.call_policy(s, &facts))?;
        Ok(match v {
            wt::PolicyVerdict::Defer => api::PolicyVerdict::Defer,
            wt::PolicyVerdict::Silence => api::PolicyVerdict::Silence,
            wt::PolicyVerdict::Value(s) => api::PolicyVerdict::Value(s),
            wt::PolicyVerdict::Say(s) => {
                api::PolicyVerdict::Say(s.into_iter().map(segment_to_api).collect())
            }
            wt::PolicyVerdict::Notice => api::PolicyVerdict::Notice,
        })
    }

    fn nav_units(&mut self) -> Result<Vec<api::NavUnit>, Fault> {
        let r = self.call(HookKind::NavUnits, |h, s| h.call_nav_units(s))?;
        Ok(r.into_iter()
            .map(|u| api::NavUnit {
                id: u.id,
                name: u.name,
            })
            .collect())
    }

    fn nav_match(&mut self, unit: u32, n: &api::Node) -> Result<bool, Fault> {
        let node = node_to_wit(n);
        self.call(HookKind::NavMatch, |h, s| h.call_nav_match(s, unit, &node))
    }

    fn command(&mut self, id: u32) -> Result<api::CommandResult, Fault> {
        let r = self.call(HookKind::Command, |h, s| h.call_command(s, id))?;
        Ok(match r {
            wt::CommandResult::Handled => api::CommandResult::Handled,
            wt::CommandResult::NotHandled => api::CommandResult::NotHandled,
            wt::CommandResult::Speak(s) => {
                api::CommandResult::Speak(s.into_iter().map(segment_to_api).collect())
            }
        })
    }

    fn term_configure(&mut self, cfg: &api::terminal::TermConfig) -> Result<(), Fault> {
        let cfg = wtm::TermConfig {
            settle_ms: cfg.settle_ms,
            max_wait_ms: cfg.max_wait_ms,
            prompt_markers: cfg.prompt_markers.clone(),
            prompt_regex: cfg.prompt_regex.clone(),
            secret_regex: cfg.secret_regex.clone(),
            burst_lines: cfg.burst_lines,
            burst_tail: cfg.burst_tail,
            max_output_chars: cfg.max_output_chars,
        };
        self.call_term(|t, s| t.call_configure(s, &cfg))
    }

    fn term_step(
        &mut self,
        now: u64,
        input: api::terminal::TermInput,
    ) -> Result<Vec<api::terminal::TermAction>, Fault> {
        let input = term_input_to_wit(input);
        let r = self.call_term(|t, s| t.call_step(s, now, &input))?;
        Ok(r.into_iter().map(term_action_to_api).collect())
    }

    fn term_snapshot(&mut self) -> Result<Option<api::terminal::TermSnapshot>, Fault> {
        let r = self.call_term(|t, s| t.call_snapshot(s))?;
        Ok(r.map(|x| api::terminal::TermSnapshot {
            text: x.text,
            caret: x.caret,
            generation: x.generation,
        }))
    }

    fn term_counters(&mut self) -> Result<api::terminal::TermCounters, Fault> {
        let c = self.call_term(|t, s| t.call_counters(s))?;
        Ok(api::terminal::TermCounters {
            fetches: c.fetches,
            fetch_failed: c.fetch_failed,
            settled_local: c.settled_local,
            resync_mismatch: c.resync_mismatch,
            output_spoken: c.output_spoken,
            output_lines_spoken: c.output_lines_spoken,
            output_lines_skipped: c.output_lines_skipped,
            repaints: c.repaints,
            typed_echoed: c.typed_echoed,
            typed_explained: c.typed_explained,
            history_read: c.history_read,
            secret_prompts: c.secret_prompts,
        })
    }
}

// The host world, over the shared host functions.
impl wt::Host for StoreData {}

impl wh::Host for StoreData {
    fn get_node(&mut self, key: wt::NodeKey) -> wasmtime::Result<Option<wt::Node>> {
        let n = self.shared.get_node(key);
        self.work.charge(n.as_ref().map_or(0, node_bytes))?;
        Ok(n.map(|n| node_to_wit(&n)))
    }

    fn fetch(
        &mut self,
        _key: wt::NodeKey,
        _mask: u32,
        _b: wt::Budget,
    ) -> wasmtime::Result<Result<wt::Node, wt::QueryError>> {
        self.work.charge(0)?;
        Ok(Err(query_error_to_wit(self.shared.fetch_error())))
    }

    fn text(
        &mut self,
        _key: wt::NodeKey,
        _start: u32,
        _end: u32,
        _b: wt::Budget,
    ) -> wasmtime::Result<Result<String, wt::QueryError>> {
        self.work.charge(0)?;
        Ok(Err(query_error_to_wit(self.shared.fetch_error())))
    }

    fn speak(&mut self, segments: Vec<wt::Segment>, prio: wt::Prio) -> wasmtime::Result<()> {
        // The lift is already done (and bounded by the hostcall fuel): charge what it cost.
        self.work
            .charge(segments.iter().map(|s| s.text.len() + 16).sum())?;
        let segs: Vec<api::Segment> = segments.into_iter().map(segment_to_api).collect();
        let prio = match prio {
            wt::Prio::Interrupt => api::Prio::Interrupt,
            wt::Prio::Message => api::Prio::Message,
            wt::Prio::Text => api::Prio::Text,
            wt::Prio::Progress => api::Prio::Progress,
            wt::Prio::Notification => api::Prio::Notification,
        };
        self.shared.speak(&segs, prio);
        Ok(())
    }

    fn get_setting(&mut self, k: String) -> wasmtime::Result<Option<String>> {
        // The value is copied (and lowered into guest memory) on the way out: charge it too.
        let v = self.shared.get_setting(&k);
        self.work
            .charge(k.len() + v.as_ref().map_or(0, String::len) * 2)?;
        Ok(v)
    }

    fn set_setting(&mut self, k: String, v: String) -> wasmtime::Result<()> {
        self.work.charge(k.len() + v.len())?;
        self.shared.set_setting(&k, &v);
        Ok(())
    }

    fn log(&mut self, level: u8, msg: String) -> wasmtime::Result<()> {
        self.work.charge(msg.len())?;
        self.shared.log(level, &msg);
        Ok(())
    }
}

fn node_bytes(n: &api::Node) -> usize {
    n.name.as_ref().map_or(0, String::len) + n.description.as_ref().map_or(0, String::len) + 32
}

fn query_error_to_wit(e: api::QueryError) -> wt::QueryError {
    match e {
        api::QueryError::Timeout => wt::QueryError::Timeout,
        api::QueryError::Gone => wt::QueryError::Gone,
        api::QueryError::NotSupported => wt::QueryError::NotSupported,
        api::QueryError::Denied => wt::QueryError::Denied,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_but_idle_call_extends() {
        let c = CallClock::start(HookKind::Present);
        assert_eq!(
            backstop(&c, Duration::from_micros(200), Duration::from_millis(60)),
            Backstop::Extend
        );
    }

    #[test]
    fn a_call_that_spent_its_cpu_backstop_is_cut() {
        let c = CallClock::start(HookKind::Present);
        assert_eq!(
            backstop(&c, Duration::from_millis(6), Duration::from_millis(7)),
            Backstop::CpuExhausted
        );
    }

    #[test]
    fn a_call_past_the_wall_ceiling_without_cpu_is_stuck() {
        let c = CallClock::start(HookKind::Present);
        assert_eq!(
            backstop(&c, Duration::ZERO, Duration::from_millis(100)),
            Backstop::Stuck
        );
        assert_eq!(wall_ceiling(HookKind::Present), Duration::from_millis(100));
        assert_eq!(wall_ceiling(HookKind::Command), Duration::from_secs(1));
    }
}
