//! Key echo and plain messages.

use gorca_input_api::Modifiers;
use gorca_model::{Epoch, Prio, SegmentKind, UttId, Utterance};
use smallvec::SmallVec;

use super::utterance::seg;

/// Modifiers that turn a key press into a command or shortcut. Orca never echoes a key pressed
/// together with Alt or Control (`typing_echo_presenter.should_echo_keyboard_event`); Super is
/// treated the same here. Shift, the locks and AltGr (a character modifier) do not count.
const COMMAND_MODIFIERS: Modifiers = Modifiers::CONTROL
    .union(Modifiers::MOD1)
    .union(Modifiers::MOD4);

/// Spoken form of a key press (key echo), following Orca's defaults: alphabetic, numeric,
/// punctuation, space, function and action keys (enter, tab, backspace, escape, delete) are
/// echoed; navigation keys and lone modifier keys are not; nothing is echoed while Alt,
/// Control or Super is held (`Alt+Tab` must not say "tab").
pub fn key_echo_text(unichar: u32, keysym: u32, modifiers: Modifiers) -> Option<String> {
    if modifiers.intersects(COMMAND_MODIFIERS) {
        return None;
    }
    let named = match keysym {
        0xff0d | 0xff8d => Some("enter"),
        0xff08 => Some("backspace"),
        0xff09 | 0xfe20 => Some("tab"),
        0xff1b => Some("escape"),
        0xffff => Some("delete"),
        0x20 => Some("space"),
        _ => None,
    };
    if let Some(n) = named {
        return Some(n.to_owned());
    }
    // Navigation keys: Home, Left, Up, Right, Down, Page Up, Page Down, End (and the keypad
    // copies). Their result is what gets spoken, not the key.
    if (0xff50..=0xff58).contains(&keysym) || (0xff95..=0xff9c).contains(&keysym) {
        return None;
    }
    // Function keys F1..F35 are named.
    if (0xffbe..=0xffe0).contains(&keysym) {
        return Some(format!("f {}", keysym - 0xffbe + 1));
    }
    // Shift, Control, Alt, Super, Caps Lock, Num Lock: pressed on their own they say nothing.
    if (0xffe1..=0xffee).contains(&keysym) || keysym == 0xff7f {
        return None;
    }
    char::from_u32(unichar)
        .filter(|c| !c.is_control())
        .map(String::from)
}

/// Is the echo text of a key a key NAME ("enter", "tab", "backspace", "escape", "delete", "f 5")
/// rather than the character it types (a letter, a digit, a symbol, "space")? Names are command
/// keys: they are spoken only when `echo.command_keys` is on.
pub fn key_echo_is_action(text: &str) -> bool {
    text != "space" && text.chars().nth(1).is_some()
}

/// Key echo: interrupts, carries the epoch of the key press.
pub fn key_echo(text: &str, id: UttId, epoch: Epoch) -> Utterance {
    let mut u = message(text, id, epoch);
    u.prio = Prio::Interrupt;
    u
}

/// A plain spoken message (for example "application not responding").
pub fn message(text: &str, id: UttId, epoch: Epoch) -> Utterance {
    let mut segments = SmallVec::new();
    segments.push(seg(text, SegmentKind::Text));
    Utterance {
        id,
        segments,
        lang: None,
        epoch,
        prio: Prio::Message,
        may_refine: false,
        survives_focus_change: false,
        ttl_secs: 0,
        refines: None,
    }
}
