//! A replica of the a11y key logic in Mutter (ADR-0005 4.3), line by line.
//!
//! Sources (`refs/mutter/src/backends`):
//! - `native/meta-seat-impl.c`: `is_a11y_modifier_first_click`, called for every non-autorepeat
//!   key event with the event's keysym;
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
}

/// One key event as the replica needs it.
#[derive(Copy, Clone, Debug)]
pub struct Ev {
    pub keysym: u32,
    pub pressed: bool,
    /// Autorepeat (a press of a key already down): Mutter skips the first-click logic.
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
        if ev.repeat
            || !grabs.modifiers.contains(&ev.keysym)
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
                self.pressed_modifiers.push(ev.keysym);
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
        })
    }

    /// Process one event. `soon` is the forced outcome of the time comparison; pass the nominal
    /// one for [`Timing::Soon`]/[`Timing::Late`] and either for [`Timing::Band`].
    pub fn step(&mut self, ev: &Ev, grabs: &GrabSet, soon: bool) -> Verdict {
        let gorca_held = grabs
            .modifiers
            .iter()
            .any(|m| self.grabbed_keypresses.contains(m));
        let ignorable = if ev.repeat {
            false
        } else {
            self.first_click(ev, grabs, soon)
        };
        let is_mod = grabs.modifiers.contains(&ev.keysym);
        if is_mod && !ignorable {
            return Verdict {
                swallowed: false,
                gorca_held,
                passed_modifier: ev.pressed,
                modifier_swallowed: false,
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
        }
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

    #[test]
    fn autorepeat_of_a_held_modifier_leaks_to_the_app_pf5() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        let rep = Ev {
            repeat: true,
            ..ev(INS, true, 1600)
        };
        let v = r.step(&rep, &g, false);
        assert!(!v.swallowed && v.passed_modifier, "{v:?}");
    }

    #[test]
    fn autorepeat_does_not_touch_the_double_tap_state() {
        let (mut r, g) = (Replica::default(), grabs());
        run(&mut r, &g, ev(INS, true, 1000));
        run(&mut r, &g, ev(T, true, 1010));
        let before = r.clone();
        let rep = Ev {
            repeat: true,
            ..ev(T, true, 1500)
        };
        let v = r.step(&rep, &g, false);
        assert!(v.swallowed);
        assert_eq!(r.last_keysym, before.last_keysym);
        assert_eq!(r.last_time, before.last_time);
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
