//! Mode state machine (ADR-0001 section 7.2).
//!
//! > "Transitions come from user commands, from focus entering or leaving a document, and from
//! > editable focus inside a document (NVDA-style auto focus mode). Extensions may veto or force
//! > auto-switching. Every transition emits `SetGrabSet(mode.grab_set())`; `InputHelp` uses
//! > `GrabKeyboard`. The full table goes in `docs/design/modes.md` (open item)."
//!
//! The table is `docs/design/modes.md` section 3. `PassThrough` and `InputHelp` remember the base
//! mode they return to ([`BaseMode`], ADR-0005 5.1). TBD: the extension veto hook.

use gorca_input_api::{Chord, GrabSet, InputContext, ModeKind};
use gorca_model::{DocId, DocNodeId};

pub use crate::review::{ReviewCursor, ReviewFrom, ReviewOrigin};

/// Virtual-cursor position in a web document: a node and a scalar offset inside the node's own
/// text (browse-mode design 7.1). Replaces the scaffold `DocCursor`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WebCursor {
    pub node: DocNodeId,
    pub local: u32,
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
        sticky: bool,
    },
    /// Flat/object review; terminal review (ADR-0006).
    Review {
        origin: ReviewOrigin,
        cursor: ReviewCursor,
    },
    PassThrough {
        scope: PassScope,
        resume: BaseMode,
    },
    /// Learn mode: keys are described, not executed.
    InputHelp {
        resume: BaseMode,
    },
}

/// The modes `PassThrough` and `InputHelp` return to. Never nests.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BaseMode {
    Focus,
    Browse {
        doc: DocId,
        sticky: bool,
    },
    Review {
        origin: ReviewOrigin,
        cursor: ReviewCursor,
    },
}

impl BaseMode {
    fn into_mode(self) -> Mode {
        match self {
            BaseMode::Focus => Mode::Focus,
            BaseMode::Browse { doc, sticky } => Mode::Browse { doc, sticky },
            BaseMode::Review { origin, cursor } => Mode::Review { origin, cursor },
        }
    }

    pub fn kind(self) -> ModeKind {
        match self {
            BaseMode::Focus => ModeKind::Focus,
            BaseMode::Browse { .. } => ModeKind::Browse,
            BaseMode::Review { .. } => ModeKind::Review,
        }
    }
}

/// Key material the grab sets are built from. K-1: grab sets are minimal.
#[derive(Clone, Debug, Default)]
pub struct ModeKeys {
    /// gorca's modifier keysym(s), for example Insert or Caps Lock.
    pub modifiers: Vec<u32>,
    /// Explicit command chords, expanded over the lock states on the wire.
    pub commands: Vec<Chord>,
    /// Keypad command chords, exact (Num Lock changes their keysym, K-6a, K-8a).
    pub exact: Vec<Chord>,
    /// Quick-navigation keys, grabbed only in Browse mode.
    pub quick_nav: Vec<Chord>,
    /// The bare single-letter quick-navigation chords (a subset of `quick_nav`): not grabbed
    /// while single-letter navigation is off.
    pub letters: Vec<Chord>,
    /// The chord(s) that leave pass-through.
    pub pass_through_exit: Vec<Chord>,
    /// The chord(s) bound to `browse.escape`: grabbed only in auto focus mode (modes.md 2).
    pub web_escape: Vec<Chord>,
}

impl Mode {
    /// The base mode, or `None` while pass-through or learn mode is active.
    pub fn base(self) -> Option<BaseMode> {
        match self {
            Mode::Focus => Some(BaseMode::Focus),
            Mode::Browse { doc, sticky } => Some(BaseMode::Browse { doc, sticky }),
            Mode::Review { origin, cursor } => Some(BaseMode::Review { origin, cursor }),
            Mode::PassThrough { .. } | Mode::InputHelp { .. } => None,
        }
    }

    /// The base mode in effect, looking through pass-through and learn mode.
    pub fn resume_base(self) -> BaseMode {
        match self {
            Mode::PassThrough { resume, .. } | Mode::InputHelp { resume } => resume,
            Mode::Focus => BaseMode::Focus,
            Mode::Browse { doc, sticky } => BaseMode::Browse { doc, sticky },
            Mode::Review { origin, cursor } => BaseMode::Review { origin, cursor },
        }
    }

    /// The fieldless mirror the keymap is keyed by.
    pub fn context(self) -> InputContext {
        match self {
            Mode::Focus => InputContext::of(ModeKind::Focus),
            Mode::Browse { .. } => InputContext::of(ModeKind::Browse),
            Mode::Review { .. } => InputContext::of(ModeKind::Review),
            Mode::PassThrough { resume, .. } => InputContext {
                mode: ModeKind::PassThrough,
                resume: resume.kind(),
            },
            Mode::InputHelp { resume } => InputContext {
                mode: ModeKind::InputHelp,
                resume: resume.kind(),
            },
        }
    }

    /// The grab set for this mode (K-1: minimal), published together with its lookup context.
    pub fn grab_set(&self, keys: &ModeKeys) -> GrabSet {
        let mut set = GrabSet {
            ctx: self.context(),
            ..GrabSet::default()
        };
        match self {
            Mode::Focus | Mode::Review { .. } => {
                set.modifiers.extend(keys.modifiers.iter().copied());
                set.chords.extend(keys.commands.iter().copied());
                set.exact.extend(keys.exact.iter().copied());
            }
            Mode::Browse { .. } => {
                set.modifiers.extend(keys.modifiers.iter().copied());
                set.chords.extend(keys.commands.iter().copied());
                set.chords.extend(keys.quick_nav.iter().copied());
                set.exact.extend(keys.exact.iter().copied());
            }
            // Pass-next-key: nothing is grabbed, not even the modifiers, so the next key or
            // chord reaches the application natively (ADR-0005 5.3).
            Mode::PassThrough { .. } => {
                set.chords.extend(keys.pass_through_exit.iter().copied());
            }
            // Learn mode: everything, with the modifiers kept so the double tap still works.
            Mode::InputHelp { .. } => {
                set.grab_all = true;
                set.modifiers.extend(keys.modifiers.iter().copied());
            }
        }
        set
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ModeMsg {
    EnterBrowse {
        doc: DocId,
    },
    ExitBrowse,
    EnterReview {
        origin: ReviewOrigin,
        cursor: ReviewCursor,
    },
    ExitReview,
    StartPassThrough(PassScope),
    /// The single key went through, or the user toggled pass-through off.
    EndPassThrough,
    ToggleInputHelp,
    /// Escape, idle timeout or Reader stall: leave learn mode.
    InputHelpExit,
    /// Focus entered a document (auto browse unless the user disabled it).
    FocusEnteredDocument {
        doc: DocId,
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
            ModeMsg::EnterBrowse { doc } => match self {
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } => {
                    Mode::Browse { doc, sticky: false }
                }
                Mode::PassThrough { .. } | Mode::InputHelp { .. } => self,
            },
            ModeMsg::ExitBrowse => match self {
                Mode::Browse { .. } => Mode::Focus,
                Mode::Focus
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. } => self,
            },
            ModeMsg::EnterReview { origin, cursor } => match self {
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } => {
                    Mode::Review { origin, cursor }
                }
                Mode::PassThrough { .. } | Mode::InputHelp { .. } => self,
            },
            ModeMsg::ExitReview => match self {
                Mode::Review { origin, .. } => match origin.from {
                    ReviewFrom::Focus => Mode::Focus,
                    ReviewFrom::Browse { doc, sticky } => Mode::Browse { doc, sticky },
                },
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. } => self,
            },
            ModeMsg::StartPassThrough(scope) => match self {
                Mode::Focus | Mode::Browse { .. } | Mode::Review { .. } => Mode::PassThrough {
                    scope,
                    resume: self.resume_base(),
                },
                // Replaces a running pass-through; the base to return to is kept (MD-2).
                Mode::PassThrough { resume, .. } => Mode::PassThrough { scope, resume },
                Mode::InputHelp { .. } => self,
            },
            ModeMsg::EndPassThrough => match self {
                Mode::PassThrough { resume, .. } => resume.into_mode(),
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::InputHelp { .. } => self,
            },
            ModeMsg::ToggleInputHelp => match self {
                Mode::InputHelp { resume } => resume.into_mode(),
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. } => Mode::InputHelp {
                    resume: self.resume_base(),
                },
            },
            // Escape, the idle timeout and a stalled Reader (LM-3): leave learn mode, never enter.
            ModeMsg::InputHelpExit => match self {
                Mode::InputHelp { resume } => resume.into_mode(),
                Mode::Focus
                | Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. } => self,
            },
            ModeMsg::FocusEnteredDocument { doc } => match self {
                Mode::Focus => Mode::Browse { doc, sticky: false },
                Mode::Browse { .. }
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. } => self,
            },
            ModeMsg::FocusEnteredEditable => match self {
                Mode::Browse { sticky: false, .. } => Mode::Focus,
                Mode::Browse { sticky: true, .. }
                | Mode::Focus
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. } => self,
            },
            ModeMsg::FocusLeftDocument => match self {
                Mode::Browse { .. } => Mode::Focus,
                Mode::Focus
                | Mode::Review { .. }
                | Mode::PassThrough { .. }
                | Mode::InputHelp { .. } => self,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::Modifiers;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};

    const KP_INSERT: u32 = 0xff9e;

    fn keys() -> ModeKeys {
        ModeKeys {
            modifiers: vec![KP_INSERT],
            commands: vec![Chord {
                keysym: 0x68,
                modifiers: Modifiers::CONTROL,
            }],
            exact: Vec::new(),
            quick_nav: vec![Chord {
                keysym: 0x68,
                modifiers: Modifiers::empty(),
            }],
            pass_through_exit: vec![Chord {
                keysym: 0x70,
                modifiers: Modifiers::MOD1,
            }],
            web_escape: Vec::new(),
            letters: Vec::new(),
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
            resume: BaseMode::Focus,
        }
        .grab_set(&k);
        assert!(
            pt.modifiers.is_empty(),
            "pass-next-key grabs nothing (ADR-0005 5.3)"
        );
        assert_eq!(pt.chords.len(), 1);

        let help = Mode::InputHelp {
            resume: BaseMode::Focus,
        }
        .grab_set(&k);
        assert!(help.grab_all);
        assert!(
            help.modifiers.contains(&KP_INSERT),
            "LM-1: modifiers stay grabbed"
        );
        assert_eq!(help.ctx.mode, ModeKind::InputHelp);
        assert_eq!(help.ctx.resume, ModeKind::Focus);
        assert_eq!(focus.ctx, InputContext::of(ModeKind::Focus));
    }

    #[test]
    fn every_real_transition_advances_the_epoch_and_sets_the_grab_set() {
        let k = keys();
        let (m, e) = Mode::Focus.step(ModeMsg::EnterBrowse { doc: DocId(1) }, &k);
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
        let (m, _) = Mode::Focus.step(ModeMsg::FocusEnteredDocument { doc: DocId(1) }, &k);
        assert_eq!(m, browse());
        let (m, _) = m.step(ModeMsg::FocusEnteredEditable, &k);
        assert_eq!(m, Mode::Focus);
        let sticky = Mode::Browse {
            doc: DocId(1),
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
                resume: BaseMode::Focus,
            },
        ] {
            let (h, _) = m.step(ModeMsg::ToggleInputHelp, &k);
            assert_eq!(
                h,
                Mode::InputHelp {
                    resume: m.resume_base()
                }
            );
            assert_eq!(h.step(ModeMsg::EnterBrowse { doc: DocId(1) }, &k).0, h);
            // Toggling off returns to the mode learn mode was entered from (never to Focus
            // by accident once Browse exists).
            assert_eq!(
                h.step(ModeMsg::ToggleInputHelp, &k).0,
                m.resume_base().into_mode()
            );
            assert_eq!(
                h.step(ModeMsg::InputHelpExit, &k).0,
                m.resume_base().into_mode()
            );
        }
        assert_eq!(Mode::Focus.step(ModeMsg::InputHelpExit, &k).0, Mode::Focus);
    }

    #[test]
    fn pass_through_and_learn_mode_resume_browse() {
        let k = keys();
        let (p, e) = browse().step(ModeMsg::StartPassThrough(PassScope::OneKey), &k);
        assert_eq!(e.len(), 2);
        assert_eq!(p.step(ModeMsg::EndPassThrough, &k).0, browse());
        let (h, _) = p.step(ModeMsg::ToggleInputHelp, &k);
        assert_eq!(h.step(ModeMsg::ToggleInputHelp, &k).0, browse());
        assert_eq!(h.context().resume, ModeKind::Browse);
    }

    #[test]
    fn review_and_pass_through_round_trips() {
        let k = keys();
        let rc = ReviewCursor::at(node(), crate::review::ReviewKind::Screen);
        let origin = ReviewOrigin {
            node: node(),
            from: ReviewFrom::Focus,
        };
        let (r, _) = Mode::Focus.step(ModeMsg::EnterReview { origin, cursor: rc }, &k);
        assert!(matches!(r, Mode::Review { .. }));
        assert_eq!(r.step(ModeMsg::ExitReview, &k).0, Mode::Focus);
        let (p, _) = r.step(ModeMsg::StartPassThrough(PassScope::OneKey), &k);
        assert_eq!(
            p,
            Mode::PassThrough {
                scope: PassScope::OneKey,
                resume: BaseMode::Review { origin, cursor: rc }
            }
        );
        assert_eq!(p.step(ModeMsg::EndPassThrough, &k).0, r);
    }

    #[test]
    fn leaving_review_returns_to_where_it_started() {
        let k = keys();
        let rc = ReviewCursor::at(node(), crate::review::ReviewKind::Object);
        let origin = ReviewOrigin {
            node: node(),
            from: ReviewFrom::Browse {
                doc: DocId(1),
                sticky: true,
            },
        };
        let (r, e) = browse().step(ModeMsg::EnterReview { origin, cursor: rc }, &k);
        assert_eq!(e.len(), 2, "mode change: epoch and grab set");
        let sticky = Mode::Browse {
            doc: DocId(1),
            sticky: true,
        };
        assert_eq!(r.step(ModeMsg::ExitReview, &k).0, sticky);
        // Replacing the cursor in place is not a mode change (the Reader does it directly).
        let (same, e) = r.step(ModeMsg::EnterReview { origin, cursor: rc }, &k);
        assert_eq!(same, r);
        assert!(e.is_empty());
    }
}
