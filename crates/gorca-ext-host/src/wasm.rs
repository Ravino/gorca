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
//! slot answering "no opinion": consistent with this failure mode. The epoch deadline stays, ten
//! times as long, as the backstop against anything fuel cannot see.
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
use std::time::Duration;

use gorca_ext_api as api;
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, ResourceLimiter, Store,
    Trap,
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
use bindings::gorca::ext::{host as wh, types as wt};
use bindings::{Extension, ExtensionPre};

/// Epoch tick of the wall-clock backstop.
pub const TICK: Duration = Duration::from_millis(1);
/// The backstop is this many times the CPU limit. `present` runs inline on the Reader, so its
/// backstop is tight (6 ms against the 2 ms limit; with fuel and the host-work budget below the
/// backstop only catches what neither can see, such as a descheduled thread or a slow page
/// fault). The other hooks run on workers or the app actor and keep a wide margin so a loaded
/// host does not turn a preempted healthy hook into a fault.
const BACKSTOP_FACTOR: u128 = 10;
const BACKSTOP_FACTOR_PRESENT: u128 = 3;
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
    let factor = if kind == HookKind::Present {
        BACKSTOP_FACTOR_PRESENT
    } else {
        BACKSTOP_FACTOR
    };
    u64::try_from((kind.limit().as_micros() * factor).div_ceil(t)).unwrap_or(u64::MAX)
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
            },
        );
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
    if kind == HookKind::Command {
        api::LIMIT_MEMORY_BYTES
    } else {
        1 << 20
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
    fn call<R>(
        &mut self,
        kind: HookKind,
        f: impl FnOnce(&wk::Guest, &mut Store<StoreData>) -> wasmtime::Result<R>,
    ) -> Result<R, Fault> {
        let _running = self.engine.enter();
        self.store.data().shared.begin_call();
        self.store.data_mut().work = HostWork::new(host_units(kind));
        self.store.data_mut().limiter.allowance = growth_allowance(kind);
        self.store.set_epoch_deadline(ticks(kind));
        // A store that cannot be refuelled is broken: report it as a fault, never panic.
        if let Err(e) = self.store.set_fuel(fuel(kind)) {
            return Err(Fault::Trap(format!("cannot refuel: {e:#}")));
        }
        let hooks = self.bindings.gorca_ext_hooks();
        f(hooks, &mut self.store).map_err(|e| classify(&self.store, &e))
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
        self.work.charge(k.len() + v.as_ref().map_or(0, String::len) * 2)?;
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
