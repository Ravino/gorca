// SPDX-License-Identifier: GPL-3.0-or-later
//! Mirror freshness (CHR-0, `docs/reports/chromium-base-design-review.md`).
//!
//! A Chromium page that is still loading sends no `children-changed:add` (the engine suppresses
//! them until the load ends), and it sends no structural event when the load completes. A mirror
//! built in that window stays at its first size until something re-fetches it. The document
//! actor therefore *repairs* the mirror on exact signals, and meters the repairs:
//!
//! * `load-complete` (or the busy to idle flip of the root) of a document that has a mirror,
//! * a focus that the join misses inside a known document,
//! * events on nodes the mirror does not know that are still unknown after a settle period,
//! * three young-document checks for a mirror built tiny and never told that the load ended.
//!
//! [`RepairGate`] is the pure part: one repair in flight per document, a minimum gap that grows
//! with the cost of the last repair (a repair never takes more than about 1/9 of the browser's
//! UI thread) and doubles after every repair that found nothing to change, so an unfixable
//! orphan cannot make the reader hammer the browser. The gate runs inside the actor, off every
//! key path; the Reader never waits for it.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gorca_doc_source_api::RepairReport;
use gorca_model::{NodeKey, WebEngine};

/// Why a repair was asked for. Higher priority wins when requests coalesce.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum RepairReason {
    /// Events on nodes the mirror did not know, still unknown after the settle period.
    Orphans,
    /// The focus is inside this document but the join missed it.
    FocusMiss,
    /// One of the young-document checks.
    Young,
    /// The root's states changed (busy to idle) while the document is young and no load end
    /// was reported.
    RootState,
    /// `load-complete` / `reload`, or the root's busy flag flipping while the document is young.
    LoadComplete,
}

impl RepairReason {
    /// A non-zero code for the router's coalescing flag (0 means no request pending).
    pub fn code(self) -> u8 {
        match self {
            RepairReason::Orphans => 1,
            RepairReason::FocusMiss => 2,
            RepairReason::Young => 3,
            RepairReason::RootState => 4,
            RepairReason::LoadComplete => 5,
        }
    }

    pub fn from_code(c: u8) -> Option<Self> {
        match c {
            1 => Some(RepairReason::Orphans),
            2 => Some(RepairReason::FocusMiss),
            3 => Some(RepairReason::Young),
            4 => Some(RepairReason::RootState),
            5 => Some(RepairReason::LoadComplete),
            _ => None,
        }
    }

    /// Guesses may be answered by a cheap look before the refetch; the exact signals are not.
    pub fn cheap_check(self) -> bool {
        matches!(self, RepairReason::Orphans | RepairReason::RootState)
    }

    /// Label of the phase log.
    pub fn label(self) -> &'static str {
        match self {
            RepairReason::Orphans => "orphans",
            RepairReason::FocusMiss => "focus_miss",
            RepairReason::Young => "young",
            RepairReason::RootState => "root_state",
            RepairReason::LoadComplete => "load_complete",
        }
    }
}

/// Which triggers an engine's documents use. Data selected by the engine class, never by an
/// application name.
#[allow(clippy::struct_excessive_bools)] // one flag per trigger, read as a table
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct RepairPolicy {
    pub load_complete: bool,
    pub focus_miss: bool,
    pub orphans: bool,
    pub young: bool,
}

impl RepairPolicy {
    pub const NONE: RepairPolicy = RepairPolicy {
        load_complete: false,
        focus_miss: false,
        orphans: false,
        young: false,
    };

    /// Every trigger.
    pub const ALL: RepairPolicy = RepairPolicy {
        load_complete: true,
        focus_miss: true,
        orphans: true,
        young: true,
    };

    /// Gecko sends the structural events a mirror needs, so its documents never repair: a
    /// whole-document refetch there only delays the incremental update that would join a focus
    /// (review F-1). Blink suppresses the events during a load, so every trigger is on.
    pub fn for_engine(e: WebEngine) -> Self {
        match e {
            WebEngine::Blink => Self::ALL,
            WebEngine::Gecko | WebEngine::None => Self::NONE,
        }
    }

    fn allows(self, r: RepairReason) -> bool {
        match r {
            RepairReason::LoadComplete | RepairReason::RootState => self.load_complete,
            RepairReason::FocusMiss => self.focus_miss,
            RepairReason::Orphans => self.orphans,
            RepairReason::Young => self.young,
        }
    }
}

/// A repair costs at most `1 / (COST_FACTOR + 1)` of the browser's time.
const COST_FACTOR: u32 = 8;
const MAX_GAP: Duration = Duration::from_secs(60);
const FOCUS_MISS_GAP: Duration = Duration::from_millis(400);
const ORPHAN_GAP: Duration = Duration::from_secs(1);

/// What the gate says about a request.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    /// Send the repair now.
    Start,
    /// Not before this instant; the caller keeps the request (see [`RepairGate::keep`]).
    Wait(Instant),
    /// A repair is in flight: the request was folded into the follow-up.
    Queued,
    /// This engine does not use that trigger.
    Refused,
}

#[derive(Debug)]
pub struct RepairGate {
    policy: RepairPolicy,
    in_flight: bool,
    queued: Option<RepairReason>,
    last_end: Option<Instant>,
    last_cost: Duration,
    /// Repairs in a row that changed nothing, per reason (index = `code() - 1`): fruitless orphan
    /// guesses must not delay a real focus-miss repair (review F-5).
    quiet: [u32; 5],
    current: Option<RepairReason>,
    loaded_seen: bool,
}

fn slot(r: RepairReason) -> usize {
    usize::from(r.code() - 1)
}

impl RepairGate {
    pub fn new(policy: RepairPolicy) -> Self {
        Self {
            policy,
            in_flight: false,
            queued: None,
            last_end: None,
            last_cost: Duration::ZERO,
            quiet: [0; 5],
            current: None,
            loaded_seen: false,
        }
    }

    pub fn policy(&self) -> RepairPolicy {
        self.policy
    }

    /// The browser reported the end of a load for this document.
    pub fn loaded_seen(&self) -> bool {
        self.loaded_seen
    }

    pub fn in_flight(&self) -> bool {
        self.in_flight
    }

    /// The smallest wait after the last repair for a request of this kind.
    fn gap(&self, r: RepairReason) -> Duration {
        let exact = matches!(r, RepairReason::LoadComplete | RepairReason::Young);
        let base = match r {
            RepairReason::LoadComplete | RepairReason::Young => Duration::ZERO,
            RepairReason::RootState | RepairReason::FocusMiss => FOCUS_MISS_GAP,
            RepairReason::Orphans => ORPHAN_GAP,
        };
        // An exact signal (the load ended) is not a guess, so a run of fruitless guesses does not
        // delay it; every kind pays for the cost of the last repair.
        let backoff = if exact {
            1
        } else {
            1u32 << self.quiet[slot(r)].min(6)
        };
        (base * backoff)
            .max(self.last_cost * COST_FACTOR)
            .min(MAX_GAP)
    }

    /// A request. `Start` means the caller sends the repair and calls [`Self::started`].
    pub fn request(&mut self, r: RepairReason, now: Instant) -> Verdict {
        if !self.policy.allows(r) {
            return Verdict::Refused;
        }
        if r == RepairReason::LoadComplete {
            self.loaded_seen = true;
        }
        if self.in_flight {
            self.queued = Some(self.queued.map_or(r, |q| q.max(r)));
            return Verdict::Queued;
        }
        let ready = self.last_end.map_or(now, |t| t + self.gap(r));
        if now >= ready {
            Verdict::Start
        } else {
            Verdict::Wait(ready)
        }
    }

    pub fn started(&mut self, r: RepairReason) {
        self.in_flight = true;
        self.current = Some(r);
    }

    /// The repair ended; `changed` says whether it changed the mirror (a refetch pushes its
    /// whole difference as deltas whether or not anything differs, so the actor compares the
    /// mirror before and after). Returns the reason of the request that arrived meanwhile.
    pub fn finished(
        &mut self,
        report: RepairReport,
        changed: bool,
        now: Instant,
    ) -> Option<RepairReason> {
        self.in_flight = false;
        self.last_end = Some(now);
        self.last_cost = report.took;
        if report.failed || !changed {
            if let Some(r) = self.current {
                let q = &mut self.quiet[slot(r)];
                *q = q.saturating_add(1);
            }
        } else {
            self.quiet = [0; 5];
        }
        self.current = None;
        self.queued.take()
    }

    /// The request could not be sent (the source's queue was full): count it as a fruitless run
    /// so the back-off applies.
    pub fn aborted(&mut self, now: Instant) -> Option<RepairReason> {
        self.finished(
            RepairReport {
                failed: true,
                ..RepairReport::default()
            },
            false,
            now,
        )
    }

    /// Fruitless repairs in a row, all reasons (for the log).
    pub fn quiet(&self) -> u32 {
        self.quiet.iter().sum()
    }
}

/// Young-document checks: after the first publish a mirror of fewer nodes than
/// [`YOUNG_NODES`] gets a check at each of these ages unless the load has been reported.
pub const YOUNG_CHECKS: [Duration; 3] = [
    Duration::from_secs(2),
    Duration::from_secs(6),
    Duration::from_secs(16),
];
pub const YOUNG_NODES: usize = 32;
/// Events on unknown nodes wait this long before they count (the incremental update that adds
/// the node is normally faster).
pub const ORPHAN_SETTLE: Duration = Duration::from_millis(750);

/// Events the router saw on nodes no join knows, per document. The router (hot path) only bumps
/// a counter and keeps a few keys; the document actor reads them on its tick.
#[derive(Debug, Default)]
pub struct Orphans {
    count: AtomicU32,
    sample: Mutex<Vec<NodeKey>>,
}

const ORPHAN_SAMPLE: usize = 16;

impl Orphans {
    /// Wait-free: a contended or full sample is skipped, the count is always bumped.
    pub fn note(&self, k: NodeKey) {
        self.count.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut s) = self.sample.try_lock()
            && s.len() < ORPHAN_SAMPLE
            && !s.contains(&k)
        {
            s.push(k);
        }
    }

    pub fn pending(&self) -> bool {
        self.count.load(Ordering::Relaxed) > 0
    }

    /// Take the sampled keys and reset the count.
    pub fn take(&self) -> Vec<NodeKey> {
        self.count.store(0, Ordering::Relaxed);
        self.sample
            .lock()
            .map(|mut s| std::mem::take(&mut *s))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rep(deltas: usize, took_ms: u64) -> RepairReport {
        RepairReport {
            nodes: 100,
            deltas,
            took: Duration::from_millis(took_ms),
            failed: false,
        }
    }

    #[test]
    fn a_gecko_document_never_repairs() {
        let mut g = RepairGate::new(RepairPolicy::for_engine(WebEngine::Gecko));
        let t = Instant::now();
        for r in [
            RepairReason::LoadComplete,
            RepairReason::Orphans,
            RepairReason::FocusMiss,
            RepairReason::Young,
            RepairReason::RootState,
        ] {
            assert_eq!(g.request(r, t), Verdict::Refused, "{r:?}");
        }
        assert!(!g.loaded_seen());
    }

    #[test]
    fn fruitless_orphan_guesses_do_not_delay_a_focus_miss() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let mut t = Instant::now();
        for _ in 0..8 {
            g.started(RepairReason::Orphans);
            g.finished(rep(0, 1), false, t);
            t += Duration::from_secs(60);
        }
        // Eight fruitless orphan repairs later a focus miss waits only for the cost gap.
        g.started(RepairReason::Orphans);
        g.finished(rep(0, 1), false, t);
        assert_eq!(
            g.request(RepairReason::FocusMiss, t),
            Verdict::Wait(t + Duration::from_millis(400)),
        );
    }

    #[test]
    fn one_repair_in_flight_and_the_rest_fold_into_one_follow_up() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let t = Instant::now();
        assert_eq!(g.request(RepairReason::FocusMiss, t), Verdict::Start);
        g.started(RepairReason::FocusMiss);
        assert_eq!(g.request(RepairReason::Orphans, t), Verdict::Queued);
        assert_eq!(g.request(RepairReason::LoadComplete, t), Verdict::Queued);
        assert_eq!(g.request(RepairReason::FocusMiss, t), Verdict::Queued);
        assert_eq!(
            g.finished(rep(3, 10), true, t),
            Some(RepairReason::LoadComplete),
            "the strongest reason survives"
        );
        assert!(!g.in_flight());
    }

    #[test]
    fn the_gap_follows_the_cost_of_the_last_repair() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let t = Instant::now();
        g.started(RepairReason::FocusMiss);
        g.finished(rep(5, 400), true, t);
        // 400 ms of repair buys 3.2 s of quiet for the guesses and for the exact signal alike.
        let want = t + Duration::from_millis(3200);
        assert_eq!(g.request(RepairReason::FocusMiss, t), Verdict::Wait(want));
        assert_eq!(
            g.request(RepairReason::LoadComplete, t),
            Verdict::Wait(want)
        );
        assert_eq!(g.request(RepairReason::FocusMiss, want), Verdict::Start);
    }

    #[test]
    fn fruitless_repairs_back_off_guesses_but_not_the_load_signal() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let mut t = Instant::now();
        let mut last_gap = Duration::ZERO;
        for _ in 0..4 {
            g.started(RepairReason::Orphans);
            g.finished(rep(0, 1), false, t);
            let Verdict::Wait(at) = g.request(RepairReason::Orphans, t) else {
                panic!("a guess right after a fruitless repair must wait");
            };
            let gap = at - t;
            assert!(gap > last_gap, "the gap grows: {gap:?} after {last_gap:?}");
            last_gap = gap;
            assert_eq!(
                g.request(RepairReason::LoadComplete, t),
                Verdict::Wait(t + Duration::from_millis(8)),
                "the load signal pays for cost only"
            );
            t = at;
        }
        // A repair that changed something resets the guess back-off.
        g.started(RepairReason::FocusMiss);
        g.finished(rep(2, 1), true, t);
        assert_eq!(g.quiet(), 0);
    }

    #[test]
    fn the_gap_is_capped() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let t = Instant::now();
        g.started(RepairReason::FocusMiss);
        g.finished(rep(1, 60_000), true, t);
        assert_eq!(
            g.request(RepairReason::Orphans, t),
            Verdict::Wait(t + MAX_GAP)
        );
    }

    #[test]
    fn a_load_complete_marks_the_document_as_told() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        assert!(!g.loaded_seen());
        let _ = g.request(RepairReason::LoadComplete, Instant::now());
        assert!(g.loaded_seen());
    }

    #[test]
    fn an_unsendable_repair_counts_as_fruitless() {
        let mut g = RepairGate::new(RepairPolicy::ALL);
        let t = Instant::now();
        g.started(RepairReason::FocusMiss);
        assert_eq!(g.aborted(t), None);
        assert_eq!(g.quiet(), 1);
    }

    #[test]
    fn orphans_keep_a_bounded_sample() {
        use gorca_model::{AppId, ConnectorId, LocalId};
        let o = Orphans::default();
        assert!(!o.pending());
        for i in 0..100u64 {
            o.note(NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(i),
            });
        }
        assert!(o.pending());
        let s = o.take();
        assert_eq!(s.len(), ORPHAN_SAMPLE);
        assert!(!o.pending());
        assert!(o.take().is_empty());
    }
}
