use std::collections::HashMap;

use crate::event::{CmdId, KeyClass, KeyEvent};
use crate::grab::{Chord, GrabSet};

/// Command bindings. Lock bits are ignored on lookup.
#[derive(Clone, Debug, Default)]
pub struct Keymap {
    bindings: HashMap<Chord, CmdId>,
}

impl Keymap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn bind(&mut self, chord: Chord, cmd: CmdId) {
        let chord = Chord {
            modifiers: chord.modifiers.without_locks(),
            ..chord
        };
        self.bindings.insert(chord, cmd);
    }

    pub fn lookup(&self, ev: &KeyEvent) -> Option<CmdId> {
        self.bindings
            .get(&Chord {
                keysym: ev.keysym,
                modifiers: ev.modifiers.without_locks(),
            })
            .copied()
    }
}

/// Key classification on the input lane, O(1) (section 7.3).
///
/// - `committed`: the grab set Mutter most likely applied.
/// - `wanted`: the grab set of an in-flight `SetKeyGrabs` (`GrabSetState::Pending`), if any.
/// - `injected`: the key matched the injection expectation table.
pub fn classify(
    ev: &KeyEvent,
    committed: &GrabSet,
    wanted: Option<&GrabSet>,
    keymap: &Keymap,
    injected: bool,
) -> KeyClass {
    if injected {
        return KeyClass::Injected;
    }
    let in_committed = committed.contains(ev.keysym, ev.modifiers);
    let verdict = if in_committed {
        keymap.lookup(ev)
    } else {
        None
    };
    match wanted {
        Some(w) if w.contains(ev.keysym, ev.modifiers) != in_committed => {
            KeyClass::Ambiguous { committed: verdict }
        }
        Some(_) | None => match verdict {
            Some(cmd) => KeyClass::Command(cmd),
            None => KeyClass::PassToApp,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::Modifiers;

    const KEY_H: u32 = 0x68;

    fn ev(keysym: u32, modifiers: Modifiers) -> KeyEvent {
        KeyEvent {
            keycode: 43,
            keysym,
            unichar: 0,
            modifiers,
            pressed: true,
        }
    }

    fn fixture() -> (GrabSet, Keymap) {
        let chord = Chord {
            keysym: KEY_H,
            modifiers: Modifiers::CONTROL,
        };
        let mut g = GrabSet::default();
        g.chords.insert(chord);
        let mut k = Keymap::new();
        k.bind(chord, CmdId(7));
        (g, k)
    }

    #[test]
    fn grabbed_chord_is_a_command_even_with_numlock() {
        let (g, k) = fixture();
        let c = classify(
            &ev(KEY_H, Modifiers::CONTROL | Modifiers::MOD2),
            &g,
            None,
            &k,
            false,
        );
        assert_eq!(c, KeyClass::Command(CmdId(7)));
    }

    #[test]
    fn ungrabbed_key_passes_to_the_app() {
        let (g, k) = fixture();
        assert_eq!(
            classify(&ev(0x61, Modifiers::empty()), &g, None, &k, false),
            KeyClass::PassToApp
        );
    }

    #[test]
    fn injected_wins_over_everything() {
        let (g, k) = fixture();
        let c = classify(&ev(KEY_H, Modifiers::CONTROL), &g, None, &k, true);
        assert_eq!(c, KeyClass::Injected);
    }

    #[test]
    fn keys_in_the_symmetric_difference_are_ambiguous_and_use_the_committed_verdict() {
        let (g, k) = fixture();
        // Pending: the chord is being removed.
        let wanted = GrabSet::default();
        let c = classify(&ev(KEY_H, Modifiers::CONTROL), &g, Some(&wanted), &k, false);
        assert_eq!(
            c,
            KeyClass::Ambiguous {
                committed: Some(CmdId(7))
            }
        );

        // Pending: the chord is being added; committed does not contain it yet.
        let c = classify(
            &ev(KEY_H, Modifiers::CONTROL),
            &GrabSet::default(),
            Some(&g),
            &k,
            false,
        );
        assert_eq!(c, KeyClass::Ambiguous { committed: None });

        // Keys in both or neither set are unambiguous during Pending.
        let c = classify(&ev(KEY_H, Modifiers::CONTROL), &g, Some(&g), &k, false);
        assert_eq!(c, KeyClass::Command(CmdId(7)));
        let c = classify(&ev(0x61, Modifiers::empty()), &g, Some(&g), &k, false);
        assert_eq!(c, KeyClass::PassToApp);
    }
}
