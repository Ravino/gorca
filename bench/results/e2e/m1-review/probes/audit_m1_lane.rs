//! Reviewer audit probe (not part of the branch): lane state across a monitor restart.
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_input_api::{
    Bound, ChordKey, CmdId, CompiledKeymap, GrabSet, GrabView, InputLane, KeyClass, KeyEvent,
    KeyRef, LaneLinks, Modifiers, TableKind, sym,
};
use gorca_rt::EpochCell;

const KC_INS: u16 = 118;
const KC_T: u16 = 28;

fn ev(code: u16, keysym: u32, pressed: bool) -> KeyEvent {
    KeyEvent { keycode: code, keysym, unichar: 0, modifiers: Modifiers::empty(), pressed }
}

#[test]
fn a_plain_key_after_a_monitor_restart_is_not_a_command() {
    let links = LaneLinks::new();
    let mut km = CompiledKeymap::new(vec![sym::INSERT]);
    km.insert(
        TableKind::Global,
        ChordKey { mods: Modifiers::GORCA, key: KeyRef::Code(KC_T) },
        1,
        Some(Bound { cmd: CmdId(1), active: true, repeatable: false, max_press: 3, bypass: false, interrupts: true }),
    );
    links.keymap.store(Arc::new(km));
    let mut g = GrabSet::default();
    g.modifiers.insert(sym::INSERT);
    let g = Arc::new(g);
    links.view.store(Arc::new(GrabView { committed_gen: 1, committed: Arc::clone(&g), wanted_gen: None, wanted: None }));
    let t0 = Instant::now();
    let mut lane = InputLane::new(Arc::clone(&links), Arc::new(EpochCell::new()), t0);
    // Insert goes down; then gnome-shell restarts. The release is never delivered (the old
    // connection is gone). The new session re-applies the same grab set (generation 1 again).
    let o = lane.on_key(ev(KC_INS, sym::INSERT, true), t0 + Duration::from_millis(1000));
    assert_eq!(o.intent.class, KeyClass::Swallowed);
    links.session.fetch_add(2, std::sync::atomic::Ordering::AcqRel);
    links.view.store(Arc::new(GrabView { committed_gen: 1, committed: Arc::clone(&g), wanted_gen: None, wanted: None }));
    // A fresh Mutter has no key down: a plain T reaches the application.
    let o = lane.on_key(ev(KC_T, 0x74, true), t0 + Duration::from_millis(5000));
    assert_eq!(
        o.intent.class,
        KeyClass::PassToApp,
        "BUG: stale lane state after a restart turns a plain T into gorca+T"
    );
}
