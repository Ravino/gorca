// M7a re-review probe: appended as `mod probe_m7a_rereview_browse` at the end of browse/tests.rs.
use super::*;

fn type_a(r: &mut Rig) -> ReaderEffects {
    r.now += Duration::from_millis(50);
    let ev = KeyEvent {
        keycode: 38,
        keysym: 0x61,
        unichar: 'a' as u32,
        modifiers: Modifiers::empty(),
        pressed: true,
    };
    r.reader.handle(
        ReaderMsg::Key(KeyIntent {
            event: ev,
            class: KeyClass::PassToApp,
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: r.now,
            epoch: Epoch(r.epoch.current().0 + 1),
        }),
        r.now,
    )
}

/// The merged test `typing_in_a_web_field_in_focus_mode_is_echoed_once` says the field is a
/// Firefox (UTF-16) one, but the rig never marks the app non-scalar: the M3 caret layer tracks
/// the field and owns the echo. Which layer spoke?
#[test]
fn probe_which_layer_echoes_in_the_merged_test() {
    let mut r = rig();
    r.focus(1014);
    let tracked = r.reader.text.caret.tracked().is_some();
    let fx = type_a(&mut r);
    eprintln!("scalar rig: tracked={tracked} spoken={:?} keys_seen={}", spoken(&fx), r.reader.text_counters().keys_seen);
    assert!(tracked, "the M3 layer does not track the rig's field");
}

/// The real Firefox path: app marked non-scalar before focus.
#[test]
fn probe_firefox_non_scalar_field_echo_is_generic_and_once() {
    let mut r = rig();
    r.reader.dir.set_text_scalar(app(), false);
    r.focus(1014);
    let tracked = r.reader.text.caret.tracked().is_some();
    let fx = type_a(&mut r);
    eprintln!("non-scalar: tracked={tracked} spoken={:?}", spoken(&fx));
    assert!(!tracked);
    assert_eq!(spoken(&fx), ["a"]);
}

fn arms(fx: &ReaderEffects) -> Vec<Instant> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::ArmTimer(t) => Some(*t),
            _ => None,
        })
        .collect()
}

/// The runtime has ONE timer slot (runtime.rs: `timer = min(timer, at)`, cleared when it
/// fires). A `Timer` that fires for another subsystem before a settling focus is due must
/// re-arm the focus deadline, or the focus is never presented. Does the Reader re-arm it?
#[test]
fn probe_an_early_timer_drops_the_focus_settle_deadline() {
    let mut r = rig();
    r.mirror.apply(
        SnapshotBatch {
            nodes: vec![snap(4000, Role::MenuItem, "Open", StateSet::FOCUSABLE)],
            ..Default::default()
        },
        ApplyOrigin::Push,
    );
    r.focus(1006);
    // A flapping role settles for FOCUS_FLAP_DELAY before it is presented.
    let fx = r.focus(4000);
    let due = arms(&fx);
    eprintln!("focus on a menu item: spoken {:?}, timers {:?}", spoken(&fx), due.iter().map(|t| t.saturating_duration_since(r.now)).collect::<Vec<_>>());
    assert!(spoken(&fx).is_empty() && !due.is_empty());
    // Another subsystem's timer fires first (scroll debounce, caret hold, say-all watchdog...).
    let early = r.now + Duration::from_millis(5);
    let fx = r.reader.handle(ReaderMsg::Timer, early);
    let rearm = arms(&fx);
    eprintln!("early timer: spoken {:?}, re-armed {:?}, focus state {:?}", spoken(&fx), rearm, r.reader.focus);
    assert!(rearm.is_empty(), "the focus deadline was re-armed");
    assert!(matches!(r.reader.focus, crate::focus::FocusState::Settling { .. }));
}

/// Same for the browse `Loading` grace (LOADING_GRACE, 1 s).
#[test]
fn probe_an_early_timer_drops_the_loading_grace() {
    let mut r = rig();
    r.focus(3000); // cold, not mirrored: Loading
    let fx = r.reader.handle(
        ReaderMsg::Doc(DocNotice::InDocument { focus: nk(3000), doc: DocId(1) }),
        r.now,
    );
    let _ = fx;
    let early = r.now + Duration::from_millis(100);
    let fx = r.reader.handle(ReaderMsg::Timer, early);
    eprintln!("early timer in Loading: re-armed {:?}, mode {:?}", arms(&fx), r.mode());
    assert!(arms(&fx).is_empty());
}
