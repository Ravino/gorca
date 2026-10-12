//! Per-app coalescing ingress (ADR-0001 section 6.3).
//!
//! Responsibility (section 11): "coalescing ingress: `CoalesceKey`, `Slot`, push/drain API
//! (B-2 arms behind it)". Written by the socket drain through [`IngressSender::push`] (O(1), never
//! awaits, C-1), drained by the app actor through [`IngressReceiver::drain`].
//!
//! The implementation here is the **reference** one: a mutex around an order-preserving slot
//! table. It exists to fix the API and the coalescing semantics (R-1 supersession, R-2 ordering,
//! R-3 overload, R-5 delete+insert collapse). Bake-off B-2 replaces the internals (lock-free MPSC,
//! generation-tagged lazy dedupe) behind the same API. TBD: adjacent-insert concatenation (needs
//! decoded text) and the depth-limited ancestor subsumption of `Children` slots.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `smallvec`. Never a transport crate.

mod coalesce;
mod dirty;

pub use coalesce::{
    CoalesceKey, Drained, HARD_CAP, IngressConfig, IngressReceiver, IngressSender, IngressStats,
    ingress,
};
pub use dirty::{DirtySet, MAX_DIRTY_NODES};
