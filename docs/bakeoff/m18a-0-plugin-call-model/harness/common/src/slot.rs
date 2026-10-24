// SPDX-License-Identifier: MIT OR Apache-2.0
//! `Slot` / `SlotState` exactly as ADR-0026 section 2.5.4, plus `slot_epoch` and `busy` from
//! section 2.4.6. The clock is an argument (`now_ns`), so the table is testable without sleeping.
//! `testkit` is the exhaustive driver: every cell of the table, plus the stale-reply and busy
//! cells, driven against an independent expected-table (both arms run it, SPEC 4 M8.2).

use crate::model::JobId;

pub const WINDOW_NS: u64 = 60_000_000_000;
pub const STRIKES_FULL: usize = 3;
pub const BACKOFF_NS: [u64; 2] = [30_000_000_000, 300_000_000_000];
pub const DEMOTE_NS: u64 = 60_000_000_000;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotState {
    Unloaded,
    Cold,
    Ready,
    Backoff { until_ns: u64 },
    Disabled,
}

impl SlotState {
    pub fn kind(self) -> &'static str {
        match self {
            SlotState::Unloaded => "Unloaded",
            SlotState::Cold => "Cold",
            SlotState::Ready => "Ready",
            SlotState::Backoff { .. } => "Backoff",
            SlotState::Disabled => "Disabled",
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SlotEvent {
    PreloadDone,
    PreloadFailed,
    CallNeeded { interactive: bool },
    CallOk,
    Fault,
    WallOverrun,
    BackoffExpires,
    UserReenable,
    PackageSwapped,
    ScopeCloses,
}

/// What the owner must do after the event.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Action {
    None,
    /// Fail-open: no call, no facts.
    Skip,
    /// `Cold` -> `Ready`: instantiate, then call.
    Instantiate,
    Call,
    /// Interactive call refused because the plugin is demoted; background lane only.
    DemotedToBackground,
    /// Ready and busy: the dirty set is coalesced into the pending set.
    Coalesce,
    RestartPreload,
    /// Strike recorded, window not full: instance dropped, slot `Cold`.
    FaultToCold,
    FaultToBackoff,
    FaultToDisabled,
    /// Pre-load failed: `Disabled` plus one spoken notice.
    DisabledWithNotice,
    /// Wall overrun recorded, plugin demoted for 60 s, call effects still admitted.
    OverrunDemoted,
    Dropped,
    /// The table says "impossible": nothing changes, the caller counts a bug.
    Impossible,
}

#[derive(Clone, Debug)]
pub struct Slot {
    pub state: SlotState,
    /// 0..=2 backoff escalations this session (kept across scope close).
    pub level: u8,
    pub strikes: Vec<u64>,
    pub overruns: Vec<u64>,
    /// Demotion to background-only until this instant.
    pub interactive_ok_after: u64,
    /// Bumped on every transition out of `Ready`, on package swap and on scope close.
    pub epoch: u64,
    pub busy: Option<JobId>,
    pub closed: bool,
}

impl Slot {
    pub fn new(state: SlotState) -> Slot {
        Slot {
            state,
            level: 0,
            strikes: Vec::new(),
            overruns: Vec::new(),
            interactive_ok_after: 0,
            epoch: 1,
            busy: None,
            closed: false,
        }
    }

    pub fn is_demoted(&self, now_ns: u64) -> bool {
        now_ns < self.interactive_ok_after
    }

    fn bump(&mut self) {
        self.epoch += 1;
        self.busy = None;
    }

    fn window_push(w: &mut Vec<u64>, now: u64) -> bool {
        w.retain(|t| now.saturating_sub(*t) < WINDOW_NS);
        w.push(now);
        w.len() >= STRIKES_FULL
    }

    fn fault(&mut self, now: u64) -> Action {
        let full = Self::window_push(&mut self.strikes, now);
        let was_ready = self.state == SlotState::Ready;
        if was_ready {
            self.bump();
        }
        if !full {
            self.state = SlotState::Cold;
            Action::FaultToCold
        } else if self.level < 2 {
            let d = BACKOFF_NS[self.level as usize];
            self.level += 1;
            self.state = SlotState::Backoff { until_ns: now + d };
            self.busy = None;
            Action::FaultToBackoff
        } else {
            self.state = SlotState::Disabled;
            self.busy = None;
            Action::FaultToDisabled
        }
    }

    fn clear_windows(&mut self) {
        self.strikes.clear();
        self.overruns.clear();
    }

    /// Drive the table. Impossible cells return [`Action::Impossible`] and change nothing.
    pub fn step(&mut self, ev: SlotEvent, now: u64) -> Action {
        if self.closed {
            return Action::None;
        }
        use SlotEvent as E;
        use SlotState as S;
        match (self.state, ev) {
            (S::Unloaded, E::PreloadDone) => {
                self.state = S::Cold;
                Action::None
            }
            (S::Unloaded, E::PreloadFailed) => {
                self.state = S::Disabled;
                Action::DisabledWithNotice
            }
            (S::Unloaded, E::CallNeeded { .. }) => Action::Skip,
            (S::Unloaded, E::CallOk | E::Fault | E::WallOverrun) => Action::Impossible,
            (S::Unloaded, E::PackageSwapped) => Action::RestartPreload,
            (S::Unloaded, E::BackoffExpires | E::UserReenable) => Action::None,

            (S::Cold, E::CallNeeded { .. }) => {
                self.state = S::Ready;
                Action::Instantiate
            }
            (S::Cold, E::Fault) => self.fault(now),
            (S::Cold, E::CallOk | E::WallOverrun) => Action::Impossible,
            (S::Cold, E::PackageSwapped) => {
                self.state = S::Unloaded;
                Action::None
            }
            (S::Cold, E::PreloadDone | E::PreloadFailed | E::BackoffExpires | E::UserReenable) => {
                Action::None
            }

            (S::Ready, E::CallNeeded { interactive }) => {
                if self.busy.is_some() {
                    Action::Coalesce
                } else if interactive && self.is_demoted(now) {
                    Action::DemotedToBackground
                } else {
                    Action::Call
                }
            }
            (S::Ready, E::CallOk) => Action::None,
            (S::Ready, E::Fault) => self.fault(now),
            (S::Ready, E::WallOverrun) => {
                self.interactive_ok_after = now + DEMOTE_NS;
                if Self::window_push(&mut self.overruns, now) {
                    // Three overruns within 60 s are one fault.
                    self.overruns.clear();
                    self.fault(now)
                } else {
                    Action::OverrunDemoted
                }
            }
            (S::Ready, E::PackageSwapped) => {
                self.bump();
                self.state = S::Unloaded;
                Action::None
            }
            (S::Ready, E::PreloadDone | E::PreloadFailed | E::BackoffExpires | E::UserReenable) => {
                Action::None
            }

            (S::Backoff { .. }, E::CallNeeded { .. }) => Action::Skip,
            (S::Backoff { .. }, E::CallOk | E::Fault | E::WallOverrun) => Action::Impossible,
            (S::Backoff { .. }, E::BackoffExpires) => {
                self.clear_windows();
                self.state = S::Cold;
                Action::None
            }
            (S::Backoff { .. }, E::UserReenable) => {
                self.clear_windows();
                self.level = 0;
                self.state = S::Cold;
                Action::None
            }
            (S::Backoff { .. }, E::PackageSwapped) => {
                self.state = S::Unloaded;
                Action::None
            }
            (S::Backoff { .. }, E::PreloadDone | E::PreloadFailed) => Action::None,

            (S::Disabled, E::CallNeeded { .. }) => Action::Skip,
            (S::Disabled, E::CallOk | E::Fault | E::WallOverrun) => Action::Impossible,
            (S::Disabled, E::UserReenable) => {
                self.clear_windows();
                self.level = 0;
                self.state = S::Cold;
                Action::None
            }
            (
                S::Disabled,
                E::PreloadDone | E::PreloadFailed | E::BackoffExpires | E::PackageSwapped,
            ) => Action::None,

            (_, E::ScopeCloses) => {
                if self.state == S::Ready {
                    self.bump();
                }
                self.closed = true;
                Action::Dropped
            }
        }
    }

    /// A reply carries the `slot_epoch` of the job. Stale replies are dropped and counted by the
    /// owner; they never change the slot (cannot revive `Backoff`/`Disabled`).
    pub fn reply_is_current(&self, epoch: u64) -> bool {
        !self.closed && epoch == self.epoch
    }

    /// Time-driven transition: `Backoff` -> `Cold` when its deadline passed.
    pub fn tick(&mut self, now: u64) -> bool {
        if let SlotState::Backoff { until_ns } = self.state {
            if now >= until_ns {
                self.step(SlotEvent::BackoffExpires, now);
                return true;
            }
        }
        false
    }
}

/// Exhaustive driver of the ADR table. The expected table below is written independently of
/// `Slot::step` (as data), so a wrong arm in `step` fails here.
pub mod testkit {
    use super::*;

    #[derive(Debug)]
    struct Cell {
        from: &'static str,
        ev: SlotEvent,
        to: &'static str,
        act: Action,
    }

    fn mk(from: &'static str, ev: SlotEvent, to: &'static str, act: Action) -> Cell {
        Cell { from, ev, to, act }
    }

    fn fresh(kind: &str, now: u64) -> Slot {
        let st = match kind {
            "Unloaded" => SlotState::Unloaded,
            "Cold" => SlotState::Cold,
            "Ready" => SlotState::Ready,
            "Backoff" => SlotState::Backoff {
                until_ns: now + 1_000,
            },
            _ => SlotState::Disabled,
        };
        let mut s = Slot::new(st);
        s.level = 1;
        s
    }

    fn table() -> Vec<Cell> {
        use Action as A;
        use SlotEvent as E;
        let ci = E::CallNeeded { interactive: true };
        vec![
            mk("Unloaded", E::PreloadDone, "Cold", A::None),
            mk(
                "Unloaded",
                E::PreloadFailed,
                "Disabled",
                A::DisabledWithNotice,
            ),
            mk("Unloaded", ci, "Unloaded", A::Skip),
            mk("Unloaded", E::CallOk, "Unloaded", A::Impossible),
            mk("Unloaded", E::Fault, "Unloaded", A::Impossible),
            mk("Unloaded", E::WallOverrun, "Unloaded", A::Impossible),
            mk("Unloaded", E::BackoffExpires, "Unloaded", A::None),
            mk("Unloaded", E::UserReenable, "Unloaded", A::None),
            mk("Unloaded", E::PackageSwapped, "Unloaded", A::RestartPreload),
            mk("Unloaded", E::ScopeCloses, "Unloaded", A::Dropped),
            mk("Cold", E::PreloadDone, "Cold", A::None),
            mk("Cold", E::PreloadFailed, "Cold", A::None),
            mk("Cold", ci, "Ready", A::Instantiate),
            mk("Cold", E::CallOk, "Cold", A::Impossible),
            mk("Cold", E::Fault, "Cold", A::FaultToCold),
            mk("Cold", E::WallOverrun, "Cold", A::Impossible),
            mk("Cold", E::BackoffExpires, "Cold", A::None),
            mk("Cold", E::UserReenable, "Cold", A::None),
            mk("Cold", E::PackageSwapped, "Unloaded", A::None),
            mk("Cold", E::ScopeCloses, "Cold", A::Dropped),
            mk("Ready", E::PreloadDone, "Ready", A::None),
            mk("Ready", E::PreloadFailed, "Ready", A::None),
            mk("Ready", ci, "Ready", A::Call),
            mk("Ready", E::CallOk, "Ready", A::None),
            mk("Ready", E::Fault, "Cold", A::FaultToCold),
            mk("Ready", E::WallOverrun, "Ready", A::OverrunDemoted),
            mk("Ready", E::BackoffExpires, "Ready", A::None),
            mk("Ready", E::UserReenable, "Ready", A::None),
            mk("Ready", E::PackageSwapped, "Unloaded", A::None),
            mk("Ready", E::ScopeCloses, "Ready", A::Dropped),
            mk("Backoff", E::PreloadDone, "Backoff", A::None),
            mk("Backoff", E::PreloadFailed, "Backoff", A::None),
            mk("Backoff", ci, "Backoff", A::Skip),
            mk("Backoff", E::CallOk, "Backoff", A::Impossible),
            mk("Backoff", E::Fault, "Backoff", A::Impossible),
            mk("Backoff", E::WallOverrun, "Backoff", A::Impossible),
            mk("Backoff", E::BackoffExpires, "Cold", A::None),
            mk("Backoff", E::UserReenable, "Cold", A::None),
            mk("Backoff", E::PackageSwapped, "Unloaded", A::None),
            mk("Backoff", E::ScopeCloses, "Backoff", A::Dropped),
            mk("Disabled", E::PreloadDone, "Disabled", A::None),
            mk("Disabled", E::PreloadFailed, "Disabled", A::None),
            mk("Disabled", ci, "Disabled", A::Skip),
            mk("Disabled", E::CallOk, "Disabled", A::Impossible),
            mk("Disabled", E::Fault, "Disabled", A::Impossible),
            mk("Disabled", E::WallOverrun, "Disabled", A::Impossible),
            mk("Disabled", E::BackoffExpires, "Disabled", A::None),
            mk("Disabled", E::UserReenable, "Cold", A::None),
            mk("Disabled", E::PackageSwapped, "Disabled", A::None),
            mk("Disabled", E::ScopeCloses, "Disabled", A::Dropped),
        ]
    }

    /// Runs every cell plus the derived rows. `Err` names the first wrong cell.
    pub fn run_exhaustive() -> Result<usize, String> {
        let now = 10_000_000_000u64;
        let mut n = 0usize;
        for c in table() {
            let mut s = fresh(c.from, now);
            let before_epoch = s.epoch;
            let act = s.step(c.ev, now);
            if act != c.act || s.state.kind() != c.to {
                return Err(format!("cell {c:?}: got {act:?} -> {}", s.state.kind()));
            }
            // The epoch rule: bumped exactly on transitions out of Ready.
            let left_ready =
                c.from == "Ready" && (c.to != "Ready" || matches!(c.ev, SlotEvent::ScopeCloses));
            if left_ready != (s.epoch != before_epoch) {
                return Err(format!("epoch rule broken at {c:?}"));
            }
            n += 1;
        }
        // Ready + busy: call needed is coalesced.
        let mut s = fresh("Ready", now);
        s.busy = Some(7);
        if s.step(SlotEvent::CallNeeded { interactive: true }, now) != Action::Coalesce {
            return Err("busy cell: call needed must coalesce".into());
        }
        // Ready + demoted: interactive refused, background allowed.
        let mut s = fresh("Ready", now);
        s.interactive_ok_after = now + 1;
        if s.step(SlotEvent::CallNeeded { interactive: true }, now) != Action::DemotedToBackground
            || s.step(SlotEvent::CallNeeded { interactive: false }, now) != Action::Call
        {
            return Err("demotion cell".into());
        }
        // Strike window: 2 faults -> Cold, third -> Backoff(30 s), then 5 min, then Disabled.
        let mut s = Slot::new(SlotState::Ready);
        let mut t = now;
        let mut seen = Vec::new();
        for _ in 0..9 {
            if s.state == SlotState::Cold {
                s.step(SlotEvent::CallNeeded { interactive: false }, t);
            }
            if let SlotState::Backoff { until_ns } = s.state {
                t = until_ns;
                s.tick(t);
                s.step(SlotEvent::CallNeeded { interactive: false }, t);
            }
            seen.push(s.step(SlotEvent::Fault, t));
            t += 1_000;
        }
        use Action::*;
        let want = [
            FaultToCold,
            FaultToCold,
            FaultToBackoff,
            FaultToCold,
            FaultToCold,
            FaultToBackoff,
            FaultToCold,
            FaultToCold,
            FaultToDisabled,
        ];
        if seen != want || s.state != SlotState::Disabled {
            return Err(format!("9 faults must disable: {seen:?}"));
        }
        // 3 wall overruns in 60 s are one fault; spread over more than 60 s they are not.
        let mut s = Slot::new(SlotState::Ready);
        let a = [
            s.step(SlotEvent::WallOverrun, now),
            s.step(SlotEvent::WallOverrun, now + 1),
            s.step(SlotEvent::WallOverrun, now + 2),
        ];
        if a != [OverrunDemoted, OverrunDemoted, FaultToCold] {
            return Err(format!("3 overruns = one fault: {a:?}"));
        }
        let mut s = Slot::new(SlotState::Ready);
        let b = [
            s.step(SlotEvent::WallOverrun, now),
            s.step(SlotEvent::WallOverrun, now + WINDOW_NS + 1),
            s.step(SlotEvent::WallOverrun, now + 2 * WINDOW_NS + 2),
        ];
        if b != [OverrunDemoted, OverrunDemoted, OverrunDemoted] {
            return Err(format!("overruns outside the window: {b:?}"));
        }
        // Backoff expiry clears windows and keeps the level; user re-enable resets the level.
        let mut s = Slot::new(SlotState::Ready);
        for i in 0..3u64 {
            if s.state == SlotState::Cold {
                s.step(SlotEvent::CallNeeded { interactive: false }, now + i);
            }
            s.step(SlotEvent::Fault, now + i);
        }
        if !matches!(s.state, SlotState::Backoff { .. }) || s.level != 1 {
            return Err("third strike must back off".into());
        }
        s.step(SlotEvent::BackoffExpires, now + BACKOFF_NS[0]);
        if s.state != SlotState::Cold || !s.strikes.is_empty() || s.level != 1 {
            return Err("expiry: Cold, windows cleared, level kept".into());
        }
        // Stale replies: per state, an old epoch is never current and changes nothing.
        for k in ["Unloaded", "Cold", "Ready", "Backoff", "Disabled"] {
            let s = fresh(k, now);
            let snap = (s.state, s.level, s.epoch);
            if s.reply_is_current(s.epoch - 1) || !s.reply_is_current(s.epoch) {
                return Err(format!("stale reply cell in {k}"));
            }
            if snap != (s.state, s.level, s.epoch) {
                return Err("reply check mutated the slot".into());
            }
            n += 1;
        }
        // Leaving Ready invalidates the in-flight job (the reply of the faulted incarnation).
        let mut s = fresh("Ready", now);
        let e = s.epoch;
        s.busy = Some(3);
        s.step(SlotEvent::Fault, now);
        if s.reply_is_current(e) || s.busy.is_some() {
            return Err("fault must invalidate the in-flight job".into());
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exhaustive_table() {
        let n = super::testkit::run_exhaustive().expect("table");
        assert!(n >= 50);
    }
}
