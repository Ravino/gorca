// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin engine and one plugin instance (a wasmtime `Store`).
//!
//! * Engine: fuel on, **epoch off** (ADR-0026 §2.5.2), Component Model, bounded wasm stack.
//!   With `portable` the engine is built with `Config::target(<host triple>)`, which yields the
//!   plain CPU baseline without `unsafe` (ADR-0027 CD-W4).
//! * Instance: per-call fuel grant, `ResourceLimiter`, hostcall fuel so an oversize string or
//!   list is refused before it is copied, no WASI beyond traps.

use std::sync::Arc;
use std::time::{Duration, Instant};

use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{CallHook, Config, Engine, Store, Trap};

use crate::handles::HandleMinter;
use crate::host::bindings::{RulesHost, RulesHostPre};
use crate::host::{Effect, HostState, Kill};
use crate::limits::{FuelGrant, HOSTCALL_LIFT_CAP, MemoryLimits, PluginLimiter};
use crate::slot::FaultKind;
use crate::trust::{Perms, TrustTier};
use crate::view::ModelView;

/// Engine options.
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineOptions {
    /// `GORCA_CPU_DISABLE=all`: compile for the baseline of the host triple (distinct cache key).
    pub portable: bool,
}

/// Build the plugin engine.
pub fn build_engine(opts: EngineOptions) -> wasmtime::Result<Engine> {
    let mut c = Config::new();
    c.wasm_component_model(true);
    c.consume_fuel(true);
    c.epoch_interruption(false);
    c.max_wasm_stack(512 << 10);
    c.parallel_compilation(true);
    if opts.portable {
        c.target(&format!("{}-unknown-linux-gnu", std::env::consts::ARCH))?;
    }
    Engine::new(&c)
}

/// A pre-loaded, pre-linked component. Cheap to clone; instantiation takes microseconds.
#[derive(Clone)]
pub struct Loaded {
    engine: Engine,
    pre: RulesHostPre<HostState>,
}

impl std::fmt::Debug for Loaded {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Loaded")
    }
}

impl Loaded {
    /// Link the host imports and pre-instantiate. Imports outside the WIT world (the WASI the
    /// Rust standard library pulls in) trap on use: a plugin has no ambient capability.
    pub fn new(engine: &Engine, component: &Component) -> wasmtime::Result<Self> {
        let mut linker = Linker::<HostState>::new(engine);
        RulesHost::add_to_linker::<HostState, HasSelf<HostState>>(&mut linker, |s| s)?;
        crate::wasi::add(&mut linker, component, engine)?;
        linker.define_unknown_imports_as_traps(component)?;
        let pre = RulesHostPre::new(linker.instantiate_pre(component)?)?;
        Ok(Self {
            engine: engine.clone(),
            pre,
        })
    }
}

/// One dirty node of a batch, by host-side index (the instance mints the handle).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirtyNode {
    /// Host-side index.
    pub index: u32,
    /// Matching `[[watch]]` selector.
    pub watch: u16,
    /// Cause registry code.
    pub cause: u8,
}

/// The read set R of a call (ADR-0026 section 2.6.1): every node the batch named or an import
/// returned or verified.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReadSet {
    /// Host-side indices, deduplicated, at most `MAX_READ_SET`.
    pub nodes: Vec<u32>,
    /// More were touched than fit: the token falls back to the scope-wide stamp.
    pub overflow: bool,
}

/// A successful call.
#[derive(Debug)]
pub struct CallDone {
    /// The read set of the call.
    pub reads: ReadSet,
    /// Buffered effects, to be admitted by the writer.
    pub effects: Vec<Effect>,
    /// Wall time of the guest call.
    pub exec: Duration,
    /// Fuel consumed.
    pub fuel_used: u64,
    /// The call took longer than the wall rule allows (§2.5.2).
    pub wall_overrun: bool,
}

/// A faulted call.
#[derive(Debug)]
pub struct CallFault {
    /// Kind.
    pub kind: FaultKind,
    /// Wall time until the fault.
    pub exec: Duration,
}

/// A live instance.
pub struct PluginInstance {
    store: Store<HostState>,
    bindings: RulesHost,
    /// Instance serial (a rebuilt instance has a new key and a new serial).
    pub serial: u64,
}

impl std::fmt::Debug for PluginInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginInstance")
            .field("serial", &self.serial)
            .finish_non_exhaustive()
    }
}

impl PluginInstance {
    /// Instantiate with a fresh handle key.
    pub fn new(
        loaded: &Loaded,
        serial: u64,
        grants: Perms,
        tier: TrustTier,
        memory_mib: u32,
        view: Arc<dyn ModelView>,
    ) -> Result<Self, FaultKind> {
        let minter = HandleMinter::new(serial).map_err(|_| FaultKind::Instantiate)?;
        let state = HostState::new(
            grants,
            tier,
            minter,
            view,
            PluginLimiter::new(MemoryLimits::for_mib(memory_mib)),
        );
        let mut store = Store::new(&loaded.engine, state);
        store.limiter(|s| &mut s.limiter);
        store.set_hostcall_fuel(HOSTCALL_LIFT_CAP);
        // Imports charge fuel before they work; the charge is applied to the store's fuel when
        // control returns to the guest, so guest code and host work draw on one grant.
        store.call_hook(|mut ctx, kind| {
            if matches!(kind, CallHook::ReturningFromHost) {
                let p = std::mem::take(&mut ctx.data_mut().pending_fuel);
                if p > 0 {
                    let left = ctx.get_fuel()?;
                    ctx.set_fuel(left.saturating_sub(p))?;
                }
            }
            Ok(())
        });
        // Instantiation runs guest start code: give it a bounded grant too.
        store
            .set_fuel(
                FuelGrant::for_time(
                    crate::limits::B1_BACKGROUND,
                    crate::limits::KAPPA_PROVISIONAL,
                )
                .units,
            )
            .map_err(|_| FaultKind::Instantiate)?;
        let bindings = loaded.pre.instantiate(&mut store).map_err(|e| {
            tracing::warn!(error = %e, "plugin instantiate failed");
            FaultKind::Instantiate
        })?;
        Ok(Self {
            store,
            bindings,
            serial,
        })
    }

    /// Lifetime counters of this instance.
    #[must_use]
    pub fn counters(&self) -> crate::host::Counters {
        self.store.data().counters.clone()
    }

    /// Mint the guest handle of a host-side index.
    #[must_use]
    pub fn mint(&self, index: u32, generation: u16) -> u64 {
        self.store.data().minter.mint(index, generation)
    }

    /// Run `rules.derive` under `grant`. Effects are returned only on a clean return with no
    /// malformed effect; otherwise the call is a fault and nothing is kept (atomic, B-6).
    pub fn derive(
        &mut self,
        interactive: bool,
        epoch: u64,
        batch: &[DirtyNode],
        view: &Arc<dyn ModelView>,
        grant: FuelGrant,
    ) -> Result<CallDone, CallFault> {
        use crate::host::bindings::exports::gorca::ext::rules::{Dirty, RuleCtx};
        self.store.data_mut().begin_call(Arc::clone(view), grant);
        let tier = self.store.data().tier;
        let mut scope_denied = 0u64;
        let dirty: Vec<Dirty> = batch
            .iter()
            .filter(|d| {
                // The match path: a third-party plugin is never called for a node of a system
                // scope (SS-1..SS-4), whatever its selector says.
                let ok = crate::trust::selector_may_match(tier, view.system_scope(d.index));
                scope_denied += u64::from(!ok);
                ok
            })
            .filter_map(|d| {
                view.generation(d.index).map(|g| Dirty {
                    node: self.store.data().minter.mint(d.index, g),
                    watch: d.watch,
                    cause: d.cause,
                })
            })
            .collect();
        self.store.data_mut().counters.scope_denied += scope_denied;
        for d in batch {
            self.store.data_mut().note_read(d.index);
        }
        if dirty.is_empty() {
            return Ok(CallDone {
                reads: ReadSet::default(),
                effects: Vec::new(),
                exec: Duration::ZERO,
                fuel_used: 0,
                wall_overrun: false,
            });
        }
        let ctx = RuleCtx { epoch, interactive };
        if self.store.set_fuel(grant.units).is_err() {
            return Err(CallFault {
                kind: FaultKind::Instantiate,
                exec: Duration::ZERO,
            });
        }
        let t0 = Instant::now();
        let r = self
            .bindings
            .gorca_ext_rules()
            .call_derive(&mut self.store, ctx, &dirty);
        let exec = t0.elapsed();
        let left = self.store.get_fuel().unwrap_or(0);
        let fuel_used = grant.units.saturating_sub(left);
        match r {
            Ok(()) => {
                let st = self.store.data_mut();
                if st.malformed {
                    return Err(CallFault {
                        kind: FaultKind::Malformed,
                        exec,
                    });
                }
                Ok(CallDone {
                    reads: ReadSet {
                        nodes: std::mem::take(&mut st.reads),
                        overflow: st.reads_overflow,
                    },
                    effects: std::mem::take(&mut st.effects),
                    exec,
                    fuel_used,
                    wall_overrun: grant.is_wall_overrun(exec),
                })
            }
            Err(e) => Err(CallFault {
                kind: self.classify(&e),
                exec,
            }),
        }
    }

    fn classify(&self, e: &wasmtime::Error) -> FaultKind {
        let st = self.store.data();
        if e.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) {
            FaultKind::OutOfFuel
        } else if let Some(k) = st.kill {
            match k {
                Kill::Budget => FaultKind::OutOfFuel,
                Kill::Watchdog => FaultKind::Watchdog,
                Kill::Forgery => FaultKind::Malformed,
            }
        } else if st.limiter.denied {
            FaultKind::MemoryDenied
        } else if st.malformed || is_hostcall_fuel(e) {
            // Includes a lift refused by the hostcall fuel: oversize, refused before the copy.
            FaultKind::Malformed
        } else {
            FaultKind::Trap
        }
    }
}

/// wasmtime reports an exhausted per-call lift budget as a plain error with this text
/// ("too much data is being copied between the host and the guest: fuel allocated for hostcalls
/// has been exhausted"); there is no typed variant in 49.0.3. A test pins it.
fn is_hostcall_fuel(e: &wasmtime::Error) -> bool {
    e.root_cause()
        .to_string()
        .contains("fuel allocated for hostcalls")
}
