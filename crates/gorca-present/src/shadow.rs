// SPDX-License-Identifier: GPL-3.0-or-later
//! Shadow mode (ADR-0022 section 2.11): the presenter actor.
//!
//! The Reader offers a [`FocusSample`] (the fused entry and the tokens the legacy presenter just
//! produced) through a bounded channel with `try_send`; it never waits. This actor composes the
//! IR, renders it, diffs the tokens against the legacy ones and counts the outcome. It speaks
//! nothing and touches no state of the Reader (R3: results are counters and a debug log).
//!
//! Ownership: [`ShadowActor::run`] is a future the owner spawns into its own `JoinSet` and ends
//! by cancelling the token. The loop awaits only the channel and the token (no IPC), so no
//! deadline is needed; a sample costs one compose, one render and one diff.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gorca_model::sem::SemEntry;
use gorca_model::{NodeKey, SegmentKind};
use smallvec::SmallVec;
use tokio::sync::mpsc::{self, error::TrySendError};
use tokio_util::sync::CancellationToken;

use crate::change::compose_change;
use crate::compose::{ComposeCtx, ReportFlags, compose};
use crate::deviations::Deviations;
use crate::diff::{DiffCtx, DiffOutcome, LegacyToken, diff};
use crate::ir::Reason;
use crate::render::{RenderOpts, render};

/// Queue depth between the Reader and the actor. A full queue drops the sample (counted).
pub const SHADOW_QUEUE: usize = 64;

/// One focus utterance of the legacy presenter, with the entry it should have been made from.
#[derive(Clone, Debug)]
pub struct FocusSample {
    pub node: NodeKey,
    pub entry: Arc<SemEntry>,
    /// The tokens of the legacy presenter's utterance, before policy and lead-in edits.
    pub legacy: SmallVec<[LegacyToken; 6]>,
    pub may_refine: bool,
    /// The node holds secret text: never log its tokens.
    pub secret: bool,
    /// `browse.description_pause` as the legacy presenter saw it.
    pub description_pause: bool,
    /// The node is web content (the legacy web presenter produced the tokens).
    pub web: bool,
}

/// One state change of the focused node: the entry that was spoken, the new one, and the tokens
/// the legacy change presenter produced.
#[derive(Clone, Debug)]
pub struct ChangeSample {
    pub node: NodeKey,
    pub old: Arc<SemEntry>,
    pub new: Arc<SemEntry>,
    pub legacy: SmallVec<[LegacyToken; 6]>,
    pub skip_selected: bool,
    pub secret: bool,
    pub web: bool,
}

/// What travels to the actor.
#[derive(Clone, Debug)]
pub enum ShadowMsg {
    Focus(FocusSample),
    Change(ChangeSample),
}

/// Why the Reader did not offer a sample.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SkipWhy {
    /// The snapshot carries no fused entry yet.
    NoEntry,
    /// Web content: the web presenter is a later slice.
    Web,
    /// A switcher selection or another non-focus context.
    Context,
}

/// Counters of the shadow run. All relaxed: they are diagnostics, not synchronisation.
#[derive(Debug, Default)]
pub struct ShadowStats {
    offered: AtomicU64,
    dropped_full: AtomicU64,
    dropped_closed: AtomicU64,
    skipped_no_entry: AtomicU64,
    skipped_web: AtomicU64,
    skipped_context: AtomicU64,
    equal: AtomicU64,
    order_only: AtomicU64,
    allowed: AtomicU64,
    different: AtomicU64,
    unsupported: AtomicU64,
    change_equal: AtomicU64,
    change_allowed: AtomicU64,
    change_different: AtomicU64,
}

/// A copy of the counters.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ShadowSnapshot {
    pub offered: u64,
    pub dropped_full: u64,
    pub dropped_closed: u64,
    pub skipped_no_entry: u64,
    pub skipped_web: u64,
    pub skipped_context: u64,
    pub equal: u64,
    pub order_only: u64,
    pub allowed: u64,
    pub different: u64,
    pub unsupported: u64,
    pub change_equal: u64,
    pub change_allowed: u64,
    pub change_different: u64,
}

impl ShadowSnapshot {
    /// Samples the actor has compared.
    pub fn compared(&self) -> u64 {
        self.equal + self.order_only + self.allowed + self.different
    }

    /// State changes compared (focus samples are counted by [`ShadowSnapshot::compared`]).
    pub fn change_compared(&self) -> u64 {
        self.change_equal + self.change_allowed + self.change_different
    }

    /// Differences outside `deviations.toml`, as a fraction of the compared samples.
    pub fn different_ratio(&self) -> f64 {
        let c = self.compared();
        if c == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)] // diagnostic ratio
            {
                self.different as f64 / c as f64
            }
        }
    }
}

fn bump(c: &AtomicU64) {
    c.fetch_add(1, Ordering::Relaxed);
}

impl ShadowStats {
    pub fn snapshot(&self) -> ShadowSnapshot {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed);
        ShadowSnapshot {
            offered: g(&self.offered),
            dropped_full: g(&self.dropped_full),
            dropped_closed: g(&self.dropped_closed),
            skipped_no_entry: g(&self.skipped_no_entry),
            skipped_web: g(&self.skipped_web),
            skipped_context: g(&self.skipped_context),
            equal: g(&self.equal),
            order_only: g(&self.order_only),
            allowed: g(&self.allowed),
            different: g(&self.different),
            unsupported: g(&self.unsupported),
            change_equal: g(&self.change_equal),
            change_allowed: g(&self.change_allowed),
            change_different: g(&self.change_different),
        }
    }
}

/// The Reader's end: cheap to clone, never blocks.
#[derive(Clone, Debug)]
pub struct ShadowHandle {
    tx: mpsc::Sender<ShadowMsg>,
    stats: Arc<ShadowStats>,
}

impl ShadowHandle {
    /// Offer a sample; a full or closed queue drops it and counts that.
    pub fn offer(&self, sample: FocusSample) {
        self.send(ShadowMsg::Focus(sample));
    }

    /// Offer a state-change sample (same drop rules).
    pub fn offer_change(&self, sample: ChangeSample) {
        self.send(ShadowMsg::Change(sample));
    }

    fn send(&self, msg: ShadowMsg) {
        bump(&self.stats.offered);
        match self.tx.try_send(msg) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => bump(&self.stats.dropped_full),
            Err(TrySendError::Closed(_)) => bump(&self.stats.dropped_closed),
        }
    }

    /// Record a focus the Reader did not offer.
    pub fn skipped(&self, why: SkipWhy) {
        bump(match why {
            SkipWhy::NoEntry => &self.stats.skipped_no_entry,
            SkipWhy::Web => &self.stats.skipped_web,
            SkipWhy::Context => &self.stats.skipped_context,
        });
    }

    pub fn stats(&self) -> ShadowSnapshot {
        self.stats.snapshot()
    }
}

/// The actor: owns the receiving end and the deviation list.
#[derive(Debug)]
pub struct ShadowActor {
    rx: mpsc::Receiver<ShadowMsg>,
    stats: Arc<ShadowStats>,
    deviations: Deviations,
}

/// A handle and the actor behind it, with the default queue depth.
pub fn channel(deviations: Deviations) -> (ShadowHandle, ShadowActor) {
    channel_with(SHADOW_QUEUE, deviations)
}

/// As [`channel`] with an explicit queue depth (at least 1).
pub fn channel_with(depth: usize, deviations: Deviations) -> (ShadowHandle, ShadowActor) {
    let (tx, rx) = mpsc::channel(depth.max(1));
    let stats = Arc::new(ShadowStats::default());
    (
        ShadowHandle {
            tx,
            stats: Arc::clone(&stats),
        },
        ShadowActor {
            rx,
            stats,
            deviations,
        },
    )
}

impl ShadowActor {
    /// Process one sample: compose, render, diff, count, log.
    pub fn process(&self, s: &FocusSample) -> Option<DiffOutcome> {
        let ctx = ComposeCtx {
            flags: ReportFlags::default(),
            may_refine: s.may_refine,
            web: s.web,
            skip_selected: false,
        };
        let ir = match compose(&s.entry, s.node, Reason::Focus, ctx) {
            Ok(ir) => ir,
            Err(e) => {
                bump(&self.stats.unsupported);
                tracing::debug!(target: "gorca_present::shadow", node = ?s.node, error = %e, "no rule row");
                return None;
            }
        };
        let tokens = render(
            &ir,
            RenderOpts {
                description_pause: s.description_pause,
            },
        );
        let outcome = diff(
            &s.legacy,
            &tokens,
            &self.deviations,
            DiffCtx::for_entry(if s.web { "focus-web" } else { "focus" }, &s.entry),
        );
        match &outcome {
            DiffOutcome::Equal => bump(&self.stats.equal),
            DiffOutcome::OrderOnly => bump(&self.stats.order_only),
            DiffOutcome::Allowed(_) => bump(&self.stats.allowed),
            DiffOutcome::Different { .. } => bump(&self.stats.different),
        }
        if matches!(
            outcome,
            DiffOutcome::OrderOnly | DiffOutcome::Different { .. }
        ) {
            log_difference(s, &outcome, &tokens);
        }
        Some(outcome)
    }

    /// Process one change sample: compose the change, render, diff, count.
    pub fn process_change(&self, s: &ChangeSample) -> DiffOutcome {
        let ctx = ComposeCtx {
            flags: ReportFlags::default(),
            may_refine: false,
            web: s.web,
            skip_selected: s.skip_selected,
        };
        let ir = compose_change(&s.old, &s.new, s.node, ctx);
        let tokens = render(
            &ir,
            RenderOpts {
                description_pause: false,
            },
        );
        let surface = if s.web { "change-web" } else { "change" };
        let outcome = diff(
            &s.legacy,
            &tokens,
            &self.deviations,
            DiffCtx::for_entry(surface, &s.new),
        );
        match &outcome {
            DiffOutcome::Equal | DiffOutcome::OrderOnly => bump(&self.stats.change_equal),
            DiffOutcome::Allowed(_) => bump(&self.stats.change_allowed),
            DiffOutcome::Different { .. } => {
                bump(&self.stats.change_different);
                if !s.secret {
                    tracing::debug!(target: "gorca_present::shadow", node = ?s.node, ?outcome,
                        legacy = ?s.legacy.iter().map(|t| &*t.text).collect::<Vec<_>>(),
                        new = ?tokens.iter().map(|t| t.text.as_str()).collect::<Vec<_>>(),
                        "shadow change diff");
                }
            }
        }
        outcome
    }

    fn handle(&self, m: &ShadowMsg) {
        match m {
            ShadowMsg::Focus(s) => {
                self.process(s);
            }
            ShadowMsg::Change(s) => {
                self.process_change(s);
            }
        }
    }

    /// Process every sample already queued, without waiting; returns how many. For tests and
    /// for an owner that wants a final flush before it reads the counters.
    pub fn drain_pending(&mut self) -> usize {
        let mut n = 0;
        while let Ok(m) = self.rx.try_recv() {
            self.handle(&m);
            n += 1;
        }
        n
    }

    /// Run until `cancel` fires or every handle is dropped.
    pub async fn run(mut self, cancel: CancellationToken) {
        loop {
            tokio::select! {
                () = cancel.cancelled() => break,
                got = self.rx.recv() => match got {
                    Some(m) => self.handle(&m),
                    None => break,
                },
            }
        }
    }
}

fn log_difference(s: &FocusSample, outcome: &DiffOutcome, new: &[crate::render::Token]) {
    if s.secret {
        // Only shapes: a password field's tokens are never logged.
        tracing::debug!(target: "gorca_present::shadow", node = ?s.node, ?outcome,
            legacy_tokens = s.legacy.len(), new_tokens = new.len(), "shadow diff (secret node)");
        return;
    }
    let kinds = |it: &mut dyn Iterator<Item = (SegmentKind, &str)>| {
        it.map(|(k, t)| format!("{k:?}:{t:?}"))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    tracing::debug!(target: "gorca_present::shadow", node = ?s.node, ?outcome,
        legacy = %kinds(&mut s.legacy.iter().map(|t| (t.kind, &*t.text))),
        new = %kinds(&mut new.iter().map(|t| (t.kind, t.text.as_str()))),
        "shadow diff");
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gorca_model::sem::{Check, SemRole};
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(2),
        }
    }

    fn entry(role: SemRole, name: &str, check: Check) -> Arc<SemEntry> {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.sem.check = check;
        e.name = Some(Arc::from(name));
        Arc::new(e)
    }

    fn tok(kind: SegmentKind, s: &str) -> LegacyToken {
        LegacyToken {
            kind,
            text: Arc::from(s),
        }
    }

    fn sample(e: Arc<SemEntry>, legacy: Vec<LegacyToken>) -> FocusSample {
        FocusSample {
            node: node(),
            entry: e,
            legacy: legacy.into(),
            may_refine: false,
            secret: false,
            description_pause: false,
            web: false,
        }
    }

    #[test]
    fn matching_tokens_count_equal_and_a_missing_one_counts_different() {
        let (h, a) = channel(Deviations::builtin());
        let e = entry(SemRole::Button, "OK", Check::NotCheckable);
        let role = gorca_model::Role::Button.spoken();
        let ok = sample(
            Arc::clone(&e),
            vec![tok(SegmentKind::Text, "OK"), tok(SegmentKind::Role, role)],
        );
        assert_eq!(a.process(&ok), Some(DiffOutcome::Equal));
        let bad = sample(
            e,
            vec![tok(SegmentKind::Text, "OK"), tok(SegmentKind::Role, "link")],
        );
        assert!(matches!(
            a.process(&bad),
            Some(DiffOutcome::Different { .. })
        ));
        let s = h.stats();
        assert_eq!((s.equal, s.different, s.compared()), (1, 1, 2));
        assert!((s.different_ratio() - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_full_queue_drops_and_never_blocks() {
        let (h, _a) = channel_with(2, Deviations::empty());
        let e = entry(SemRole::Button, "OK", Check::NotCheckable);
        for _ in 0..5 {
            h.offer(sample(Arc::clone(&e), vec![]));
        }
        let s = h.stats();
        assert_eq!((s.offered, s.dropped_full), (5, 3));
    }

    #[test]
    fn a_closed_queue_counts_closed() {
        let (h, a) = channel(Deviations::empty());
        drop(a);
        h.offer(sample(
            entry(SemRole::Button, "OK", Check::NotCheckable),
            vec![],
        ));
        assert_eq!(h.stats().dropped_closed, 1);
    }

    #[test]
    fn skips_are_counted_by_reason() {
        let (h, _a) = channel(Deviations::empty());
        h.skipped(SkipWhy::NoEntry);
        h.skipped(SkipWhy::Web);
        h.skipped(SkipWhy::Web);
        h.skipped(SkipWhy::Context);
        let s = h.stats();
        assert_eq!(
            (s.skipped_no_entry, s.skipped_web, s.skipped_context),
            (1, 2, 1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn the_actor_drains_the_queue_and_stops_on_cancel() {
        let (h, a) = channel(Deviations::builtin());
        let cancel = CancellationToken::new();
        let task = tokio::spawn(a.run(cancel.clone()));
        let e = entry(SemRole::Checkbox, "Mute", Check::On);
        let role = gorca_model::Role::CheckBox.spoken();
        let checked = gorca_i18n::t(gorca_i18n::MsgId::StateChecked);
        h.offer(sample(
            e,
            vec![
                tok(SegmentKind::Text, "Mute"),
                tok(SegmentKind::Role, role),
                tok(SegmentKind::State, checked),
            ],
        ));
        for _ in 0..100 {
            if h.stats().equal == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        assert_eq!(h.stats().equal, 1);
        cancel.cancel();
        assert!(task.await.is_ok(), "the actor ends without panicking");
    }

    /// R-1: the actor runs on the multi-thread pool, off the producer's thread. The producer
    /// here is a plain thread standing in for the Reader lane; the offer cost is what it pays.
    #[test]
    #[allow(clippy::disallowed_methods)] // a plain thread stands for the Reader lane
    fn the_actor_on_the_pool_keeps_up_and_costs_the_producer_microseconds() {
        const N: u32 = 20_000;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .expect("runtime");
        let (h, a) = channel(Deviations::builtin());
        let cancel = CancellationToken::new();
        let task = rt.spawn(a.run(cancel.clone()));
        let e = entry(SemRole::Checkbox, "Remember this choice", Check::On);
        let role = gorca_model::Role::CheckBox.spoken();
        let checked = gorca_i18n::t(gorca_i18n::MsgId::StateChecked);
        let legacy = vec![
            tok(SegmentKind::Text, "Remember this choice"),
            tok(SegmentKind::Role, role),
            tok(SegmentKind::State, checked),
        ];
        let mut worst = std::time::Duration::ZERO;
        let mut total = std::time::Duration::ZERO;
        let start = std::time::Instant::now();
        for i in 0..N {
            let s = sample(Arc::clone(&e), legacy.clone());
            let t = std::time::Instant::now();
            h.offer(s);
            let d = t.elapsed();
            total += d;
            worst = worst.max(d);
            // A burst of 64 then a pause: the 1 kHz focus rate of a hostile app is far above use.
            if i % 64 == 63 {
                std::thread::sleep(Duration::from_micros(200));
            }
        }
        let produced = start.elapsed();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while {
            let s = h.stats();
            s.compared() + s.dropped_full < u64::from(N) && std::time::Instant::now() < deadline
        } {
            std::thread::sleep(Duration::from_millis(1));
        }
        let s = h.stats();
        eprintln!(
            "shadow on the pool: offer mean {:?}, worst {worst:?}, produced {N} in {produced:?}, \
             compared {}, dropped {}",
            total / N,
            s.compared(),
            s.dropped_full
        );
        assert_eq!(s.compared() + s.dropped_full, u64::from(N));
        assert_eq!(s.different, 0);
        assert!(total / N < Duration::from_micros(50), "{:?}", total / N);
        cancel.cancel();
        assert!(rt.block_on(task).is_ok());
    }

    #[tokio::test]
    async fn the_actor_ends_when_every_handle_is_gone() {
        let (h, a) = channel(Deviations::empty());
        let task = tokio::spawn(a.run(CancellationToken::new()));
        drop(h);
        assert!(task.await.is_ok());
    }
}
