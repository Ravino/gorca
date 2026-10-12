//! Per-app coalescing ingress (ADR-0001 section 6.3).
//!
//! Responsibility (section 11): "coalescing ingress: `CoalesceKey`, `Slot`, push/drain API
//! (B-2 arms behind it)". Written by the socket drain through [`IngressSender::push`] (O(1), never
//! awaits, C-1), drained by the app actor through [`IngressReceiver::drain`].
//!
//! Candidate A of bake-off B-2: a bounded lock-free MPSC ring per app in front of a
//! consumer-side coalescing table. `push` is one CAS on the ring; the SPEC 4.2 rules are applied
//! when the ring is folded into the table (in `drain()`, or by a producer that finds the ring
//! full), in ring order, so a single producer sees exactly the executable model. The pending
//! events are kept in output order in one vector that `drain()` hands over without copying.
//! TBD: adjacent-insert concatenation (needs decoded text) and the depth-limited ancestor
//! subsumption of `Children` slots.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `smallvec`, `crossbeam-queue`. Never a
//! transport crate. No `unsafe`: the ring is an audited crate.

mod coalesce;
mod dirty;
mod hash;
mod table;

pub use coalesce::{
    CoalesceKey, Drained, GLOBAL_SLOT_BUDGET, HARD_CAP, IngressConfig, IngressReceiver,
    IngressSender, IngressStats, RESERVED_SLOTS, RING_CAP, SlotBudget, ingress,
    ingress_with_budget,
};
pub use dirty::{DirtySet, MAX_DIRTY_NODES};
