// SPDX-License-Identifier: MIT OR Apache-2.0
//! The one boundary where the arms differ (SPEC 2.3): [`WLaneRunner`], its message types, and the
//! runners that belong to the harness: [`NoopRunner`] (smoke, plugins off), [`RefInlineRunner`]
//! (the trivial reference that proves the harness end to end; NOT a candidate) and the three
//! deliberately bad runners that the harness self-tests must reject (SPEC 7 requirement 5).

use crate::host::{DoneReply, FaultKind, PluginBatch, PluginReply, PluginRuntime};
use crate::model::{FrozenRowsFactory, JobId, PluginId};
use crate::timing::now_ns;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

/// Writer turn context handed to the runner.
#[derive(Copy, Clone, Debug)]
pub struct TurnCtx {
    pub turn: u64,
    /// `CLOCK_MONOTONIC` ns at the first instruction of the writer turn.
    pub t0_ns: u64,
    /// `t0 + 0.3 ms`, absolute `CLOCK_MONOTONIC` ns.
    pub deadline_ns: u64,
    /// Writer clock `T` after the S0 recompute (snapshot basis).
    pub t: u64,
    /// This writer owns the bounded wait of the keypress path. `false`: send, do not wait
    /// (the second writer of a K1+K2 path, or a flapping writer inside the `N` ms window).
    pub wait: bool,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct HandoffOutcome {
    pub arrived: u32,
    pub asked: u32,
    pub waited: Duration,
    /// Actual wake minus the deadline (zero when the countdown ended the wait early).
    pub woke_late: Duration,
    pub busy_skips: u32,
}

/// Where replies come back. Cheap to clone, usable from any thread; never blocks the sender.
#[derive(Clone)]
pub struct WriterMailbox {
    tx: Sender<PluginReply>,
    depth: Arc<AtomicUsize>,
}

pub struct MailboxRx {
    rx: Receiver<PluginReply>,
    depth: Arc<AtomicUsize>,
}

pub fn mailbox() -> (WriterMailbox, MailboxRx) {
    let (tx, rx) = channel();
    let depth = Arc::new(AtomicUsize::new(0));
    (
        WriterMailbox {
            tx,
            depth: depth.clone(),
        },
        MailboxRx { rx, depth },
    )
}

impl WriterMailbox {
    pub fn send(&self, r: PluginReply) {
        self.depth.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(r).is_err() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
        }
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

impl MailboxRx {
    pub fn try_recv(&self) -> Option<PluginReply> {
        let r = self.rx.try_recv().ok();
        if r.is_some() {
            self.depth.fetch_sub(1, Ordering::Relaxed);
        }
        r
    }

    pub fn depth(&self) -> usize {
        self.depth.load(Ordering::Relaxed)
    }
}

/// The runner interface. Implemented by the two arms; admission, slots, budgets, fusion, tokens
/// and metrics live in the shared writer and are the same code in both arms.
pub trait WLaneRunner: Send {
    /// Interactive handoff for one focus change. Returns when the arm's wait policy says so
    /// (A: after all calls ran inline; B': at the zero countdown or at `deadline`). Replies that
    /// did not make it are delivered later through `writer_mailbox`. Replies that made it are
    /// already in the mailbox when this returns.
    fn interactive(
        &mut self,
        turn: &TurnCtx,
        batches: &[PluginBatch],
        rows: &FrozenRowsFactory<'_>,
        writer_mailbox: &WriterMailbox,
    ) -> HandoffOutcome;

    /// Background slice: never waits; replies arrive through the mailbox (A: synchronous).
    fn background(
        &mut self,
        batches: &[PluginBatch],
        rows: &FrozenRowsFactory<'_>,
        writer_mailbox: &WriterMailbox,
    );

    fn cancel(&mut self, plugin: PluginId, job: JobId);

    fn shutdown(&mut self);

    /// Name and configuration for `meta.json` (T, spin, wait mechanism, slack, ...).
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    /// Timer slack in ns of every thread that can run or wake a bounded wait (writer excluded;
    /// the harness checks the writer itself). B' gating runs require all of them to read 1.
    fn thread_timer_slacks(&self) -> Vec<u64> {
        Vec::new()
    }

    /// Mailbox depths of the plugin actors (M6 sampling). Empty for inline arms.
    fn plugin_queue_depths(&self) -> Vec<usize> {
        Vec::new()
    }
}

/// Plugins off: nothing to hand off. The baseline of every "plugins-off" leg.
#[derive(Default)]
pub struct NoopRunner;

impl WLaneRunner for NoopRunner {
    fn interactive(
        &mut self,
        _t: &TurnCtx,
        b: &[PluginBatch],
        _r: &FrozenRowsFactory<'_>,
        _m: &WriterMailbox,
    ) -> HandoffOutcome {
        HandoffOutcome {
            asked: b.len() as u32,
            ..HandoffOutcome::default()
        }
    }
    fn background(&mut self, _b: &[PluginBatch], _r: &FrozenRowsFactory<'_>, _m: &WriterMailbox) {}
    fn cancel(&mut self, _p: PluginId, _j: JobId) {}
    fn shutdown(&mut self) {}
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"runner": "noop"})
    }
}

/// Trivial reference: runs every call inline, one after another, on the writer thread, no policy.
/// Exists to prove the harness; the real arm A adds nothing the writer does not already do, but
/// candidates are judged on their own crates, not on this one.
pub struct RefInlineRunner {
    rts: Vec<PluginRuntime>,
}

impl RefInlineRunner {
    pub fn new(rts: Vec<PluginRuntime>) -> RefInlineRunner {
        RefInlineRunner { rts }
    }

    fn rt(&mut self, p: PluginId) -> Option<&mut PluginRuntime> {
        self.rts.iter_mut().find(|r| r.plugin == p)
    }

    pub fn run_all(
        &mut self,
        batches: &[PluginBatch],
        rows: &FrozenRowsFactory<'_>,
        mb: &WriterMailbox,
    ) -> u32 {
        let mut n = 0;
        for b in batches {
            let fr = rows.build(&b.nodes);
            if let Some(rt) = self.rt(b.plugin) {
                mb.send(rt.run(b, fr));
                n += 1;
            }
        }
        n
    }
}

impl WLaneRunner for RefInlineRunner {
    fn interactive(
        &mut self,
        _t: &TurnCtx,
        b: &[PluginBatch],
        r: &FrozenRowsFactory<'_>,
        m: &WriterMailbox,
    ) -> HandoffOutcome {
        let s = now_ns();
        let arrived = self.run_all(b, r, m);
        HandoffOutcome {
            arrived,
            asked: b.len() as u32,
            waited: Duration::from_nanos(now_ns() - s),
            ..HandoffOutcome::default()
        }
    }
    fn background(&mut self, b: &[PluginBatch], r: &FrozenRowsFactory<'_>, m: &WriterMailbox) {
        self.run_all(b, r, m);
    }
    fn cancel(&mut self, _p: PluginId, _j: JobId) {}
    fn shutdown(&mut self) {}
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"runner": "ref-inline", "note": "harness reference, not a candidate"})
    }
}

/// Self-test runner: sleeps `delay_us` before running the calls inline. Must fail M1.
pub struct SlowRunner {
    pub inner: RefInlineRunner,
    pub delay_us: u64,
}

impl WLaneRunner for SlowRunner {
    fn interactive(
        &mut self,
        t: &TurnCtx,
        b: &[PluginBatch],
        r: &FrozenRowsFactory<'_>,
        m: &WriterMailbox,
    ) -> HandoffOutcome {
        std::thread::sleep(Duration::from_micros(self.delay_us));
        self.inner.interactive(t, b, r, m)
    }
    fn background(&mut self, b: &[PluginBatch], r: &FrozenRowsFactory<'_>, m: &WriterMailbox) {
        self.inner.background(b, r, m)
    }
    fn cancel(&mut self, p: PluginId, j: JobId) {
        self.inner.cancel(p, j)
    }
    fn shutdown(&mut self) {}
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"runner": "self-test-slow", "delay_us": self.delay_us})
    }
}

/// Self-test runner: pretends to wait for the plugins but ignores the deadline: it spins until
/// `deadline + overshoot_us` (a runner whose wait is unbounded by `D`). Must fail M1.
pub struct IgnoreDeadlineRunner {
    pub inner: RefInlineRunner,
    pub overshoot_us: u64,
}

impl WLaneRunner for IgnoreDeadlineRunner {
    fn interactive(
        &mut self,
        t: &TurnCtx,
        b: &[PluginBatch],
        r: &FrozenRowsFactory<'_>,
        m: &WriterMailbox,
    ) -> HandoffOutcome {
        let o = self.inner.interactive(t, b, r, m);
        crate::timing::wait_until(t.deadline_ns + self.overshoot_us * 1000, 50_000);
        o
    }
    fn background(&mut self, b: &[PluginBatch], r: &FrozenRowsFactory<'_>, m: &WriterMailbox) {
        self.inner.background(b, r, m)
    }
    fn cancel(&mut self, p: PluginId, j: JobId) {
        self.inner.cancel(p, j)
    }
    fn shutdown(&mut self) {}
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"runner": "self-test-ignore-deadline", "overshoot_us": self.overshoot_us})
    }
}

/// Self-test runner: after each real reply it injects a copy of a `Done` with a stale
/// `slot_epoch`; the writer must drop and count it (and never admit its facts).
pub struct StaleEpochInjector {
    pub inner: RefInlineRunner,
}

impl WLaneRunner for StaleEpochInjector {
    fn interactive(
        &mut self,
        t: &TurnCtx,
        b: &[PluginBatch],
        r: &FrozenRowsFactory<'_>,
        m: &WriterMailbox,
    ) -> HandoffOutcome {
        for x in b {
            m.send(PluginReply::Done(DoneReply {
                plugin: x.plugin,
                job: x.job,
                slot_epoch: x.slot_epoch.wrapping_sub(1),
                interactive: true,
                grant: x.grant,
                token: Default::default(),
                facts: vec![crate::host::FactEffect {
                    node: x.nodes.first().copied().unwrap_or(0),
                    group: 100,
                    value: crate::host::Value::Number(666),
                    conf: 900,
                    inferred: true,
                }],
                refreshes: vec![],
                exec_ns: 1,
                queued_ns: 0,
                fuel_used: 1,
                wall_overrun: false,
                imports: 0,
                dropped: 0,
                forged: 0,
            }));
        }
        self.inner.interactive(t, b, r, m)
    }
    fn background(&mut self, b: &[PluginBatch], r: &FrozenRowsFactory<'_>, m: &WriterMailbox) {
        self.inner.background(b, r, m)
    }
    fn cancel(&mut self, p: PluginId, j: JobId) {
        self.inner.cancel(p, j)
    }
    fn shutdown(&mut self) {}
    fn describe(&self) -> serde_json::Value {
        serde_json::json!({"runner": "self-test-stale-epoch"})
    }
}

/// Convenience for tests: a fault reply with no execution.
pub fn fault_reply(b: &PluginBatch, kind: FaultKind) -> PluginReply {
    PluginReply::Fault(crate::host::FaultReply {
        plugin: b.plugin,
        job: b.job,
        slot_epoch: b.slot_epoch,
        interactive: b.interactive,
        grant: b.grant,
        kind,
        exec_ns: 0,
        queued_ns: 0,
        fuel_used: 0,
    })
}
