//! Text-key classification (ADR-0007 4.1): which keys act on a text field, and how.
//!
//! A pure table over the keysym, the physical key and the modifiers without locks. The Reader
//! applies it to keys of class `PassToApp` (the input lane could carry the result, but the
//! table costs a few comparisons, so the lane's `KeyIntent` stays unchanged).

use crate::event::{KeyEvent, Modifiers};
use crate::keys::static_keycode;

/// Direction of a movement or an erase.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextDir {
    Prev,
    Next,
}

/// The unit a movement key advances by.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeyUnit {
    Char,
    Word,
    Line,
    Paragraph,
    LineEdge,
    DocEdge,
    Page,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ClipOp {
    Paste,
    Cut,
    Undo,
    Redo,
}

/// What a key does to a text field.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum TextKey {
    /// A caret movement; `select` when Shift is held.
    Move {
        unit: KeyUnit,
        dir: TextDir,
        select: bool,
    },
    /// Ctrl+A.
    SelectAll,
    /// A printable character (Return is `'\n'`).
    Typed {
        ch: char,
    },
    /// BackSpace or Delete; `word` with Ctrl.
    Erase {
        dir: TextDir,
        word: bool,
    },
    Clipboard(ClipOp),
}

const HOME: u32 = 0xff50;
const LEFT: u32 = 0xff51;
const UP: u32 = 0xff52;
const RIGHT: u32 = 0xff53;
const DOWN: u32 = 0xff54;
const PRIOR: u32 = 0xff55;
const NEXT: u32 = 0xff56;
const END: u32 = 0xff57;
const INSERT: u32 = 0xff63;
const BACKSPACE: u32 = 0xff08;
const DELETE: u32 = 0xffff;
const RETURN: u32 = 0xff0d;
const TAB: u32 = 0xff09;
const KP_ENTER: u32 = 0xff8d;
const KP_HOME: u32 = 0xff95;
const KP_END: u32 = 0xff9c;

/// The main-block navigation keysym of a keypad navigation keysym (Num Lock off).
fn fold_keypad(ks: u32) -> u32 {
    match ks {
        KP_HOME..=KP_END => ks - KP_HOME + HOME,
        other => other,
    }
}

/// Classify a key press. `None` for keys that do not act on text (and for releases).
pub fn classify(ev: &KeyEvent) -> Option<TextKey> {
    if !ev.pressed {
        return None;
    }
    let m = ev.modifiers.without_locks();
    // Alt and Super are application shortcuts.
    if m.intersects(Modifiers::MOD1 | Modifiers::MOD4) {
        return None;
    }
    let ctrl = m.contains(Modifiers::CONTROL);
    let shift = m.contains(Modifiers::SHIFT);
    let ks = fold_keypad(ev.keysym);
    let mv = |unit, dir| {
        Some(TextKey::Move {
            unit,
            dir,
            select: shift,
        })
    };
    match ks {
        LEFT => mv(
            if ctrl { KeyUnit::Word } else { KeyUnit::Char },
            TextDir::Prev,
        ),
        RIGHT => mv(
            if ctrl { KeyUnit::Word } else { KeyUnit::Char },
            TextDir::Next,
        ),
        UP => mv(
            if ctrl {
                KeyUnit::Paragraph
            } else {
                KeyUnit::Line
            },
            TextDir::Prev,
        ),
        DOWN => mv(
            if ctrl {
                KeyUnit::Paragraph
            } else {
                KeyUnit::Line
            },
            TextDir::Next,
        ),
        HOME => mv(
            if ctrl {
                KeyUnit::DocEdge
            } else {
                KeyUnit::LineEdge
            },
            TextDir::Prev,
        ),
        END => mv(
            if ctrl {
                KeyUnit::DocEdge
            } else {
                KeyUnit::LineEdge
            },
            TextDir::Next,
        ),
        PRIOR if !ctrl => mv(KeyUnit::Page, TextDir::Prev),
        NEXT if !ctrl => mv(KeyUnit::Page, TextDir::Next),
        BACKSPACE => Some(TextKey::Erase {
            dir: TextDir::Prev,
            word: ctrl,
        }),
        DELETE => Some(TextKey::Erase {
            dir: TextDir::Next,
            word: ctrl,
        }),
        INSERT if shift && !ctrl => Some(TextKey::Clipboard(ClipOp::Paste)),
        RETURN | KP_ENTER if !ctrl => Some(TextKey::Typed { ch: '\n' }),
        // Tab inserts a tab in a text view; in an entry it moves the focus (the Reader knows).
        TAB if !ctrl => Some(TextKey::Typed { ch: '\t' }),
        _ if ctrl => ctrl_letter(ev.keycode, shift),
        _ => printable(ev.unichar),
    }
}

/// Ctrl shortcuts are physical keys, so they work in every layout.
fn ctrl_letter(code: u16, shift: bool) -> Option<TextKey> {
    let is = |name: &str| static_keycode(name) == Some(code);
    if is("a") && !shift {
        Some(TextKey::SelectAll)
    } else if is("v") {
        Some(TextKey::Clipboard(ClipOp::Paste))
    } else if is("x") {
        Some(TextKey::Clipboard(ClipOp::Cut))
    } else if is("z") {
        Some(TextKey::Clipboard(if shift {
            ClipOp::Redo
        } else {
            ClipOp::Undo
        }))
    } else if is("y") && !shift {
        Some(TextKey::Clipboard(ClipOp::Redo))
    } else {
        None
    }
}

fn printable(unichar: u32) -> Option<TextKey> {
    char::from_u32(unichar)
        .filter(|c| !c.is_control())
        .map(|ch| TextKey::Typed { ch })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(keysym: u32, unichar: u32, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            keycode: 0,
            keysym,
            unichar,
            modifiers,
            pressed: true,
        }
    }

    fn code(name: &str, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            keycode: static_keycode(name).unwrap_or(0),
            keysym: 0x61,
            unichar: 0,
            modifiers,
            pressed: true,
        }
    }

    #[test]
    fn arrows_pick_the_unit_by_modifier() {
        let none = Modifiers::empty();
        assert_eq!(
            classify(&ev(LEFT, 0, none)),
            Some(TextKey::Move {
                unit: KeyUnit::Char,
                dir: TextDir::Prev,
                select: false
            })
        );
        assert_eq!(
            classify(&ev(RIGHT, 0, Modifiers::CONTROL)),
            Some(TextKey::Move {
                unit: KeyUnit::Word,
                dir: TextDir::Next,
                select: false
            })
        );
        assert_eq!(
            classify(&ev(DOWN, 0, Modifiers::SHIFT)),
            Some(TextKey::Move {
                unit: KeyUnit::Line,
                dir: TextDir::Next,
                select: true
            })
        );
        assert_eq!(
            classify(&ev(UP, 0, Modifiers::CONTROL)),
            Some(TextKey::Move {
                unit: KeyUnit::Paragraph,
                dir: TextDir::Prev,
                select: false
            })
        );
        assert_eq!(
            classify(&ev(HOME, 0, none)),
            Some(TextKey::Move {
                unit: KeyUnit::LineEdge,
                dir: TextDir::Prev,
                select: false
            })
        );
        assert_eq!(
            classify(&ev(END, 0, Modifiers::CONTROL | Modifiers::SHIFT)),
            Some(TextKey::Move {
                unit: KeyUnit::DocEdge,
                dir: TextDir::Next,
                select: true
            })
        );
        assert_eq!(
            classify(&ev(PRIOR, 0, none)),
            Some(TextKey::Move {
                unit: KeyUnit::Page,
                dir: TextDir::Prev,
                select: false
            })
        );
        assert_eq!(classify(&ev(PRIOR, 0, Modifiers::CONTROL)), None);
    }

    #[test]
    fn locks_and_the_keypad_are_folded() {
        let locks = Modifiers::LOCK | Modifiers::MOD2;
        assert_eq!(
            classify(&ev(LEFT, 0, locks)),
            classify(&ev(LEFT, 0, Modifiers::empty()))
        );
        // KP_Left with Num Lock off acts like Left.
        assert_eq!(
            classify(&ev(0xff96, 0, Modifiers::empty())),
            classify(&ev(LEFT, 0, Modifiers::empty()))
        );
        // KP_Down (0xff99) and KP_End (0xff9c) too.
        assert_eq!(
            classify(&ev(0xff99, 0, Modifiers::empty())),
            classify(&ev(DOWN, 0, Modifiers::empty()))
        );
        assert_eq!(
            classify(&ev(KP_END, 0, Modifiers::empty())),
            classify(&ev(END, 0, Modifiers::empty()))
        );
    }

    #[test]
    fn alt_and_super_are_application_shortcuts() {
        assert_eq!(classify(&ev(LEFT, 0, Modifiers::MOD1)), None);
        assert_eq!(classify(&ev(0x61, 0x61, Modifiers::MOD4)), None);
    }

    #[test]
    fn typing_erasing_and_clipboard() {
        let none = Modifiers::empty();
        assert_eq!(
            classify(&ev(0x61, 0x61, none)),
            Some(TextKey::Typed { ch: 'a' })
        );
        assert_eq!(
            classify(&ev(0x61, 0x41, Modifiers::SHIFT)),
            Some(TextKey::Typed { ch: 'A' })
        );
        assert_eq!(
            classify(&ev(0x6d, 0x43f, Modifiers::MOD5)),
            Some(TextKey::Typed { ch: 'п' })
        );
        assert_eq!(
            classify(&ev(RETURN, 0x0d, none)),
            Some(TextKey::Typed { ch: '\n' })
        );
        assert_eq!(
            classify(&ev(BACKSPACE, 0x08, Modifiers::CONTROL)),
            Some(TextKey::Erase {
                dir: TextDir::Prev,
                word: true
            })
        );
        assert_eq!(
            classify(&ev(DELETE, 0x7f, none)),
            Some(TextKey::Erase {
                dir: TextDir::Next,
                word: false
            })
        );
        assert_eq!(
            classify(&ev(INSERT, 0, Modifiers::SHIFT)),
            Some(TextKey::Clipboard(ClipOp::Paste))
        );
        assert_eq!(
            classify(&ev(0xff09, 0x09, none)),
            Some(TextKey::Typed { ch: '\t' }),
            "Tab"
        );
        assert_eq!(
            classify(&ev(0xfe20, 0x09, Modifiers::SHIFT)),
            None,
            "Shift+Tab"
        );
        assert_eq!(
            classify(&ev(0xffe1, 0, Modifiers::SHIFT)),
            None,
            "lone Shift"
        );
    }

    #[test]
    fn ctrl_letters_are_physical_keys() {
        let c = Modifiers::CONTROL;
        assert_eq!(classify(&code("a", c)), Some(TextKey::SelectAll));
        assert_eq!(
            classify(&code("v", c)),
            Some(TextKey::Clipboard(ClipOp::Paste))
        );
        assert_eq!(
            classify(&code("x", c)),
            Some(TextKey::Clipboard(ClipOp::Cut))
        );
        assert_eq!(
            classify(&code("z", c)),
            Some(TextKey::Clipboard(ClipOp::Undo))
        );
        assert_eq!(
            classify(&code("z", c | Modifiers::SHIFT)),
            Some(TextKey::Clipboard(ClipOp::Redo))
        );
        assert_eq!(
            classify(&code("y", c)),
            Some(TextKey::Clipboard(ClipOp::Redo))
        );
        assert_eq!(classify(&code("q", c)), None);
    }

    #[test]
    fn releases_are_not_text_keys() {
        let mut e = ev(LEFT, 0, Modifiers::empty());
        e.pressed = false;
        assert_eq!(classify(&e), None);
    }
}
