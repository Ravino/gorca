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
    }
}

impl Modifiers {
    /// Lock bits that must not take part in command lookup (K-6).
    pub const LOCKS: Modifiers = Modifiers::LOCK.union(Modifiers::MOD2);

    #[must_use]
    pub fn without_locks(self) -> Modifiers {
        self.difference(Modifiers::LOCKS)
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

/// ADR-0001 section 7.3. `Ambiguous` carries the verdict of the **committed** grab set, because
/// that is what Mutter most likely applied; the key is counted (M-16), never re-executed and
/// never re-injected.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KeyClass {
    /// In the committed grab set: Mutter swallowed it.
    Command(CmdId),
    /// Observed for echo and interrupt only.
    PassToApp,
    Ambiguous {
        committed: Option<CmdId>,
    },
    /// Matches the injection expectation table: self-feedback, ignored.
    Injected,
}

/// What the input lane hands to the Reader.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct KeyIntent {
    pub event: KeyEvent,
    pub class: KeyClass,
    /// Epoch after K-3 advanced it; the Reader adopts it (E-4).
    pub epoch: Epoch,
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
