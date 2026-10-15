//! One extension for one application: the instance, its health and the fault policy.
//!
//! Every fault (trap, panic, epoch interrupt, native overrun) drops the instance and builds a
//! fresh one; the third reset within 60 seconds disables the slot for good, with one spoken
//! notice (ADR-0001 section 10, ADR-0008 section 3.3). A slot never lets a fault out: callers see
//! `None`, which is the same as "this extension has no opinion".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, TryLockError};
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

/// A loaded extension: manifest and instance factory.
#[derive(Debug)]
pub struct LoadedExt {
    pub manifest: Manifest,
    pub factory: Factory,
}

impl LoadedExt {
    fn create(&self, shared: &Arc<InstShared>) -> Result<Box<dyn HookInstance>, String> {
        match &self.factory {
            Factory::Wasm(m) => m
                .instantiate(Arc::clone(shared))
                .map(|i| Box::new(i) as Box<dyn HookInstance>)
                .map_err(|e| format!("{e:#}")),
            Factory::Native(f) => Ok(Box::new(NativeInstance::new(f, Arc::clone(shared)))),
        }
    }
}

struct SlotInner {
    inst: Option<Box<dyn HookInstance>>,
    guard: InstanceGuard,
}

pub struct Slot {
    ext: Arc<LoadedExt>,
    shared: Arc<InstShared>,
    app_label: Arc<str>,
    inner: Mutex<SlotInner>,
    disabled: AtomicBool,
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
    /// is full, the component traps in its start function): the caller skips the slot.
    pub fn new(
        ext: Arc<LoadedExt>,
        shared: Arc<InstShared>,
        app_label: Arc<str>,
        stats: Arc<ExtStats>,
        sink: Arc<OnceLock<Arc<dyn ExtSink>>>,
    ) -> Result<Self, String> {
        let inst = ext.create(&shared)?;
        Ok(Self {
            ext,
            shared,
            app_label,
            inner: Mutex::new(SlotInner {
                inst: Some(inst),
                guard: InstanceGuard::new(),
            }),
            disabled: AtomicBool::new(false),
            stats,
            sink,
        })
    }

    pub fn name(&self) -> &str {
        &self.ext.manifest.name
    }

    pub fn manifest(&self) -> &Manifest {
        &self.ext.manifest
    }

    pub fn health(&self) -> InstanceHealth {
        if self.disabled.load(Ordering::Acquire) {
            InstanceHealth::Disabled
        } else {
            InstanceHealth::Active
        }
    }

    fn lock(&self, blocking: bool) -> Option<MutexGuard<'_, SlotInner>> {
        if blocking {
            return Some(self.inner.lock().unwrap_or_else(PoisonError::into_inner));
        }
        match self.inner.try_lock() {
            Ok(g) => Some(g),
            Err(TryLockError::Poisoned(p)) => Some(p.into_inner()),
            Err(TryLockError::WouldBlock) => {
                self.stats.busy_skips.fetch_add(1, Ordering::Relaxed);
                None
            }
        }
    }

    /// Run `f` on the instance. `None` when the slot is disabled, busy (non-blocking calls only)
    /// or the call faulted (the instance is reset before this returns).
    pub fn call<R>(
        &self,
        kind: HookKind,
        blocking: bool,
        f: impl FnOnce(&mut dyn HookInstance) -> Result<R, Fault>,
    ) -> Option<R> {
        if self.disabled.load(Ordering::Acquire) {
            return None;
        }
        let mut inner = self.lock(blocking)?;
        let inst = inner.inst.as_mut()?;
        self.stats.hook_calls.fetch_add(1, Ordering::Relaxed);
        match f(inst.as_mut()) {
            Ok(v) => Some(v),
            Err(fault) => {
                self.reset(&mut inner, &fault, kind);
                None
            }
        }
    }

    fn reset(&self, inner: &mut SlotInner, fault: &Fault, kind: HookKind) {
        let t0 = Instant::now();
        self.stats.faults.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            ext = self.name(),
            app = &*self.app_label,
            ?kind,
            "extension fault: {fault}; resetting the instance"
        );
        // Drop first: a faulted instance must not outlive its fault, and its pool slot is
        // needed for the replacement.
        inner.inst = None;
        self.stats.resets.fetch_add(1, Ordering::Relaxed);
        if inner.guard.on_reset(t0) == InstanceHealth::Disabled {
            self.disable();
            return;
        }
        match self.ext.create(&self.shared) {
            Ok(i) => {
                inner.inst = Some(i);
                let ns = u64::try_from(t0.elapsed().as_nanos()).unwrap_or(u64::MAX);
                self.stats.max_reset_ns.fetch_max(ns, Ordering::Relaxed);
            }
            Err(e) => {
                tracing::error!(ext = self.name(), "cannot rebuild the instance: {e}");
                self.disable();
            }
        }
    }

    fn disable(&self) {
        if self.disabled.swap(true, Ordering::AcqRel) {
            return;
        }
        self.stats.disabled.fetch_add(1, Ordering::Relaxed);
        tracing::error!(
            ext = self.name(),
            app = &*self.app_label,
            "extension disabled for this application after repeated faults"
        );
        // The user is told once.
        if let Some(sink) = self.sink.get() {
            sink.speak(
                format!(
                    "Extension {} disabled for {}",
                    self.name().replace(['-', '_'], " "),
                    self.app_label
                ),
                false,
            );
        }
    }
}
