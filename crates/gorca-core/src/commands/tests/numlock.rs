// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard report bug 3: with Num Lock on the keypad Insert key types "0".

use super::*;

fn kp0(f: &mut Fx, mods: Modifiers, code: u16) -> Vec<String> {
    let mut m = ReaderMsg::Key(KeyIntent {
        class: KeyClass::PassToApp,
        event: KeyEvent {
            keycode: code,
            keysym: 0xffb0,
            unichar: 0x30,
            modifiers: mods,
            pressed: true,
        },
        ..intent(CoreCmd::SpeechStop, 1, 1)
    });
    if let ReaderMsg::Key(k) = &mut m {
        k.class = KeyClass::PassToApp;
    }
    spoken(&f.reader.handle(m, f.now))
}

fn explains(said: &[String]) -> bool {
    said.iter().any(|s| s.contains("Num Lock is on"))
}

/// With Num Lock on, keypad zero is `KP_0`, never the grabbed `KP_Insert`: gorca says why once
/// instead of leaving the user to guess.
#[test]
fn keypad_zero_with_num_lock_on_explains_itself_once() {
    let mut f = fixture();
    f.reader.keys.modifiers = vec![0xff9e];
    // Without Mod2, or on another key, it is not the Num Lock case.
    assert!(!explains(&kp0(&mut f, Modifiers::empty(), 90)));
    assert!(!explains(&kp0(&mut f, Modifiers::MOD2, 91)));
    let said = kp0(&mut f, Modifiers::MOD2, 90);
    assert!(explains(&said), "first press explains: {said:?}");
    let again = kp0(&mut f, Modifiers::MOD2, 90);
    assert!(!explains(&again), "only once: {again:?}");
}

/// The keypad Insert is not a modifier of this run: keypad zero is just a digit.
#[test]
fn keypad_zero_stays_silent_when_the_keypad_insert_is_not_a_modifier() {
    let mut f = fixture();
    f.reader.keys.modifiers = vec![0xff63];
    let said = kp0(&mut f, Modifiers::MOD2, 90);
    assert!(!explains(&said), "{said:?}");
}
