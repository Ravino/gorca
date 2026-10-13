//! Reviewer probe (not part of the branch): the overflow resync is applied at the head of the
//! queue, not at the gap left by the dropped event.
#![allow(clippy::unwrap_used)]
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use gorca_input_api::{
    Bound, ChordKey, CmdId, CompiledKeymap, GrabSet, GrabView, InputLane, KeyClass, KeyEvent,
    KeyRef, LaneLinks, Modifiers, TableKind, sym,
};
use gorca_rt::EpochCell;

fn ev(code: u16, keysym: u32, pressed: bool) -> KeyEvent {
    KeyEvent {
        keycode: code,
        keysym,
        unichar: 0,
        modifiers: Modifiers::empty(),
        pressed,
    }
}

#[test]
fn a_release_dropped_on_overflow_still_leaves_the_lane_stale() {
    let links = LaneLinks::new();
    let mut km = CompiledKeymap::new(vec![sym::INSERT]);
    km.insert(
        TableKind::Global,
        ChordKey {
            mods: Modifiers::GORCA,
            key: KeyRef::Code(28),
        },
        1,
        Some(Bound {
            cmd: CmdId(1),
            active: true,
            repeatable: false,
            max_press: 3,
            bypass: false,
            interrupts: true,
        }),
    );
    links.keymap.store(Arc::new(km));
    let mut g = GrabSet::default();
    g.modifiers.insert(sym::INSERT);
    links.view.store(Arc::new(GrabView {
        committed_gen: 1,
        committed: Arc::new(g),
        wanted_gen: None,
        wanted: None,
    }));
    let t0 = Instant::now();
    let mut lane = InputLane::new(Arc::clone(&links), Arc::new(EpochCell::new()), t0);
    // The queue holds [Insert press]; the Insert release does not fit and is dropped; the
    // source bumps the session at that moment. The lane next pops the OLD Insert press.
    links.session.fetch_add(1, Ordering::AcqRel);
    lane.on_key(ev(118, sym::INSERT, true), t0 + Duration::from_millis(1000));
    // (Insert release lost.) Later a plain T: Mutter has no key down and passes it.
    let o = lane.on_key(ev(28, 0x74, true), t0 + Duration::from_millis(3000));
    assert_eq!(
        o.intent.class,
        KeyClass::PassToApp,
        "the resync ran before the queued events, not at the gap"
    );
}
