// SPDX-License-Identifier: GPL-3.0-or-later
//! The pointer gate of ADR-0025 section 2.5 (RG-1, RG-4, RG-5): when the keypad mouse chords are
//! grabbed. Pure: availability and the `mouse.enabled` setting in, "open changed" and a wake time
//! out. Gate changes become `SetGrabSet` on the existing session, never a reconnect.
//!
//! - The gate wants to be open while the helper is `Ready` or `Unresponsive` (C-3) **and**
//!   `mouse.enabled`. `Unresponsive` keeps it open: a Shell stall stalls key delivery too, so
//!   closing would only queue a `SetKeyGrabs` into the stalled Mutter and let `/` and `*` type
//!   into the focused field after recovery.
//! - **Closing is immediate** (fail safe: the keys type again).
//! - **Opening is damped (RG-5):** the wish must have held for [`OPEN_AFTER`] and the last
//!   opening must be at least [`OPEN_AFTER`] ago, so a flapping helper costs at most one open and
//!   one close per 2 s and an owner's hot reload costs two `SetKeyGrabs`.

use std::time::{Duration, Instant};

use gorca_output_api::PointerAvail;

use super::spec::RuntimeGate;

/// How long the helper must have been usable before the keys are grabbed, and the minimum
/// distance between two openings (RG-5).
pub const OPEN_AFTER: Duration = Duration::from_secs(2);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PointerMsg {
    /// The availability watch of the pointer sink changed.
    Avail(PointerAvail),
    /// The `mouse.enabled` setting.
    Enabled(bool),
}

/// What the Reader must do after a step.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Default)]
pub struct GateOut {
    /// The gate changed to this state: send a new grab set.
    pub changed: Option<bool>,
    /// Call [`PointerGate::tick`] at this time.
    pub wake: Option<Instant>,
}

#[derive(Copy, Clone, Debug)]
pub struct PointerGate {
    avail: PointerAvail,
    enabled: bool,
    open: bool,
    /// Since when the gate has wanted to be open, without interruption.
    wanted_since: Option<Instant>,
    last_open: Option<Instant>,
}

impl Default for PointerGate {
    fn default() -> Self {
        Self::new()
    }
}

impl PointerGate {
    /// Closed, helper absent, `mouse.enabled` on (the schema default).
    pub const fn new() -> Self {
        Self {
            avail: PointerAvail::Absent,
            enabled: true,
            open: false,
            wanted_since: None,
            last_open: None,
        }
    }

    pub const fn avail(&self) -> PointerAvail {
        self.avail
    }

    pub const fn enabled(&self) -> bool {
        self.enabled
    }

    /// The keys are grabbed.
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// The bit set of open gates, for `ModeKeys::open_gates`.
    pub const fn open_bits(&self) -> u8 {
        if self.open {
            RuntimeGate::Pointer.bit()
        } else {
            0
        }
    }

    const fn wants_open(&self) -> bool {
        self.enabled && self.avail.gate_open()
    }

    pub fn step(&mut self, msg: PointerMsg, now: Instant) -> GateOut {
        match msg {
            PointerMsg::Avail(a) => self.avail = a,
            PointerMsg::Enabled(e) => self.enabled = e,
        }
        self.settle(now)
    }

    /// The wake time armed by an earlier step has come.
    pub fn tick(&mut self, now: Instant) -> GateOut {
        self.settle(now)
    }

    fn settle(&mut self, now: Instant) -> GateOut {
        if !self.wants_open() {
            self.wanted_since = None;
            if self.open {
                self.open = false;
                return GateOut {
                    changed: Some(false),
                    wake: None,
                };
            }
            return GateOut::default();
        }
        if self.open {
            return GateOut::default();
        }
        let since = *self.wanted_since.get_or_insert(now);
        let mut due = since + OPEN_AFTER;
        if let Some(last) = self.last_open {
            due = due.max(last + OPEN_AFTER);
        }
        if now >= due {
            self.open = true;
            self.last_open = Some(now);
            GateOut {
                changed: Some(true),
                wake: None,
            }
        } else {
            GateOut {
                changed: None,
                wake: Some(due),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const READY: PointerAvail = PointerAvail::Ready { version: 2 };

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn starts_closed_and_opens_only_after_two_seconds_of_ready() {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        assert!(!g.is_open());
        let o = g.step(PointerMsg::Avail(READY), t0);
        assert_eq!(o.changed, None);
        assert_eq!(o.wake, Some(at(t0, 2000)));
        assert_eq!(g.tick(at(t0, 1999)).changed, None);
        let o = g.tick(at(t0, 2000));
        assert_eq!(o.changed, Some(true));
        assert!(g.is_open() && g.open_bits() == RuntimeGate::Pointer.bit());
    }

    #[test]
    fn closing_is_immediate_for_every_closed_state() {
        for closed in [
            PointerAvail::Absent,
            PointerAvail::Squatted,
            PointerAvail::Denied,
            PointerAvail::TooOld { version: 1 },
            PointerAvail::NoCap,
        ] {
            let t0 = Instant::now();
            let mut g = PointerGate::new();
            g.step(PointerMsg::Avail(READY), t0);
            g.tick(at(t0, 2000));
            assert!(g.is_open());
            let o = g.step(PointerMsg::Avail(closed), at(t0, 2100));
            assert_eq!(o.changed, Some(false), "{closed:?}");
            assert!(!g.is_open() && g.open_bits() == 0);
        }
    }

    #[test]
    fn unresponsive_keeps_the_gate_open_and_never_flips_it() {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        g.step(PointerMsg::Avail(READY), t0);
        g.tick(at(t0, 2000));
        let o = g.step(PointerMsg::Avail(PointerAvail::Unresponsive), at(t0, 5000));
        assert_eq!(o, GateOut::default());
        assert!(g.is_open());
        let o = g.step(PointerMsg::Avail(READY), at(t0, 9000));
        assert_eq!(o, GateOut::default());
        assert!(g.is_open());
    }

    #[test]
    fn mouse_enabled_off_closes_and_on_reopens_after_the_damping() {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        g.step(PointerMsg::Avail(READY), t0);
        g.tick(at(t0, 2000));
        assert_eq!(
            g.step(PointerMsg::Enabled(false), at(t0, 3000)).changed,
            Some(false)
        );
        let o = g.step(PointerMsg::Enabled(true), at(t0, 3100));
        // wish held again from 3100; and the last opening was at 2000
        assert_eq!(o.wake, Some(at(t0, 5100)));
        assert_eq!(g.tick(at(t0, 5100)).changed, Some(true));
    }

    /// Flip Ready/Absent every `period_ms` for 12 s on a realistic timeline (the Reader's timer fires at
    /// the armed time); returns (openings, closings).
    fn flap(period_ms: u64) -> (u32, u32) {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        let (mut opens, mut closes) = (0, 0);
        let mut count = |o: GateOut| match o.changed {
            Some(true) => opens += 1,
            Some(false) => closes += 1,
            None => {}
        };
        let mut wake = g.step(PointerMsg::Avail(READY), t0).wake;
        let mut ms = 0_u64;
        let mut ready = true;
        while ms < 12_000 {
            ms += 50;
            let now = at(t0, ms);
            if let Some(w) = wake.filter(|w| *w <= now) {
                let o = g.tick(w);
                count(o);
                wake = o.wake;
            }
            if ms.is_multiple_of(period_ms) {
                ready = !ready;
                let a = if ready { READY } else { PointerAvail::Absent };
                let o = g.step(PointerMsg::Avail(a), now);
                count(o);
                wake = o.wake;
            }
        }
        (opens, closes)
    }

    #[test]
    fn a_fast_flapping_helper_never_opens_the_gate() {
        assert_eq!(flap(100), (0, 0));
        assert_eq!(flap(1_500), (0, 0), "ready never holds for 2 s");
    }

    #[test]
    fn a_slow_flapping_helper_costs_at_most_one_open_and_one_close_per_two_seconds() {
        // ready for 2.1 s, gone for 2.1 s: one opening and one closing per period
        let (opens, closes) = flap(2_100);
        assert!((1..=3).contains(&opens), "opens={opens}");
        assert!(closes <= opens + 1, "closes={closes} opens={opens}");
        assert!(opens <= 12 / 2, "never more than one opening per 2 s");
    }

    #[test]
    fn the_second_opening_waits_two_seconds_after_the_first() {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        g.step(PointerMsg::Avail(READY), t0);
        g.tick(at(t0, 2000)); // opened at 2000
        g.step(PointerMsg::Avail(PointerAvail::Absent), at(t0, 2100)); // closed
        let o = g.step(PointerMsg::Avail(READY), at(t0, 2200));
        // wished since 2200 (+2 s) and last opening 2000 (+2 s): 4200
        assert_eq!(o.wake, Some(at(t0, 4200)));
    }

    #[test]
    fn disabled_or_absent_never_wakes() {
        let t0 = Instant::now();
        let mut g = PointerGate::new();
        assert_eq!(
            g.step(PointerMsg::Avail(PointerAvail::Absent), t0),
            GateOut::default()
        );
        g.step(PointerMsg::Enabled(false), t0);
        assert_eq!(g.step(PointerMsg::Avail(READY), t0), GateOut::default());
    }
}
