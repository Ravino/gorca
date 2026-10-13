use std::collections::HashSet;

use crate::event::Modifiers;
use crate::keymap::InputContext;

/// A keystroke grabbed by exact keysym and modifier mask.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Chord {
    pub keysym: u32,
    pub modifiers: Modifiers,
}

/// The set of keys Mutter should swallow for us (`SetKeyGrabs`), or everything (`GrabKeyboard`).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct GrabSet {
    /// XKB keysyms grabbed as modifiers (pressed alone they are swallowed).
    pub modifiers: HashSet<u32>,
    /// Explicit chords, stored **without** lock-bit variants.
    pub chords: HashSet<Chord>,
    /// `InputHelp` mode: `GrabKeyboard`.
    pub grab_all: bool,
    /// Lookup context to use while this set is committed (ADR-0005 5.2). Not sent to Mutter.
    pub ctx: InputContext,
}

impl GrabSet {
    pub fn is_empty(&self) -> bool {
        !self.grab_all && self.modifiers.is_empty() && self.chords.is_empty()
    }

    /// O(1) membership test on the input lane. Lock bits are ignored (K-6).
    pub fn contains(&self, keysym: u32, modifiers: Modifiers) -> bool {
        self.grab_all
            || self.modifiers.contains(&keysym)
            || self.chords.contains(&Chord {
                keysym,
                modifiers: modifiers.without_locks(),
            })
    }

    /// Arguments of `SetKeyGrabs(au, a(uu))`, sorted for determinism, with every chord expanded
    /// to its four lock variants (K-6).
    pub fn to_wire(&self) -> (Vec<u32>, Vec<(u32, u32)>) {
        let mut mods: Vec<u32> = self.modifiers.iter().copied().collect();
        mods.sort_unstable();
        let mut strokes: Vec<(u32, u32)> = self
            .chords
            .iter()
            .flat_map(|c| expand_lock_variants(*c))
            .map(|c| (c.keysym, c.modifiers.to_wire().bits()))
            .collect();
        strokes.sort_unstable();
        strokes.dedup();
        (mods, strokes)
    }
}

/// K-6: Mutter matches the Clutter state mask by exact equality, so each chord is registered
/// with every combination of Caps Lock and Num Lock.
pub fn expand_lock_variants(c: Chord) -> [Chord; 4] {
    let base = c.modifiers.without_locks();
    [
        Chord {
            keysym: c.keysym,
            modifiers: base,
        },
        Chord {
            keysym: c.keysym,
            modifiers: base | Modifiers::LOCK,
        },
        Chord {
            keysym: c.keysym,
            modifiers: base | Modifiers::MOD2,
        },
        Chord {
            keysym: c.keysym,
            modifiers: base | Modifiers::LOCK | Modifiers::MOD2,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_H: u32 = 0x68;
    const KP_INSERT: u32 = 0xff9e;

    fn set() -> GrabSet {
        let mut s = GrabSet::default();
        s.modifiers.insert(KP_INSERT);
        s.chords.insert(Chord {
            keysym: KEY_H,
            modifiers: Modifiers::CONTROL,
        });
        s
    }

    #[test]
    fn contains_ignores_lock_bits() {
        let s = set();
        assert!(s.contains(KEY_H, Modifiers::CONTROL));
        assert!(s.contains(
            KEY_H,
            Modifiers::CONTROL | Modifiers::MOD2 | Modifiers::LOCK
        ));
        assert!(!s.contains(KEY_H, Modifiers::empty()));
        assert!(!s.contains(KEY_H, Modifiers::CONTROL | Modifiers::SHIFT));
        assert!(s.contains(KP_INSERT, Modifiers::empty()));
    }

    #[test]
    fn grab_all_contains_everything() {
        let s = GrabSet {
            grab_all: true,
            ..GrabSet::default()
        };
        assert!(s.contains(0x1234, Modifiers::SHIFT));
        assert!(!s.is_empty());
        assert!(GrabSet::default().is_empty());
    }

    #[test]
    fn wire_form_has_four_lock_variants_per_chord() {
        let (mods, strokes) = set().to_wire();
        assert_eq!(mods, vec![KP_INSERT]);
        let c = Modifiers::CONTROL.bits();
        assert_eq!(
            strokes,
            vec![
                (KEY_H, c),
                (KEY_H, c | Modifiers::LOCK.bits()),
                (KEY_H, c | Modifiers::MOD2.bits()),
                (KEY_H, c | Modifiers::LOCK.bits() | Modifiers::MOD2.bits()),
            ]
        );
    }

    #[test]
    fn expansion_is_idempotent_for_chords_that_already_carry_locks() {
        let a = expand_lock_variants(Chord {
            keysym: 1,
            modifiers: Modifiers::LOCK,
        });
        let b = expand_lock_variants(Chord {
            keysym: 1,
            modifiers: Modifiers::empty(),
        });
        assert_eq!(a, b);
    }
}
