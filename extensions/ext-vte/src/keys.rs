//! Which key is what in a terminal: the readline and emacs bindings (shell policy, ADR-0008).
//!
//! The core hands over the key as it saw it ([`RawKey`]); the Control and Alt shortcuts are
//! matched on the physical letter key so they work in every keyboard layout.

use gorca_ext_api::terminal::{RawKey, TermKey};

const HOME: u32 = 0xff50;
const LEFT: u32 = 0xff51;
const UP: u32 = 0xff52;
const RIGHT: u32 = 0xff53;
const DOWN: u32 = 0xff54;
const END: u32 = 0xff57;
const BACKSPACE: u32 = 0xff08;
const DELETE: u32 = 0xffff;
const RETURN: u32 = 0xff0d;
const KP_ENTER: u32 = 0xff8d;
const TAB: u32 = 0xff09;
const KP_HOME: u32 = 0xff95;
const KP_LEFT: u32 = 0xff96;
const KP_UP: u32 = 0xff97;
const KP_RIGHT: u32 = 0xff98;
const KP_DOWN: u32 = 0xff99;
const KP_END: u32 = 0xff9c;

/// Classify a key press that reaches the terminal.
pub fn classify(k: &RawKey) -> TermKey {
    if k.logo {
        return TermKey::Other;
    }
    let ks = match k.keysym {
        KP_HOME => HOME,
        KP_LEFT => LEFT,
        KP_UP => UP,
        KP_RIGHT => RIGHT,
        KP_DOWN => DOWN,
        KP_END => END,
        other => other,
    };
    let is = |c: u8| k.phys == c;
    match (k.ctrl, k.alt) {
        (true, true) => TermKey::Other,
        (true, false) => match ks {
            LEFT => TermKey::WordLeft,
            RIGHT => TermKey::WordRight,
            _ if is(b'c') && !k.shift => TermKey::Interrupt,
            _ if is(b'a') => TermKey::LineStart,
            _ if is(b'e') => TermKey::LineEnd,
            _ if is(b'b') => TermKey::CharLeft,
            _ if is(b'f') => TermKey::CharRight,
            _ if is(b'p') => TermKey::LineUp,
            _ if is(b'n') => TermKey::LineDown,
            _ => TermKey::Other,
        },
        (false, true) => match ks {
            LEFT => TermKey::WordLeft,
            RIGHT => TermKey::WordRight,
            _ if is(b'b') => TermKey::WordLeft,
            _ if is(b'f') => TermKey::WordRight,
            _ => TermKey::Other,
        },
        (false, false) => match ks {
            LEFT => TermKey::CharLeft,
            RIGHT => TermKey::CharRight,
            UP => TermKey::LineUp,
            DOWN => TermKey::LineDown,
            HOME => TermKey::LineStart,
            END => TermKey::LineEnd,
            BACKSPACE => TermKey::Backspace,
            DELETE => TermKey::Delete,
            RETURN | KP_ENTER => TermKey::Enter,
            TAB => TermKey::Tab,
            _ => match char::from_u32(k.unichar) {
                Some(c) if k.unichar != 0 && !c.is_control() => TermKey::Typed(c),
                Some(_) | None => TermKey::Other,
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(keysym: u32, unichar: u32, phys: char) -> RawKey {
        RawKey {
            keysym,
            unichar,
            phys: if phys == '-' { 0 } else { phys as u8 },
            shift: false,
            ctrl: false,
            alt: false,
            logo: false,
        }
    }

    fn ctrl(mut k: RawKey) -> RawKey {
        k.ctrl = true;
        k
    }

    fn alt(mut k: RawKey) -> RawKey {
        k.alt = true;
        k
    }

    #[test]
    fn keys_are_classified_for_a_terminal() {
        assert_eq!(
            classify(&key('a' as u32, 'a' as u32, 'a')),
            TermKey::Typed('a')
        );
        assert_eq!(classify(&key(0x20, 0x20, '-')), TermKey::Typed(' '));
        assert_eq!(classify(&key(0xff0d, 13, '-')), TermKey::Enter);
        assert_eq!(classify(&key(0xff08, 8, '-')), TermKey::Backspace);
        assert_eq!(classify(&key(0xff09, 9, '-')), TermKey::Tab);
        assert_eq!(classify(&key(0xff52, 0, '-')), TermKey::LineUp);
        assert_eq!(classify(&key(0xff51, 0, '-')), TermKey::CharLeft);
        assert_eq!(classify(&ctrl(key(0xff51, 0, '-'))), TermKey::WordLeft);
        assert_eq!(classify(&ctrl(key('c' as u32, 3, 'c'))), TermKey::Interrupt);
        assert_eq!(classify(&ctrl(key('a' as u32, 1, 'a'))), TermKey::LineStart);
        assert_eq!(classify(&ctrl(key('e' as u32, 5, 'e'))), TermKey::LineEnd);
        assert_eq!(classify(&alt(key('b' as u32, 0, 'b'))), TermKey::WordLeft);
        assert_eq!(classify(&alt(key('f' as u32, 0, 'f'))), TermKey::WordRight);
        let mut shifted = key('A' as u32, 'A' as u32, 'a');
        shifted.shift = true;
        assert_eq!(classify(&shifted), TermKey::Typed('A'));
        // Ctrl+Shift+C is the terminal's copy, not an interrupt.
        let mut copy = ctrl(key('C' as u32, 3, 'c'));
        copy.shift = true;
        assert_eq!(classify(&copy), TermKey::Other);
        assert_eq!(classify(&key(0xffe1, 0, '-')), TermKey::Other);
        assert_eq!(classify(&key(0xff1b, 27, '-')), TermKey::Other);
        assert_eq!(classify(&key(0xffbe, 0, '-')), TermKey::Other);
        let mut logo = key('x' as u32, 'x' as u32, 'x');
        logo.logo = true;
        assert_eq!(classify(&logo), TermKey::Other);
        // The keypad arrows are the arrows.
        assert_eq!(classify(&key(0xff96, 0, '-')), TermKey::CharLeft);
        // Ctrl+Alt is the desktop's.
        assert_eq!(
            classify(&alt(ctrl(key('b' as u32, 0, 'b')))),
            TermKey::Other
        );
    }
}
