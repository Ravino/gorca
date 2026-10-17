// SPDX-License-Identifier: GPL-3.0-or-later
//! A replica of the a11y key logic in Mutter (ADR-0005 4.3), line by line.
//!
//! Sources (`refs/mutter/src/backends`):
//! - `native/meta-seat-impl.c`: `is_a11y_modifier_first_click`, called with the event's keysym
//!   for every key event. **Mutter 48.7 also calls it for autorepeat events** (state 2, treated as
//!   a press); upstream main guards them (`if (state == AUTOREPEAT_VALUE) should_ignore =
//!   FALSE`), 48.7 does not. The replica follows 48.7, the Mutter this project targets
//!   (`docs/reports/live-keys-swallowed.md`): holding a grabbed modifier that autorepeats
//!   (Insert, KP_Insert) for about 0.57 s or more leaves its entry in `grabbed_keypresses` for
//!   good, and from then on every key is swallowed until the modifier is tapped once
//!   ([`Verdict::stale`]);
//! - `meta-a11y-manager.c`: `should_grab_keypress` and `meta_a11y_manager_notify_clients`, which
//!   decide whether the event is swallowed.
//!
//! The only input the replica cannot reproduce exactly is time: Mutter compares libinput
//! timestamps, gorca sees receipt times (PF-4). When the interval that decides the double-tap
//! rule lies within `±guard` of `repeat_delay`, the caller forks the replica into both outcomes
//! ([`Replica::step`] takes the forced outcome) and the lane reports `Ambiguous` for any key
//! whose verdict differs between the worlds.

use smallvec::SmallVec;

use crate::event::Modifiers;
use crate::grab::GrabSet;

type KeySet = SmallVec<[u32; 8]>;

/// Mutter's per-seat a11y state plus the a11y manager's `grabbed_keypresses`.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Replica {
    last_keysym: u32,
    last_time: u32,
    saw_first_release: bool,
    pressed_modifiers: KeySet,
    grabbed_keypresses: KeySet,
    /// Grabbed modifiers that are physically down (our own bookkeeping, not Mutter's): tells a
    /// real chord from a stale `grabbed_keypresses` entry.
    down_mods: KeySet,
    /// Autorepeat events seen since the last non-repeat event.
    repeats: u32,
}

/// One key event as the replica needs it.
#[derive(Copy, Clone, Debug)]
pub struct Ev {
    pub keysym: u32,
    pub pressed: bool,
    /// Autorepeat (a press of a key already down). Mutter 48.7 runs the first-click logic for it
    /// like for a press; the flag only decides how [`Replica::timing`] resolves the time rule.
    pub repeat: bool,
    /// Event state without mouse buttons; locks are ignored by the grab-set lookup.
    pub state: Modifiers,
    /// Milliseconds on any monotonic clock (wrapping `u32`, like libinput's).
    pub time_ms: u32,
}

/// What Mutter did with the event.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Verdict {
    /// Never reached the application.
    pub swallowed: bool,
    /// A grabbed modifier was held when the event arrived (the gorca modifier is "down").
    pub gorca_held: bool,
    /// The event is the press of a grabbed modifier that Mutter passed on (double tap).
    pub passed_modifier: bool,
    /// The event is the press or release of a grabbed modifier that was swallowed.
    pub modifier_swallowed: bool,
    /// After this event a grabbed modifier is in `grabbed_keypresses` although it is not held:
    /// Mutter 48.7 swallows every key until the modifier is tapped once. Such a key is never a
    /// chord and never reaches the application.
    pub stale: Option<u32>,
}

/// How the timing of the event decides the first-click rule.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Timing {
    /// The outcome does not depend on the time.
    Irrelevant,
    Soon,
    Late,
    /// Within the guard band: both outcomes are possible.
    Band,
}

impl Replica {
    /// Does the first-click decision for `ev` hinge on `time_ms - last_time < repeat_delay`?
    pub fn timing(&self, ev: &Ev, grabs: &GrabSet, repeat_delay: u32, guard: u32) -> Timing {
        if ev.repeat {
            // Mutter's repeat timer never fires early, so the first autorepeat of a key comes at
            // least `repeat_delay` after its press (never "soon"); later ones come one repeat
            // interval apart (always "soon"). Receipt jitter must not decide it.
            if !grabs.modifiers.contains(&ev.keysym) || ev.keysym != self.last_keysym {
                return Timing::Irrelevant;
            }
            return if self.repeats == 0 {
                Timing::Late
            } else {
                Timing::Soon
            };
        }
        if !grabs.modifiers.contains(&ev.keysym)
            || ev.keysym != self.last_keysym
            || (!ev.pressed && self.pressed_modifiers.contains(&ev.keysym))
        {
            return Timing::Irrelevant;
        }
        let dt = ev.time_ms.wrapping_sub(self.last_time);
        // The wrapped subtraction makes a clock that went backwards look huge: treat as late.
        let lo = repeat_delay.saturating_sub(guard);
        let hi = repeat_delay.saturating_add(guard);
        if dt < lo {
            Timing::Soon
        } else if dt > hi {
            Timing::Late
        } else {
            Timing::Band
        }
    }

    /// `is_a11y_modifier_first_click`. `true` = Mutter ignores (swallows) the event.
    fn first_click(&mut self, ev: &Ev, grabs: &GrabSet, soon: bool) -> bool {
        let is_same = ev.keysym == self.last_keysym;
        self.last_keysym = ev.keysym;
        self.last_time = ev.time_ms;
        if !grabs.modifiers.contains(&ev.keysym) {
            return false;
        }
        if !ev.pressed && self.pressed_modifiers.contains(&ev.keysym) {
            self.pressed_modifiers.retain(|k| *k != ev.keysym);
            return false;
        }
        if is_same && soon {
            if ev.pressed && self.saw_first_release {
                self.saw_first_release = false;
                if !self.pressed_modifiers.contains(&ev.keysym) {
                    self.pressed_modifiers.push(ev.keysym);
                }
                return false;
            }
            self.saw_first_release = true;
            return true;
        }
        self.saw_first_release = false;
        true
    }

    fn should_grab(&self, grabs: &GrabSet, keysym: u32, state: Modifiers) -> bool {
        if grabs.grab_all {
            return true;
        }
        if grabs
            .modifiers
            .iter()
            .any(|m| *m == keysym || self.grabbed_keypresses.contains(m))
        {
            return true;
        }
        grabs.chords.contains(&crate::grab::Chord {
            keysym,
            modifiers: state.without_locks().to_wire(),
        }) || grabs.exact.contains(&crate::grab::Chord {
            keysym,
            modifiers: state.to_wire(),
        })
    }

    /// Process one event. `soon` is the forced outcome of the time comparison; pass the nominal
    /// one for [`Timing::Soon`]/[`Timing::Late`] and either for [`Timing::Band`].
    pub fn step(&mut self, ev: &Ev, grabs: &GrabSet, soon: bool) -> Verdict {
        let gorca_held = grabs
            .modifiers
            .iter()
            .any(|m| self.grabbed_keypresses.contains(m) && self.down_mods.contains(m));
        self.repeats = if ev.repeat { self.repeats + 1 } else { 0 };
        let ignorable = self.first_click(ev, grabs, soon);
        let is_mod = grabs.modifiers.contains(&ev.keysym);
        if !ev.pressed {
            self.down_mods.retain(|k| *k != ev.keysym);
        } else if is_mod && !self.down_mods.contains(&ev.keysym) {
            self.down_mods.push(ev.keysym);
        }
        if is_mod && !ignorable {
            return Verdict {
                swallowed: false,
                gorca_held,
                passed_modifier: ev.pressed,
                modifier_swallowed: false,
                stale: self.is_stale(grabs),
            };
        }
        let swallowed = if ev.pressed {
            let mut grabbed = self.grabbed_keypresses.contains(&ev.keysym);
            if self.should_grab(grabs, ev.keysym, ev.state) {
                if !grabbed {
                    self.grabbed_keypresses.push(ev.keysym);
                }
                grabbed = true;
            }
            grabbed
        } else if self.grabbed_keypresses.contains(&ev.keysym) {
            self.grabbed_keypresses.retain(|k| *k != ev.keysym);
            true
        } else {
            false
        };
        Verdict {
            swallowed,
            gorca_held,
            passed_modifier: false,
            modifier_swallowed: is_mod && swallowed,
            stale: self.is_stale(grabs),
        }
    }

    /// A grabbed modifier sits in `grabbed_keypresses` while it is not held (Mutter 48.7 bug).
    fn is_stale(&self, grabs: &GrabSet) -> Option<u32> {
        grabs
            .modifiers
            .iter()
            .copied()
            .find(|m| self.grabbed_keypresses.contains(m) && !self.down_mods.contains(m))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INS: u32 = 0xff63;
    const T: u32 = 0x74;
    const A: u32 = 0x61;

    fn grabs() -> GrabSet {
        let mut g = GrabSet::default();
        g.modifiers.insert(INS);
        g
    }

    fn ev(keysym: u32, pressed: bool, t: u32) -> Ev {
        Ev {
            keysym,
            pressed,
            repeat: false,
            state: Modifiers::empty(),
            time_ms: t,
        }
    }

    fn run(r: &mut Replica, g: &GrabSet, e: Ev) -> Verdict {
        let soon = e.time_ms.wrapping_sub(r.last_time) < 500;
        r.step(&e, g, soon)
    }

    #[test]
    fn modifier_chord_swallows_everything_while_held() {
        let (mut r, g) = (Replica::default(), grabs());
        let v = run(&mut r, &g, ev(INS, true, 1000));
        assert!(v.swallowed && v.modifier_swallowed && !v.passed_modifier);
        let v = run(&mut r, &g, ev(T, true, 1100));
        assert!(v.swallowed && v.gorca_held);
        let v = run(&mut r, &g, ev(T, false, 1150));
        assert!(v.swallowed && v.gorca_held);
        let v = run(&mut r, &g, ev(INS, false, 1200));
        assert!(v.swallowed);
        // Held state is gone: plain keys reach the app.
        let v = run(&mut r, &g, ev(A, true, 1300));
        assert!(!v.swallowed && !v.gorca_held);
    }

    #[test]
    fn double_tap_passes_the_second_press_and_what_is_typed_under_it() {
        let (mut r, g) = (Replica::default(), grabs());
        assert!(run(&mut r, &g, ev(INS, true, 1000)).swallowed);
        assert!(run(&mut r, &g, ev(INS, false, 1050)).swallowed);
        let v = run(&mut r, &g, ev(INS, true, 1200));
        assert!(!v.swallowed && v.passed_modifier, "{v:?}");
        // Keys typed while the passed press is down are not grabbed (PF-2).
        let v = run(&mut r, &g, ev(A, true, 1250));
        assert!(!v.swallowed && !v.gorca_held);
        run(&mut r, &g, ev(A, false, 1260));
        let v = run(&mut r, &g, ev(INS, false, 1300));
        assert!(!v.swallowed, "release of a passed modifier reaches the app");
        // The next lone press is a first click again.
        let v = run(&mut r, &g, ev(INS, true, 3000));
        assert!(v.swallowed);
    }

    #[test]
    fn a_second_press_after_the_repeat_delay_is_swallowed() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(INS, false, 1050));
        let v = run(&mut r, &g, ev(INS, true, 1050 + 500));
        assert!(v.swallowed && !v.passed_modifier);
    }

    #[test]
    fn any_key_in_between_breaks_the_double_tap() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(INS, false, 1050));
        run(&mut r, &g, ev(A, true, 1100));
        run(&mut r, &g, ev(A, false, 1120));
        let v = run(&mut r, &g, ev(INS, true, 1200));
        assert!(v.swallowed && !v.passed_modifier);
    }

    #[test]
    fn a_long_hold_does_not_arm_the_double_tap() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(INS, false, 1000 + 800));
        let v = run(&mut r, &g, ev(INS, true, 1000 + 900));
        assert!(v.swallowed && !v.passed_modifier);
    }

    #[test]
    fn chord_then_release_does_not_arm() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(T, true, 1010));
        run(&mut r, &g, ev(T, false, 1020));
        run(&mut r, &g, ev(INS, false, 1030));
        // Press again right away: swallowed, and the following key is a chord.
        let v = run(&mut r, &g, ev(INS, true, 1100));
        assert!(v.swallowed);
        let v = run(&mut r, &g, ev(T, true, 1120));
        assert!(v.swallowed && v.gorca_held);
    }

    /// Feed an event the way the lane does: repeats get their time rule from `timing`.
    fn feed(r: &mut Replica, g: &GrabSet, e: Ev) -> Verdict {
        let soon = match r.timing(&e, g, 500, 15) {
            Timing::Soon => true,
            Timing::Late | Timing::Irrelevant => false,
            Timing::Band => panic!("band at {}", e.time_ms),
        };
        r.step(&e, g, soon)
    }

    fn rep(keysym: u32, t: u32) -> Ev {
        Ev {
            repeat: true,
            ..ev(keysym, true, t)
        }
    }

    /// Hold a grabbed modifier from t=1000: repeats start at +500 ms, one per 30 ms, release at
    /// `hold_ms`. Returns the replica afterwards.
    fn hold(hold_ms: u32) -> Replica {
        let (mut r, g) = (Replica::default(), grabs());
        feed(&mut r, &g, ev(INS, true, 1000));
        let mut t = 1500;
        while t < 1000 + hold_ms {
            feed(&mut r, &g, rep(INS, t));
            t += 30;
        }
        feed(&mut r, &g, ev(INS, false, 1000 + hold_ms));
        r
    }

    #[test]
    fn mutter_48_7_autorepeat_of_a_grabbed_modifier_runs_the_first_click_rule() {
        let (mut r, g) = (Replica::default(), grabs());
        feed(&mut r, &g, ev(INS, true, 1000));
        // The first repeat comes after the repeat delay: not "soon", so a first click again.
        let v = feed(&mut r, &g, rep(INS, 1500));
        assert!(
            v.swallowed && v.modifier_swallowed && !v.passed_modifier,
            "{v:?}"
        );
        // The second one is soon after the first: "first release", swallowed.
        let v = feed(&mut r, &g, rep(INS, 1530));
        assert!(v.swallowed, "{v:?}");
        // The third is "the second press": processed normally, so it leaks to the app and the
        // modifier lands in pressed_modifiers.
        let v = feed(&mut r, &g, rep(INS, 1560));
        assert!(!v.swallowed && v.passed_modifier, "{v:?}");
    }

    #[test]
    fn a_hold_of_570_ms_or_more_leaves_the_modifier_stale_and_shorter_holds_do_not() {
        // The thresholds measured on real Mutter 48.7 (tools/platform-probes/stuck-modifier).
        for ms in [300, 450, 480, 520, 550, 560] {
            assert!(
                hold(ms).is_stale(&grabs()).is_none(),
                "hold {ms} ms must be clean"
            );
        }
        for ms in [570, 600, 700, 900, 1500] {
            assert!(
                hold(ms).is_stale(&grabs()).is_some(),
                "hold {ms} ms must be stale"
            );
        }
    }

    #[test]
    fn while_stale_every_key_is_swallowed_and_none_is_a_chord() {
        let (mut r, g) = (hold(900), grabs());
        let v = feed(&mut r, &g, ev(T, true, 3000));
        assert!(
            v.swallowed && v.stale == Some(INS) && !v.gorca_held,
            "{v:?}"
        );
        let v = feed(&mut r, &g, ev(T, false, 3040));
        assert!(v.swallowed && !v.gorca_held, "{v:?}");
        assert!(feed(&mut r, &g, ev(A, true, 3100)).swallowed);
    }

    #[test]
    fn one_tap_of_the_modifier_heals_the_stale_state() {
        let (mut r, g) = (hold(900), grabs());
        let v = feed(&mut r, &g, ev(INS, true, 5000));
        assert!(v.swallowed && v.modifier_swallowed, "{v:?}");
        let v = feed(&mut r, &g, ev(INS, false, 5050));
        assert!(v.swallowed && v.stale.is_none(), "{v:?}");
        let v = feed(&mut r, &g, ev(A, true, 5200));
        assert!(!v.swallowed && v.stale.is_none(), "{v:?}");
    }

    #[test]
    fn a_real_chord_is_not_stale() {
        let (mut r, g) = (Replica::default(), grabs());
        feed(&mut r, &g, ev(INS, true, 1000));
        let v = feed(&mut r, &g, ev(T, true, 1100));
        assert!(v.swallowed && v.gorca_held && v.stale.is_none(), "{v:?}");
    }

    #[test]
    fn mutter_48_7_autorepeat_updates_the_double_tap_state() {
        // Upstream main skips the first-click logic for autorepeat; 48.7 does not.
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(T, true, 1010));
        let v = r.step(&rep(T, 1500), &g, false);
        assert!(v.swallowed);
        assert_eq!(r.last_keysym, T);
        assert_eq!(r.last_time, 1500);
    }

    #[test]
    fn keystroke_grabs_match_exact_masks_and_ignore_locks() {
        let mut g = GrabSet::default();
        g.chords.insert(crate::grab::Chord {
            keysym: T,
            modifiers: Modifiers::CONTROL,
        });
        let mut r = Replica::default();
        let mut e = ev(T, true, 10);
        e.state = Modifiers::CONTROL | Modifiers::MOD2;
        assert!(
            r.step(&e, &g, false).swallowed,
            "NumLock variant is registered too"
        );
        let mut r = Replica::default();
        e.state = Modifiers::CONTROL | Modifiers::SHIFT;
        assert!(!r.step(&e, &g, false).swallowed);
    }

    #[test]
    fn grab_all_swallows_everything() {
        let g = GrabSet {
            grab_all: true,
            ..GrabSet::default()
        };
        let mut r = Replica::default();
        assert!(r.step(&ev(A, true, 1), &g, false).swallowed);
        assert!(r.step(&ev(A, false, 2), &g, false).swallowed);
    }

    #[test]
    fn timing_reports_the_band_around_the_repeat_delay() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(INS, false, 1050));
        let e = |t| ev(INS, true, t);
        assert_eq!(r.timing(&e(1100), &g, 500, 15), Timing::Soon);
        assert_eq!(r.timing(&e(1050 + 490), &g, 500, 15), Timing::Band);
        assert_eq!(r.timing(&e(1050 + 515), &g, 500, 15), Timing::Band);
        assert_eq!(r.timing(&e(1050 + 516), &g, 500, 15), Timing::Late);
        assert_eq!(
            r.timing(&ev(A, true, 1100), &g, 500, 15),
            Timing::Irrelevant
        );
    }
}
