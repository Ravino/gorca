// SPDX-License-Identifier: GPL-3.0-or-later
use bitflags::bitflags;
use gorca_model::Epoch;

bitflags! {
    /// Clutter/X11-style modifier mask as delivered by Mutter (mouse-button bits stripped).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
    pub struct Modifiers: u32 {
        const SHIFT     = 1 << 0;
        /// Caps Lock.
        const LOCK      = 1 << 1;
        const CONTROL   = 1 << 2;
        /// Alt.
        const MOD1      = 1 << 3;
        /// Num Lock.
        const MOD2      = 1 << 4;
        /// Super.
        const MOD4      = 1 << 6;
        /// ISO_Level3_Shift / AltGr.
        const MOD5      = 1 << 7;
        /// Virtual bit: a gorca modifier is held (the input lane's `ModLatch`). Part of the
        /// chord identity ([`crate::ChordKey`]); never sent to Mutter.
        const GORCA     = 1 << 24;
    }
}

impl Modifiers {
    /// Lock bits that must not take part in command lookup (K-6).
    pub const LOCKS: Modifiers = Modifiers::LOCK.union(Modifiers::MOD2);

    #[must_use]
    pub fn without_locks(self) -> Modifiers {
        self.difference(Modifiers::LOCKS)
    }

    /// The bits Mutter knows: the virtual `GORCA` bit removed.
    #[must_use]
    pub fn to_wire(self) -> Modifiers {
        self.difference(Modifiers::GORCA)
    }
}

/// One key press or release, source-neutral.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyEvent {
    /// XKB keycode (evdev + 8). Mutter truncates it to 16 bits.
    pub keycode: u16,
    pub keysym: u32,
    /// Unicode scalar produced by the key, 0 if none.
    pub unichar: u32,
    pub modifiers: Modifiers,
    pub pressed: bool,
}

/// Identifier of a bound command (resolved by the keymap in `gorca-core`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct CmdId(pub u32);

/// A press of a key already down is an autorepeat (PF-4: `KeyEvent` has no repeat flag).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PressKind {
    Press,
    Repeat,
    Release,
}

/// ADR-0001 section 7.3 and ADR-0005 4.1. `Ambiguous` carries the verdict of the **committed**
/// grab set, because that is what Mutter most likely applied; the key is counted (M-16), never
/// re-executed and never re-injected.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KeyClass {
    /// Swallowed by Mutter and bound to an implemented command.
    Command(CmdId),
    /// Swallowed by Mutter (`gorca+X`, or the lone modifier) and not bound: no effect.
    Swallowed,
    /// Learn mode: swallowed, to be described and not executed. Carries the bound command.
    Described(Option<CmdId>),
    /// Reached the app **and** triggers a command (Control press, Shift tap).
    Watched(CmdId),
    /// Observed for echo and interrupt only.
    PassToApp,
    /// Double tap: Mutter passed the modifier press to the app (PF-2).
    ModifierPassThrough,
    /// Mutter's verdict is unknown (grab set in flight, or inside the double-tap guard band).
    Ambiguous { committed: Option<CmdId> },
    /// Matches the injection expectation table: self-feedback, ignored.
    Injected,
}

/// What the input lane hands to the Reader.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyIntent {
    pub event: KeyEvent,
    pub kind: PressKind,
    pub class: KeyClass,
    /// Press count of a command chord within `multi_press_ms` (1 to 3), 1 otherwise.
    pub repeat: u8,
    /// Physical chord identity, locks removed, `GORCA` set iff a gorca modifier was held.
    /// Present for presses of non-modifier keys.
    pub chord: Option<crate::ChordKey>,
    /// Epoch after K-3 advanced it; the Reader adopts it (E-4).
    pub epoch: Epoch,
    /// Receipt time on the input lane.
    pub at: std::time::Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_bits_are_stripped() {
        let m = Modifiers::CONTROL | Modifiers::MOD2 | Modifiers::LOCK;
        assert_eq!(m.without_locks(), Modifiers::CONTROL);
    }
}
