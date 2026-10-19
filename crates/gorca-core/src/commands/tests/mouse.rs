// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer gate and mouse command answers through the Reader (ADR-0025 MO-3).

use super::*;
use crate::mode::GatedGrabs;
use gorca_input_api::GrabSet;
use gorca_output_api::PointerAvail;

const READY: PointerAvail = PointerAvail::Ready { version: 2 };

fn chords() -> Vec<gorca_input_api::Chord> {
    [0xffaf_u32, 0xffaa]
        .iter()
        .map(|k| gorca_input_api::Chord {
            keysym: *k,
            modifiers: Modifiers::empty(),
        })
        .collect()
}

/// A fixture whose keys carry the gated chords and one modifier.
fn gated_fixture() -> Fx {
    let mut f = fixture();
    f.reader.keys = ModeKeys {
        modifiers: vec![0xff9e],
        gated: vec![GatedGrabs {
            gate: RuntimeGate::Pointer,
            chords: Vec::new(),
            exact: chords(),
        }],
        ..ModeKeys::default()
    };
    f
}

fn grab_sets(fx: &ReaderEffects) -> Vec<GrabSet> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::SetGrabSet(g) = e {
                Some(g.clone())
            } else {
                None
            }
        })
        .collect()
}

fn timers(fx: &ReaderEffects) -> Vec<Instant> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::ArmTimer(t) = e {
                Some(*t)
            } else {
                None
            }
        })
        .collect()
}

fn avail(f: &mut Fx, a: PointerAvail, at: Duration) -> ReaderEffects {
    f.reader.handle(
        ReaderMsg::Control(ControlMsg::Pointer(PointerMsg::Avail(a))),
        f.now + at,
    )
}

fn tick(f: &mut Fx, at: Duration) -> ReaderEffects {
    f.reader.handle(ReaderMsg::Timer, f.now + at)
}

#[test]
fn the_gate_opens_after_two_seconds_with_one_set_key_grabs_and_closes_at_once() {
    let mut f = gated_fixture();
    let fx = avail(&mut f, READY, Duration::ZERO);
    assert!(
        grab_sets(&fx).is_empty(),
        "no grab change before the damping"
    );
    assert_eq!(timers(&fx), [f.now + Duration::from_secs(2)]);
    assert!(grab_sets(&tick(&mut f, Duration::from_millis(1999))).is_empty());
    let sets = grab_sets(&tick(&mut f, Duration::from_secs(2)));
    assert_eq!(
        sets.len(),
        1,
        "exactly one SetKeyGrabs generation per opening (MI-4)"
    );
    assert!(chords().iter().all(|c| sets[0].exact.contains(c)));
    assert!(
        !sets[0].grab_all,
        "never GrabKeyboard/UngrabKeyboard for a gate change (RG-4)"
    );
    assert_eq!(sets[0].modifiers.len(), 1);
    // Unresponsive keeps the keys grabbed: no grab change at all (C-3).
    assert!(
        grab_sets(&avail(
            &mut f,
            PointerAvail::Unresponsive,
            Duration::from_secs(5)
        ))
        .is_empty()
    );
    assert!(grab_sets(&avail(&mut f, READY, Duration::from_secs(9))).is_empty());
    // Every state in which the helper is gone closes at once, with one grab change.
    for gone in [
        PointerAvail::Absent,
        PointerAvail::Squatted,
        PointerAvail::Denied,
        PointerAvail::TooOld { version: 1 },
        PointerAvail::NoCap,
    ] {
        let mut g = gated_fixture();
        avail(&mut g, READY, Duration::ZERO);
        tick(&mut g, Duration::from_secs(2));
        let sets = grab_sets(&avail(&mut g, gone, Duration::from_secs(3)));
        assert_eq!(sets.len(), 1, "{gone:?}");
        assert!(
            chords().iter().all(|c| !sets[0].exact.contains(c)),
            "{gone:?}"
        );
    }
}

#[test]
fn a_helper_that_flaps_never_costs_a_grab_change() {
    let mut f = gated_fixture();
    let mut total = 0;
    for i in 0..40_u64 {
        let a = if i % 2 == 0 {
            READY
        } else {
            PointerAvail::Absent
        };
        let at = Duration::from_millis(i * 100);
        total += grab_sets(&avail(&mut f, a, at)).len();
        total += grab_sets(&tick(&mut f, at + Duration::from_millis(50))).len();
    }
    assert_eq!(
        total, 0,
        "a helper that never holds for 2 s never moves the grabs"
    );
}

#[test]
fn a_settings_reload_keeps_the_open_gate() {
    let mut f = gated_fixture();
    avail(&mut f, READY, Duration::ZERO);
    tick(&mut f, Duration::from_secs(2));
    let mut l = (*loaded("", LoadCause::Startup)).clone();
    l.mode_keys.gated = f.reader.keys.gated.clone();
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(Arc::new(l))));
    let sets = grab_sets(&fx);
    assert_eq!(sets.len(), 1);
    assert!(
        chords().iter().all(|c| sets[0].exact.contains(c)),
        "the gate state survives a snapshot"
    );
}

#[test]
fn mouse_commands_say_why_the_mouse_is_not_available() {
    let say = |f: &mut Fx| spoken(&f.run(CoreCmd::MouseLeftClick, 1));
    let mut f = fixture();
    assert_eq!(
        say(&mut f),
        ["Mouse not available: gorca helper is not running"]
    );
    avail(&mut f, PointerAvail::TooOld { version: 1 }, Duration::ZERO);
    assert_eq!(
        say(&mut f),
        ["Mouse not available: gorca helper is too old"]
    );
    avail(&mut f, PointerAvail::NoCap, Duration::ZERO);
    assert_eq!(
        say(&mut f),
        ["Mouse not available: allow pointer in ~/.config/gorca/shell-helper.json"]
    );
    avail(&mut f, PointerAvail::Denied, Duration::ZERO);
    assert_eq!(
        say(&mut f),
        ["Mouse not available: gorca is not allowed by the helper"]
    );
    avail(&mut f, PointerAvail::Unresponsive, Duration::ZERO);
    assert_eq!(
        say(&mut f),
        ["Mouse not responding"],
        "transiently unresponsive: keys stay grabbed, the command says so"
    );
    f.control(ControlMsg::Pointer(PointerMsg::Enabled(false)));
    assert_eq!(say(&mut f), ["Mouse emulation is off"]);
}

#[test]
fn learn_mode_describes_a_gated_command_with_a_note_while_the_gate_is_closed() {
    let mut f = gated_fixture();
    let describe = |f: &mut Fx| {
        let intent = intent(CoreCmd::MouseRightClick, 1, 1);
        let mut fx = ReaderEffects::new();
        f.reader
            .describe(&intent, Some(CoreCmd::MouseRightClick.id()), &mut fx);
        spoken(&fx).join(" ")
    };
    let closed = describe(&mut f);
    assert!(
        closed.contains("click the right mouse button at the mouse position"),
        "{closed}"
    );
    assert!(closed.ends_with("not available now"), "{closed}");
    avail(&mut f, READY, Duration::ZERO);
    tick(&mut f, Duration::from_secs(2));
    let open = describe(&mut f);
    assert!(!open.contains("not available now"), "{open}");
}
