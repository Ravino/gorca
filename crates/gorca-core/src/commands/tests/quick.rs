//! F0 quick wins: lock keys, capital pitch, speech history, sleep mode, battery.

use super::*;
use gorca_model::SegmentKind;

fn key_msg(class: KeyClass, keysym: u32, unichar: u32, mods: Modifiers, epoch: u64) -> ReaderMsg {
    ReaderMsg::Key(KeyIntent {
        class,
        event: KeyEvent {
            keycode: 66,
            keysym,
            unichar,
            modifiers: mods,
            pressed: true,
        },
        ..intent(CoreCmd::SpeechStop, 1, epoch)
    })
}

const CAPS: u32 = 0xffe5;
const NUM: u32 = 0xff7f;
const SCROLL: u32 = 0xff14;

fn say(f: &mut Fx, class: KeyClass, keysym: u32, mods: Modifiers) -> Vec<String> {
    spoken(&f.reader.handle(key_msg(class, keysym, 0, mods, 1), f.now))
}

#[test]
fn lock_keys_are_announced_with_the_state_they_switch_to() {
    let mut f = fixture();
    // The event carries the state before the press: the new state is the inverse.
    let none = Modifiers::empty();
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, CAPS, none),
        ["Caps Lock on"]
    );
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, CAPS, Modifiers::LOCK),
        ["Caps Lock off"]
    );
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, NUM, Modifiers::MOD2),
        ["Num Lock off"]
    );
    assert_eq!(say(&mut f, KeyClass::PassToApp, NUM, none), ["Num Lock on"]);
    // Scroll Lock has no modifier bit: it is counted.
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, SCROLL, none),
        ["Scroll Lock on"]
    );
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, SCROLL, none),
        ["Scroll Lock off"]
    );
}

#[test]
fn caps_lock_as_the_gorca_modifier_announces_only_the_double_tap() {
    let mut f = fixture();
    let none = Modifiers::empty();
    // A single tap is swallowed by Mutter: it is the modifier, nothing toggles, nothing is said.
    assert!(say(&mut f, KeyClass::Swallowed, CAPS, none).is_empty());
    // The double tap passes the second press to the application, which toggles Caps Lock.
    assert_eq!(
        say(&mut f, KeyClass::ModifierPassThrough, CAPS, none),
        ["Caps Lock on"]
    );
    // A release says nothing.
    let mut rel = key_msg(KeyClass::PassToApp, CAPS, 0, Modifiers::LOCK, 3);
    if let ReaderMsg::Key(k) = &mut rel {
        k.event.pressed = false;
        k.kind = PressKind::Release;
    }
    assert!(spoken(&f.reader.handle(rel, f.now)).is_empty());
}

#[test]
fn lock_keys_are_announced_in_the_speech_language() {
    let mut f = fixture();
    f.reader.cmd.file.speech.language = "ru".to_owned();
    f.reader.cmd.republish();
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, CAPS, Modifiers::empty()),
        ["Caps Lock включён"]
    );
    assert_eq!(
        say(&mut f, KeyClass::PassToApp, NUM, Modifiers::MOD2),
        ["Num Lock выключен"]
    );
}

fn kinds(fx: &ReaderEffects) -> Vec<SegmentKind> {
    fx.iter()
        .filter_map(|e| match e {
            ReaderEffect::Speak(s) => s.utterance.segments.first().map(|g| g.kind),
            ReaderEffect::Fetch { .. }
            | ReaderEffect::Demand { .. }
            | ReaderEffect::FocusSettled { .. }
            | ReaderEffect::QueryActiveWindowFocus
            | ReaderEffect::ArmTimer(_)
            | ReaderEffect::SetGrabSet(_)
            | ReaderEffect::SpeechCtl(_)
            | ReaderEffect::Config(_)
            | ReaderEffect::ReviewNav { .. }
            | ReaderEffect::ReviewBuild(_)
            | ReaderEffect::ReviewRevalidate { .. }
            | ReaderEffect::ReviewStatusBar { .. }
            | ReaderEffect::ReviewAct { .. }
            | ReaderEffect::Web(_)
            | ReaderEffect::Desktop(_)
            | ReaderEffect::TextFetch { .. }
            | ReaderEffect::SayAllChunk { .. } => None,
        })
        .collect()
}

fn typed(f: &mut Fx, ch: char, mods: Modifiers) -> ReaderEffects {
    f.reader.handle(
        key_msg(KeyClass::PassToApp, ch as u32, ch as u32, mods, 1),
        f.now,
    )
}

#[test]
fn a_typed_capital_is_marked_for_pitch_a_lower_case_letter_is_not() {
    let mut f = fixture();
    // Shift+a and a letter typed with Caps Lock on both arrive as the capital character.
    for mods in [Modifiers::SHIFT, Modifiers::LOCK] {
        let fx = typed(&mut f, 'A', mods);
        assert_eq!(spoken(&fx), ["A"]);
        assert_eq!(kinds(&fx), [SegmentKind::Capital]);
    }
    let fx = typed(&mut f, 'a', Modifiers::empty());
    assert_eq!(kinds(&fx), [SegmentKind::Said]);
    // Cyrillic capitals count.
    let fx = typed(&mut f, 'Ж', Modifiers::SHIFT);
    assert_eq!(kinds(&fx), [SegmentKind::Capital]);
}

#[test]
fn capital_say_mode_speaks_the_word_and_off_does_nothing() {
    let mut f = fixture();
    f.reader.cmd.file.speech.caps = gorca_config::Caps::Say;
    f.reader.cmd.republish();
    assert_eq!(spoken(&typed(&mut f, 'B', Modifiers::SHIFT)), ["cap B"]);
    f.reader.cmd.file.speech.language = "ru".to_owned();
    f.reader.cmd.republish();
    assert_eq!(
        spoken(&typed(&mut f, 'B', Modifiers::SHIFT)),
        ["заглавная B"]
    );
    f.reader.cmd.file.speech.caps = gorca_config::Caps::Off;
    f.reader.cmd.republish();
    let fx = typed(&mut f, 'B', Modifiers::SHIFT);
    assert_eq!(spoken(&fx), ["B"]);
    assert_eq!(kinds(&fx), [SegmentKind::Said]);
}

/// Speech made outside a key press, as the Reader hands it on: recorded in the history.
fn utter(f: &mut Fx, text: &str) {
    let mut fx = ReaderEffects::new();
    f.reader.announce(text, &mut fx);
    f.reader.after_message(true, &mut fx);
}

#[test]
fn history_steps_back_and_forward_and_stops_at_the_ends() {
    let mut f = fixture();
    assert_eq!(
        spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)),
        ["no speech history"]
    );
    for t in ["one", "two", "three"] {
        utter(&mut f, t);
    }
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["three"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["two"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["one"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["oldest"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryNext, 1)), ["two"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryNext, 1)), ["three"]);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryNext, 1)), ["newest"]);
    // Reading the history does not add to it, and new speech restarts the walk at the newest.
    assert_eq!(f.reader.cmd.quick.history.items_len(), 3);
    utter(&mut f, "four");
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["four"]);
}

#[test]
fn history_is_bounded_and_keeps_the_newest() {
    let mut f = fixture();
    for i in 0..(crate::HISTORY_CAP + 7) {
        utter(&mut f, &format!("u{i}"));
    }
    assert_eq!(f.reader.cmd.quick.history.items_len(), crate::HISTORY_CAP);
    let last = format!("u{}", crate::HISTORY_CAP + 6);
    assert_eq!(
        spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)),
        [last.as_str()]
    );
}

#[test]
fn history_never_keeps_secrets_key_echo_or_typing_echo() {
    let mut f = fixture();
    // A secret utterance (a password field's text).
    let mut fx = ReaderEffects::new();
    f.reader.announce("hunter2", &mut fx);
    if let Some(ReaderEffect::Speak(s)) = fx.first() {
        f.reader.text.sensitive_utt = Some(s.utterance.id);
    }
    f.reader.after_message(true, &mut fx);
    // The history holds a placeholder, never the text; stepping back speaks the placeholder.
    assert_eq!(f.reader.cmd.quick.history.items_len(), 1);
    assert_eq!(spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1)), ["password"]);
    f.reader.text.sensitive_utt = None;
    // Typed-character echo: a key message that is not a command.
    typed(&mut f, 'x', Modifiers::empty());
    // Typing echo reported by the text layer.
    let mut fx = ReaderEffects::new();
    f.reader.announce("typed", &mut fx);
    if let Some(ReaderEffect::Speak(s)) = fx.first() {
        f.reader.cmd.quick.echo_ids.push(s.utterance.id);
    }
    f.reader.after_message(true, &mut fx);
    assert_eq!(f.reader.cmd.quick.history.items_len(), 1);
    utter(&mut f, "kept");
    assert_eq!(f.reader.cmd.quick.history.items_len(), 2);
    for _ in 0..3 {
        let t = spoken(&f.run(CoreCmd::SpeechHistoryPrev, 1));
        assert!(!t.concat().contains("hunter2"), "{t:?}");
    }
}

#[test]
fn sleep_mode_silences_the_focused_application_until_toggled_back() {
    let mut f = fixture();
    f.focus(1);
    let fx = f.run(CoreCmd::InputSleepToggle, 1);
    assert_eq!(spoken(&fx), ["sleep mode on"]);
    // Commands do not run and nothing is spoken in the sleeping application.
    assert!(spoken(&f.run(CoreCmd::InfoTimeDate, 1)).is_empty());
    let mut fx = ReaderEffects::new();
    f.reader.announce("hello", &mut fx);
    f.reader.after_message(true, &mut fx);
    assert!(spoken(&fx).is_empty());
    // The toggle itself still answers, and wakes the application.
    assert_eq!(
        spoken(&f.run(CoreCmd::InputSleepToggle, 1)),
        ["sleep mode off"]
    );
    assert!(!spoken(&f.run(CoreCmd::InfoTimeDate, 1)).is_empty());
}

#[test]
fn sleep_mode_is_remembered_per_application() {
    let mut f = fixture();
    f.focus(1);
    f.run(CoreCmd::InputSleepToggle, 1);
    let other = AppId {
        connector: ConnectorId(1),
        serial: 2,
    };
    assert!(f.reader.cmd.quick.sleeping.contains(&app()));
    assert!(!f.reader.cmd.quick.sleeping.contains(&other));
    // Leaving and coming back finds it still asleep.
    f.reader.focus = crate::focus::FocusState::NoFocus;
    assert!(!f.reader.sleeping_now());
    f.reader.focus = crate::focus::FocusState::Settling {
        candidate: key(1),
        epoch: f.epoch.current(),
        until: f.now,
    };
    assert!(f.reader.sleeping_now());
}

#[test]
fn battery_is_a_desktop_request_and_the_answer_is_localised() {
    let mut f = fixture();
    let fx = f.run(CoreCmd::InfoBattery, 1);
    assert!(fx.iter().any(|e| matches!(
        e,
        ReaderEffect::Desktop(crate::DesktopEffect::Battery { .. })
    )));
    let epoch = f.epoch.current();
    let info = crate::PowerInfo {
        present: true,
        percent: 82,
        state: crate::BatteryState::Discharging,
        secs_left: Some(3 * 3600 + 5 * 60),
    };
    let ans = |f: &mut Fx, i, e| {
        spoken(&f.reader.handle(
            ReaderMsg::Background(crate::BackgroundMsg::Battery { info: i, epoch: e }),
            f.now,
        ))
    };
    assert_eq!(
        ans(&mut f, Some(info), epoch),
        ["82 percent, discharging, 3 h 5 min remaining"]
    );
    assert_eq!(ans(&mut f, None, epoch), ["battery status unavailable"]);
    // A key pressed since makes the answer stale.
    let newer = f.epoch.advance();
    assert!(ans(&mut f, Some(info), epoch).is_empty());
    assert!(!ans(&mut f, Some(info), newer).is_empty());
    f.reader.cmd.file.speech.language = "ru".to_owned();
    f.reader.cmd.republish();
    assert_eq!(
        ans(&mut f, Some(info), newer),
        ["82 процентов, разряжается, осталось 3 ч 5 мин"]
    );
}
