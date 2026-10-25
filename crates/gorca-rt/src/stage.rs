// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-stage timestamps for latency probes (M18a-4).
//!
//! Off by default: a disabled [`mark`] is one relaxed load. A benchmark turns the recorder on,
//! runs a trial, and drains the records of that trial. A record is a stage id, a tag (usually
//! a plugin id) and a `CLOCK_MONOTONIC` instant; nothing here ever holds text or model data.
//! The ring is lock-free and bounded: when it is full new records are dropped and counted.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::process::monotonic_ns;

const CAP: usize = 1 << 14;

static ON: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DROPPED: AtomicU64 = AtomicU64::new(0);
/// `stage << 16 | tag`.
static META: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];
/// The instant; 0 means "not written yet".
static TIME: [AtomicU64; CAP] = [const { AtomicU64::new(0) }; CAP];

/// Stage ids of the plugin call path (writer 1..19, worker 20..39).
pub mod id {
    /// `PluginBridge::interactive` entered.
    pub const W_ENTRY: u16 = 1;
    /// Snapshot and job lists built.
    pub const W_SNAP: u16 = 2;
    /// Governor claim done, runner called next.
    pub const W_CLAIM: u16 = 3;
    /// Runner entered (`interactive_sps` / `interactive`).
    pub const SC_ENTER: u16 = 4;
    /// Plan made.
    pub const SC_PLAN: u16 = 5;
    /// Every `DeriveJob` built.
    pub const SC_BUILT: u16 = 6;
    /// Actor handoff entered.
    pub const H_ENTER: u16 = 7;
    /// Waited jobs queued and the parked workers woken.
    pub const H_SENT_W: u16 = 8;
    /// All jobs out.
    pub const H_SENT_ALL: u16 = 9;
    /// The wait ended.
    pub const H_WAIT_END: u16 = 10;
    /// Replies polled (conversion done).
    pub const B_POLL: u16 = 11;
    /// Replies admitted: the turn is complete.
    pub const B_ADMIT: u16 = 12;
    /// Worker picked a job up (tag: plugin).
    pub const K_PICK: u16 = 20;
    /// Worker about to run the executor.
    pub const K_CALL0: u16 = 21;
    /// Guest call entered.
    pub const K_G0: u16 = 22;
    /// Guest call returned.
    pub const K_G1: u16 = 23;
    /// Executor returned its reply.
    pub const K_RUN1: u16 = 24;
    /// About to post the reply.
    pub const K_POST0: u16 = 25;
    /// Reply posted (mailbox push and writer wake done).
    pub const K_POST1: u16 = 26;
    /// Gate counted down.
    pub const K_GATE: u16 = 27;
    /// Not an instant: the nanoseconds the guest call spent inside host imports.
    pub const K_HOSTNS: u16 = 28;
    /// Not an instant: the number of host imports of the guest call.
    pub const K_NHOST: u16 = 29;
}

/// One timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Record {
    /// Stage id (the caller's numbering).
    pub stage: u16,
    /// Tag, typically the plugin id.
    pub tag: u16,
    /// `CLOCK_MONOTONIC` ns.
    pub ns: u64,
}

/// Turn the recorder on or off.
pub fn enable(on: bool) {
    ON.store(on, Ordering::Release);
}

/// True when the recorder is on.
#[must_use]
pub fn enabled() -> bool {
    ON.load(Ordering::Relaxed)
}

/// Record that `stage` was reached now (no-op when off).
#[inline]
pub fn mark(stage: u16, tag: u16) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    mark_at(stage, tag, monotonic_ns());
}

thread_local! {
    static TAG: std::cell::Cell<u16> = const { std::cell::Cell::new(0) };
}

/// Set the tag that [`mark_here`] uses on this thread (a worker sets its plugin id).
pub fn set_tag(tag: u16) {
    TAG.with(|t| t.set(tag));
}

/// [`mark`] with this thread's tag.
#[inline]
pub fn mark_here(stage: u16) {
    if ON.load(Ordering::Relaxed) {
        mark(stage, TAG.with(std::cell::Cell::get));
    }
}

/// Record a value (not an instant) under this thread's tag.
pub fn value_here(stage: u16, v: u64) {
    if ON.load(Ordering::Relaxed) {
        mark_at(stage, TAG.with(std::cell::Cell::get), v);
    }
}

/// As [`mark`] with a caller-measured instant.
pub fn mark_at(stage: u16, tag: u16, ns: u64) {
    if !ON.load(Ordering::Relaxed) {
        return;
    }
    let i = NEXT.fetch_add(1, Ordering::AcqRel);
    if i >= CAP {
        DROPPED.fetch_add(1, Ordering::Relaxed);
        return;
    }
    META[i].store((u64::from(stage) << 16) | u64::from(tag), Ordering::Relaxed);
    TIME[i].store(ns.max(1), Ordering::Release);
}

/// Take every record made since the last drain, in the order they were made, and reset.
/// Call it while no marker is running (between trials).
pub fn drain() -> Vec<Record> {
    let n = NEXT.swap(0, Ordering::AcqRel).min(CAP);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let ns = TIME[i].swap(0, Ordering::Acquire);
        if ns == 0 {
            continue;
        }
        let m = META[i].load(Ordering::Relaxed);
        out.push(Record {
            stage: u16::try_from(m >> 16).unwrap_or(u16::MAX),
            tag: u16::try_from(m & 0xffff).unwrap_or(0),
            ns,
        });
    }
    out
}

/// Records dropped because the ring was full.
#[must_use]
pub fn dropped() -> u64 {
    DROPPED.load(Ordering::Relaxed)
}
