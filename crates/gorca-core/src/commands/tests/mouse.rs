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

// ---- MO-4: click at the pointer, drag lock, position ----------------------------------------

use crate::commands::{PointerDone, PointerOp, PointerReply, PointerReq, PtrReqId};
use gorca_output_api::{Button, ButtonAct, OutcomeKind, PointerError, PointerOutcome, PointerPos};

fn ready_fixture() -> Fx {
    let mut f = gated_fixture();
    avail(&mut f, READY, Duration::ZERO);
    tick(&mut f, Duration::from_secs(2));
    f
}

fn pointer_reqs(fx: &ReaderEffects) -> Vec<PointerReq> {
    fx.iter()
        .filter_map(|e| {
            if let ReaderEffect::Pointer(r) = e {
                Some(*r)
            } else {
                None
            }
        })
        .collect()
}

fn outcome(held: u32) -> PointerOutcome {
    PointerOutcome {
        kind: OutcomeKind::Done,
        x: 10.0,
        y: 20.0,
        window_at: 0,
        occluded: false,
        clamped: false,
        held,
    }
}

fn reply(
    f: &mut Fx,
    id: PtrReqId,
    result: Result<PointerDone, PointerError>,
    at: Duration,
) -> ReaderEffects {
    f.reader.handle(
        ReaderMsg::Control(ControlMsg::PointerReply(PointerReply { id, result })),
        f.now + at,
    )
}

fn timers_of(fx: &ReaderEffects) -> Vec<Instant> {
    timers(fx)
}

#[test]
fn a_click_speaks_first_and_sends_one_pointer_request() {
    let mut f = ready_fixture();
    for (cmd, word, b) in [
        (CoreCmd::MouseLeftClick, "Left click", Button::Left),
        (CoreCmd::MouseRightClick, "Right click", Button::Right),
        (CoreCmd::MouseMiddleClick, "Middle click", Button::Middle),
    ] {
        let fx = f.run(cmd, 1);
        assert_eq!(spoken(&fx), [word]);
        let reqs = pointer_reqs(&fx);
        assert_eq!(reqs.len(), 1, "{word}");
        assert_eq!(reqs[0].op, PointerOp::Click(b, ButtonAct::Click));
        // The speech is the first effect: the click is never awaited for it (EX-4).
        assert!(matches!(fx[0], ReaderEffect::Speak(_)));
        reply(
            &mut f,
            reqs[0].id,
            Ok(PointerDone::Outcome(outcome(0))),
            Duration::from_millis(3),
        );
    }
}

#[test]
fn a_click_the_shell_ran_late_is_reported_not_silently_lost() {
    let mut f = ready_fixture();
    let id = pointer_reqs(&f.run(CoreCmd::MouseLeftClick, 1))[0].id;
    let mut o = outcome(0);
    o.kind = OutcomeKind::Expired;
    let fx = reply(
        &mut f,
        id,
        Ok(PointerDone::Outcome(o)),
        Duration::from_millis(200),
    );
    assert_eq!(spoken(&fx), ["Mouse not responding"]);
}

#[test]
fn pointer_errors_are_spoken() {
    for (err, text) in [
        (
            PointerError::Locked,
            "Not available while the screen is locked",
        ),
        (PointerError::Busy, "Mouse busy"),
        (PointerError::RateLimited, "Mouse busy"),
        (PointerError::NoWindow, "No location"),
        (PointerError::Timeout, "Mouse not responding"),
        (PointerError::NotResponding, "Mouse not responding"),
    ] {
        let mut f = ready_fixture();
        let id = pointer_reqs(&f.run(CoreCmd::MouseLeftClick, 1))[0].id;
        let fx = reply(&mut f, id, Err(err.clone()), Duration::from_millis(5));
        assert_eq!(spoken(&fx), [text], "{err:?}");
    }
}

#[test]
fn the_left_lock_presses_speaks_locked_arms_120_s_and_unlocks() {
    let mut f = ready_fixture();
    let fx = f.run(CoreCmd::MouseLeftLockToggle, 1);
    let reqs = pointer_reqs(&fx);
    assert_eq!(reqs[0].op, PointerOp::Press(Button::Left));
    assert!(
        spoken(&fx).is_empty(),
        "locked is spoken when the helper confirms (hold table)"
    );
    let at = Duration::from_millis(4);
    let fx = reply(&mut f, reqs[0].id, Ok(PointerDone::Outcome(outcome(1))), at);
    assert_eq!(spoken(&fx), ["Left button locked"]);
    assert_eq!(timers_of(&fx), [f.now + at + Duration::from_secs(120)]);
    // A click of the locked button releases it instead.
    let fx = f.run(CoreCmd::MouseLeftClick, 1);
    let reqs = pointer_reqs(&fx);
    assert_eq!(reqs[0].op, PointerOp::Release(Button::Left));
    let fx = reply(&mut f, reqs[0].id, Ok(PointerDone::Outcome(outcome(0))), at);
    assert_eq!(spoken(&fx), ["Left button unlocked"]);
    // The right button locks independently.
    let reqs = pointer_reqs(&f.run(CoreCmd::MouseRightLockToggle, 1));
    assert_eq!(reqs[0].op, PointerOp::Press(Button::Right));
    let fx = reply(&mut f, reqs[0].id, Ok(PointerDone::Outcome(outcome(4))), at);
    assert_eq!(spoken(&fx), ["Right button locked"]);
}

#[test]
fn a_toggle_while_the_press_is_in_flight_is_busy() {
    let mut f = ready_fixture();
    f.run(CoreCmd::MouseLeftLockToggle, 1);
    assert_eq!(
        spoken(&f.run(CoreCmd::MouseLeftLockToggle, 1)),
        ["Mouse busy"]
    );
}

#[test]
fn the_lock_releases_itself_at_120_seconds_with_a_spoken_notice() {
    let mut f = ready_fixture();
    let id = pointer_reqs(&f.run(CoreCmd::MouseLeftLockToggle, 1))[0].id;
    let t0 = Duration::from_millis(1);
    reply(&mut f, id, Ok(PointerDone::Outcome(outcome(1))), t0);
    // Not yet at 119 s: the timer only re-arms.
    let fx = tick(&mut f, t0 + Duration::from_secs(119));
    assert!(pointer_reqs(&fx).is_empty());
    assert_eq!(timers_of(&fx), [f.now + t0 + Duration::from_secs(120)]);
    let fx = tick(&mut f, t0 + Duration::from_secs(120));
    let reqs = pointer_reqs(&fx);
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].op, PointerOp::Release(Button::Left));
    let fx = reply(
        &mut f,
        reqs[0].id,
        Ok(PointerDone::Outcome(outcome(0))),
        t0 + Duration::from_secs(121),
    );
    assert_eq!(spoken(&fx), ["Left button released after 2 minutes"]);
}

#[test]
fn a_failed_lock_goes_unknown_and_the_next_toggle_releases_everything() {
    let mut f = ready_fixture();
    let id = pointer_reqs(&f.run(CoreCmd::MouseLeftLockToggle, 1))[0].id;
    let fx = reply(
        &mut f,
        id,
        Err(PointerError::Timeout),
        Duration::from_millis(150),
    );
    assert_eq!(spoken(&fx), ["Mouse not responding"]);
    let fx = f.run(CoreCmd::MouseLeftLockToggle, 1);
    let reqs = pointer_reqs(&fx);
    assert_eq!(reqs[0].op, PointerOp::ReleaseAll);
    let fx = reply(
        &mut f,
        reqs[0].id,
        Ok(PointerDone::Released(0)),
        Duration::from_millis(160),
    );
    assert_eq!(spoken(&fx), ["Left button unlocked"]);
}

#[test]
fn losing_the_helper_forgets_the_lock_and_switching_the_mouse_off_releases_it() {
    let mut f = ready_fixture();
    let id = pointer_reqs(&f.run(CoreCmd::MouseLeftLockToggle, 1))[0].id;
    reply(
        &mut f,
        id,
        Ok(PointerDone::Outcome(outcome(1))),
        Duration::from_millis(1),
    );
    // mouse.enabled = false while usable: a best-effort ReleaseAll goes out before the gate closes.
    let fx = f.control(ControlMsg::Pointer(PointerMsg::Enabled(false)));
    assert_eq!(
        pointer_reqs(&fx).iter().map(|r| r.op).collect::<Vec<_>>(),
        [PointerOp::ReleaseAll]
    );
    assert_eq!(
        grab_sets(&fx).len(),
        1,
        "and the gate closes with one grab change"
    );
    // Back on, helper present: the lock state is fresh.
    f.control(ControlMsg::Pointer(PointerMsg::Enabled(true)));
    tick(&mut f, Duration::from_secs(10));
    let reqs = pointer_reqs(&f.run(CoreCmd::MouseLeftLockToggle, 1));
    assert_eq!(
        reqs[0].op,
        PointerOp::Press(Button::Left),
        "a fresh press, not a release"
    );
    // Helper vanishes while pressed: the state is dropped.
    reply(
        &mut f,
        reqs[0].id,
        Ok(PointerDone::Outcome(outcome(1))),
        Duration::from_secs(11),
    );
    avail(&mut f, PointerAvail::Absent, Duration::from_secs(12));
    assert_eq!(
        spoken(&f.run(CoreCmd::MouseLeftLockToggle, 1)),
        ["Mouse not available: gorca helper is not running"]
    );
}

#[test]
fn say_position_reads_the_window_position_once_and_the_screen_position_twice() {
    let mut f = ready_fixture();
    let pos = PointerPos {
        x: 310.4,
        y: 200.6,
        window: 42,
        wx: 210.0,
        wy: 100.0,
        monitor: 1,
    };
    let id = pointer_reqs(&f.run(CoreCmd::MouseSayPosition, 1))[0].id;
    let fx = reply(
        &mut f,
        id,
        Ok(PointerDone::Pos(pos.clone())),
        Duration::from_millis(2),
    );
    assert_eq!(spoken(&fx), ["window, 210, 100"]);
    let id = pointer_reqs(&f.run(CoreCmd::MouseSayPosition, 2))[0].id;
    let fx = reply(
        &mut f,
        id,
        Ok(PointerDone::Pos(pos)),
        Duration::from_millis(3),
    );
    assert_eq!(spoken(&fx), ["Screen 310, 201, monitor 2"]);
}

#[test]
fn routing_and_item_clicks_say_no_location_until_the_resolver_exists() {
    let mut f = ready_fixture();
    for c in [
        CoreCmd::MouseRouteToReview,
        CoreCmd::MouseReviewToPointer,
        CoreCmd::MouseLeftClickItem,
        CoreCmd::MouseRightClickItem,
    ] {
        assert_eq!(spoken(&f.run(c, 1)), ["No location"], "{}", c.name());
    }
}

#[test]
fn mouse_enabled_in_the_settings_drives_the_gate() {
    let mut f = ready_fixture();
    assert!(f.reader.cmd.pointer.is_open());
    let mut l = (*loaded("[mouse]\nenabled = false\n", LoadCause::Startup)).clone();
    l.mode_keys.gated = f.reader.keys.gated.clone();
    let fx = f.control(ControlMsg::Settings(SettingsMsg::Loaded(Arc::new(l))));
    assert!(!f.reader.cmd.pointer.is_open());
    let sets = grab_sets(&fx);
    assert!(!sets.is_empty());
    assert!(
        chords()
            .iter()
            .all(|c| !sets.last().is_some_and(|s| s.exact.contains(c)))
    );
}

#[test]
fn a_lost_reply_does_not_wedge_the_lock() {
    let mut f = ready_fixture();
    let first = pointer_reqs(&f.run(CoreCmd::MouseLeftLockToggle, 1));
    assert_eq!(first.len(), 1);
    // The reply never comes (dropped channel, cleared table): the Reader's timer gives up after 1 s.
    let fx = tick(&mut f, Duration::from_millis(1100));
    assert_eq!(spoken(&fx), ["Mouse not responding"]);
    let fx = f.run(CoreCmd::MouseLeftLockToggle, 1);
    assert_eq!(pointer_reqs(&fx)[0].op, PointerOp::ReleaseAll, "recovers instead of answering busy forever");
}
