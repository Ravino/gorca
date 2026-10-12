//! Mode state machine (ADR-0001 section 7.2).
//!
//! > "Transitions come from user commands, from focus entering or leaving a document, and from
//! > editable focus inside a document (NVDA-style auto focus mode). Extensions may veto or force
//! > auto-switching. Every transition emits `SetGrabSet(mode.grab_set())`; `InputHelp` uses
//! > `GrabKeyboard`. The full table goes in `docs/design/modes.md` (open item)."
//!
//! TBD (open item): the full table, the extension veto hook, and where `PassThrough` returns to
//! (it returns to `Focus` here; resuming `Browse` needs the modes document).

use gorca_input_api::{Chord, GrabSet};
use gorca_model::{DocId, NodeKey};

use gorca_doc::DocCursor;

/// Review cursor over flat text, objects or a terminal screen. TBD: flat/object/terminal kinds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReviewCursor {
    pub node: NodeKey,
    pub offset: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PassScope {
    OneKey,
    UntilToggle,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Follow system focus; keys pass through.
    Focus,
    /// Virtual cursor over the document mirror.
    Browse {
        doc: DocId,
        cursor: DocCursor,
        sticky: bool,
    },
    /// Flat/object review; terminal review.
    Review {
        origin: NodeKey,
        cursor: ReviewCursor,
    },
    PassThrough {
        scope: PassScope,
    },
    /// Learn mode: keys are described, not executed.
    InputHelp,
}

/// Key material the grab sets are built from. K-1: grab sets are minimal.
#[derive(Clone, Debug, Default)]
pub struct ModeKeys {
    /// gorca's modifier keysym(s), for example Insert or Caps Lock.
    pub modifiers: Vec<u32>,
    /// Explicit command chords.
    pub commands: Vec<Chord>,
    /// Quick-navigation keys, grabbed only in Browse mode.
    pub quick_nav: Vec<Chord>,
    /// The chord(s) that leave pass-through.
    pub pass_through_exit: Vec<Chord>,
}

impl Mode {
    /// The grab set for this mode.
    pub fn grab_set(&self, keys: &ModeKeys) -> GrabSet {
        let mut set = GrabSet::default();
        match self {
            Mode::Focus | Mode::Review { .. } => {
                set.modifiers.extend(keys.modifiers.iter().copied());
                set.chords.extend(keys.commands.iter().copied());
            }
            Mode::Browse { .. } => {
                set.modifiers.extend(keys.modifiers.iter().copied());
                set.chords.extend(keys.commands.iter().copied());
                set.chords.extend(keys.quick_nav.iter().copied());
            }
            Mode::PassThrough { .. } => {
                set.chords.extend(keys.pass_through_exit.iter().copied());
            }
            Mode::InputHelp => set.grab_all = true,
        }
        set
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ModeMsg {
    EnterBrowse {
        doc: DocId,
        cursor: DocCursor,
    },
    ExitBrowse,
    EnterReview {
        origin: NodeKey,
        cursor: ReviewCursor,
    },
    ExitReview,
    StartPassThrough(PassScope),
    /// The single key went through, or the user toggled pass-through off.
    EndPassThrough,
    ToggleInputHelp,
    /// Focus entered a document (auto browse unless the user disabled it).
    FocusEnteredDocument {
        doc: DocId,
        cursor: DocCursor,
    },
    /// Focus entered an editable control inside a document (auto focus mode, unless sticky).
    FocusEnteredEditable,
    FocusLeftDocument,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModeEffect {
    /// "The epoch also advances on every mode change."
    AdvanceEpoch,
    SetGrabSet(GrabSet),
}

impl Mode {
    /// Pure transition function. Returns the new mode and, when the mode changed, the effects.
    pub fn step(self, msg: ModeMsg, keys: &ModeKeys) -> (Mode, Vec<ModeEffect>) {
        let next = self.next(msg);
        if next == self {
            return (self, Vec::new());
        }
        let effects = vec![
            ModeEffect::AdvanceEpoch,
            ModeEffect::SetGrabSet(next.grab_set(keys)),
        ];
        (next, effects)
    }

    fn next(self, msg: ModeMsg) -> Mode {
        match msg {
            ModeMsg::EnterBrowse { doc, cursor } => match self {
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } => Mode::Browse {
                    doc,
                    cursor,
                    sticky: false,
                },
                Mode::PassThrough { .. } | Mode::InputHelp => self,
            },
            ModeMsg::ExitBrowse => match self {
                Mode::Browse { .. } => Mode::Focus,
                Mode::Focus | Mode::Review { .. } | Mode::PassThrough { .. } | Mode::InputHelp => {
                    self
                }
            },
            ModeMsg::EnterReview { origin, cursor } => match self {
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } => {
                    Mode::Review { origin, cursor }
                }
                Mode::PassThrough { .. } | Mode::InputHelp => self,
            },
            ModeMsg::ExitReview => match self {
                Mode::Review { .. } => Mode::Focus,
                Mode::Focus | Mode::Browse { .. } | Mode::PassThrough { .. } | Mode::InputHelp => {
                    self
                }
            },
            ModeMsg::StartPassThrough(scope) => match self {
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. } => Mode::PassThrough { scope },
                Mode::InputHelp => self,
            },
            ModeMsg::EndPassThrough => match self {
                Mode::PassThrough { .. } => Mode::Focus,
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } | Mode::InputHelp => self,
            },
            ModeMsg::ToggleInputHelp => match self {
                Mode::InputHelp => Mode::Focus,
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. } => Mode::InputHelp,
            },
            ModeMsg::FocusEnteredDocument { doc, cursor } => match self {
                Mode::Focus => Mode::Browse {
                    doc,
                    cursor,
                    sticky: false,
                },
                Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp => self,
            },
            ModeMsg::FocusEnteredEditable => match self {
                Mode::Browse { sticky: false, .. } => Mode::Focus,
                Mode::Browse { sticky: true, .. }
                | Mode::Focus
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp => self,
            },
            ModeMsg::FocusLeftDocument => match self {
                Mode::Browse { .. } => Mode::Focus,
                Mode::Focus | Mode::Review { .. } | Mode::PassThrough { .. } | Mode::InputHelp => {
                    self
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::Modifiers;
    use gorca_model::{AppId, ConnectorId, DocNodeId, LocalId};

    const KP_INSERT: u32 = 0xff9e;

    fn keys() -> ModeKeys {
        ModeKeys {
            modifiers: vec![KP_INSERT],
            commands: vec![Chord {
                keysym: 0x68,
                modifiers: Modifiers::CONTROL,
            }],
            quick_nav: vec![Chord {
                keysym: 0x68,
                modifiers: Modifiers::empty(),
            }],
            pass_through_exit: vec![Chord {
                keysym: 0x70,
                modifiers: Modifiers::MOD1,
            }],
        }
    }

    fn cursor() -> DocCursor {
        DocCursor {
            node: DocNodeId(1),
            offset: 0,
        }
    }

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(1),
        }
    }

    fn browse() -> Mode {
        Mode::Browse {
            doc: DocId(1),
            cursor: cursor(),
            sticky: false,
        }
    }

    #[test]
    fn grab_sets_are_minimal_and_mode_specific() {
        let k = keys();
        let focus = Mode::Focus.grab_set(&k);
        assert!(focus.modifiers.contains(&KP_INSERT));
        assert_eq!(focus.chords.len(), 1);

        // Quick-nav keys are grabbed only in Browse (K-1).
        let b = browse().grab_set(&k);
        assert_eq!(b.chords.len(), 2);
        assert!(b.contains(0x68, Modifiers::empty()));
        assert!(!focus.contains(0x68, Modifiers::empty()));

        let pt = Mode::PassThrough {
            scope: PassScope::OneKey,
        }
        .grab_set(&k);
        assert!(pt.modifiers.is_empty());
        assert_eq!(pt.chords.len(), 1);

        assert!(Mode::InputHelp.grab_set(&k).grab_all);
    }

    #[test]
    fn every_real_transition_advances_the_epoch_and_sets_the_grab_set() {
        let k = keys();
        let (m, e) = Mode::Focus.step(
            ModeMsg::EnterBrowse {
                doc: DocId(1),
                cursor: cursor(),
            },
            &k,
        );
        assert_eq!(m, browse());
        assert_eq!(e[0], ModeEffect::AdvanceEpoch);
        assert_eq!(e[1], ModeEffect::SetGrabSet(browse().grab_set(&k)));
    }

    #[test]
    fn no_op_transitions_emit_nothing() {
        let k = keys();
        let (m, e) = Mode::Focus.step(ModeMsg::ExitBrowse, &k);
        assert_eq!(m, Mode::Focus);
        assert!(e.is_empty());
    }

    #[test]
    fn auto_mode_switching_follows_focus_unless_sticky() {
        let k = keys();
        let (m, _) = Mode::Focus.step(
            ModeMsg::FocusEnteredDocument {
                doc: DocId(1),
                cursor: cursor(),
            },
            &k,
        );
        assert_eq!(m, browse());
        let (m, _) = m.step(ModeMsg::FocusEnteredEditable, &k);
        assert_eq!(m, Mode::Focus);
        let sticky = Mode::Browse {
            doc: DocId(1),
            cursor: cursor(),
            sticky: true,
        };
        assert_eq!(sticky.step(ModeMsg::FocusEnteredEditable, &k).0, sticky);
        assert_eq!(browse().step(ModeMsg::FocusLeftDocument, &k).0, Mode::Focus);
    }

    #[test]
    fn input_help_toggles_from_anywhere_and_blocks_other_modes() {
        let k = keys();
        for m in [
            Mode::Focus,
            browse(),
            Mode::PassThrough {
                scope: PassScope::UntilToggle,
            },
        ] {
            let (h, _) = m.step(ModeMsg::ToggleInputHelp, &k);
            assert_eq!(h, Mode::InputHelp);
            assert_eq!(
                h.step(
                    ModeMsg::EnterBrowse {
                        doc: DocId(1),
                        cursor: cursor()
                    },
                    &k
                )
                .0,
                Mode::InputHelp
            );
            assert_eq!(h.step(ModeMsg::ToggleInputHelp, &k).0, Mode::Focus);
        }
    }

    #[test]
    fn review_and_pass_through_round_trips() {
        let k = keys();
        let rc = ReviewCursor {
            node: node(),
            offset: 0,
        };
        let (r, _) = Mode::Focus.step(
            ModeMsg::EnterReview {
                origin: node(),
                cursor: rc,
            },
            &k,
        );
        assert!(matches!(r, Mode::Review { .. }));
        assert_eq!(r.step(ModeMsg::ExitReview, &k).0, Mode::Focus);
        let (p, _) = r.step(ModeMsg::StartPassThrough(PassScope::OneKey), &k);
        assert_eq!(
            p,
            Mode::PassThrough {
                scope: PassScope::OneKey
            }
        );
        assert_eq!(p.step(ModeMsg::EndPassThrough, &k).0, Mode::Focus);
    }
}
