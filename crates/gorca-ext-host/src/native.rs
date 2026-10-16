//! The native backend (ADR-0008 section 3.3): the extension value is linked in-process and called
//! under `catch_unwind`; a call that returns after its CPU limit is a fault (detected, not
//! pre-empted: a native hang stalls its caller until the app actor's watchdog acts, which is why
//! `wasm` is the default and `native` must be chosen explicitly in the manifest).

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::Instant;

use gorca_ext_api as api;
use gorca_ext_api::host::with_host;

use crate::instance::{Fault, HookInstance, HookKind, InstShared, NativeHost};

/// Builds a fresh native extension value (initial creation and every reset).
pub type NativeFactory = Arc<dyn Fn() -> Box<dyn api::Hooks + Send> + Send + Sync>;

pub struct NativeInstance {
    ext: Box<dyn api::Hooks + Send>,
    shared: Arc<InstShared>,
    host: Arc<NativeHost>,
}

impl std::fmt::Debug for NativeInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NativeInstance")
            .field("ext", &self.shared.ext)
            .finish_non_exhaustive()
    }
}

impl NativeInstance {
    pub fn new(factory: &NativeFactory, shared: Arc<InstShared>) -> Self {
        Self {
            ext: factory(),
            host: Arc::new(NativeHost(Arc::clone(&shared))),
            shared,
        }
    }

    fn run<R>(
        &mut self,
        kind: HookKind,
        f: impl FnOnce(&mut dyn api::Hooks) -> R,
    ) -> Result<R, Fault> {
        self.shared.begin_call();
        let host: Arc<dyn api::Host> = self.host.clone();
        let ext = self.ext.as_mut();
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| with_host(host, || f(ext))));
        let took = t.elapsed();
        match r {
            Err(p) => Err(Fault::Panic(panic_text(&p))),
            Ok(_) if took > kind.limit() => Err(Fault::Overrun(took)),
            Ok(v) => Ok(v),
        }
    }
}

fn panic_text(p: &Box<dyn std::any::Any + Send>) -> String {
    p.downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned())
}

impl HookInstance for NativeInstance {
    fn on_event(&mut self, ev: &api::Event) -> Result<api::EventVerdict, Fault> {
        self.run(HookKind::OnEvent, |e| e.on_event(ev))
    }

    fn present(
        &mut self,
        n: &api::Node,
        ctx: api::PresentCtx,
    ) -> Result<Option<Vec<api::Segment>>, Fault> {
        self.run(HookKind::Present, |e| e.present(n, ctx))
    }

    fn policy(&mut self, f: &api::PolicyFacts) -> Result<api::PolicyVerdict, Fault> {
        self.run(HookKind::Policy, |e| e.policy(f))
    }

    fn nav_units(&mut self) -> Result<Vec<api::NavUnit>, Fault> {
        self.run(HookKind::NavUnits, |e| e.nav_units())
    }

    fn nav_match(&mut self, unit: u32, n: &api::Node) -> Result<bool, Fault> {
        self.run(HookKind::NavMatch, |e| e.nav_match(unit, n))
    }

    fn command(&mut self, id: u32) -> Result<api::CommandResult, Fault> {
        self.run(HookKind::Command, |e| e.command(id))
    }

    fn term_configure(&mut self, cfg: &api::terminal::TermConfig) -> Result<(), Fault> {
        self.run(HookKind::Terminal, |e| {
            if let Some(t) = e.terminal() {
                t.configure(cfg);
            }
        })
    }

    fn term_step(
        &mut self,
        now: u64,
        input: api::terminal::TermInput,
    ) -> Result<Vec<api::terminal::TermAction>, Fault> {
        self.run(HookKind::Terminal, |e| {
            let mut out = Vec::new();
            if let Some(t) = e.terminal() {
                t.step(now, input, &mut out);
            }
            out
        })
    }

    fn term_snapshot(&mut self) -> Result<Option<api::terminal::TermSnapshot>, Fault> {
        self.run(HookKind::Terminal, |e| {
            e.terminal().and_then(|t| t.snapshot())
        })
    }

    fn term_counters(&mut self) -> Result<api::terminal::TermCounters, Fault> {
        self.run(HookKind::Terminal, |e| {
            e.terminal().map(|t| t.counters()).unwrap_or_default()
        })
    }
}
