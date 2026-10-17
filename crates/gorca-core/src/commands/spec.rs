//! Command metadata (ADR-0005 3.1): the static spec of every command and the name table.

use std::collections::HashMap;
use std::sync::OnceLock;

use gorca_config::ContextName;
use gorca_input_api::CmdId;

use super::table::{COUNT, CoreCmd, SPECS};

/// The milestone that implements a declared command.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Milestone {
    M3,
    M4,
    M5,
    M6,
    M7,
    M8,
    Later,
}

impl Milestone {
    pub fn name(self) -> &'static str {
        match self {
            Milestone::M3 => "M3",
            Milestone::M4 => "M4",
            Milestone::M5 => "M5",
            Milestone::M6 => "M6",
            Milestone::M7 => "M7",
            Milestone::M8 => "M8",
            Milestone::Later => "later",
        }
    }
}

/// `Planned` commands are bound in presets but inert: not grabbed, learn mode says "not
/// available yet" (KC-10).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Availability {
    Implemented,
    Planned(Milestone),
}

/// The keymap contexts a command may be bound in (KC-8).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ContextMask(u8);

impl ContextMask {
    pub const ANY: ContextMask = ContextMask(0b1111);
    pub const BROWSE_ONLY: ContextMask = ContextMask(0b0100);

    /// The mask allowing exactly `ctxs` (extension commands declare theirs in the manifest).
    pub fn of(ctxs: &[ContextName]) -> ContextMask {
        let mut bits = 0u8;
        for c in ctxs {
            bits |= match c {
                ContextName::Global => 0b0001,
                ContextName::Focus => 0b0010,
                ContextName::Browse => 0b0100,
                ContextName::Review => 0b1000,
            };
        }
        ContextMask(bits)
    }

    pub fn allows(self, ctx: ContextName) -> bool {
        let bit = match ctx {
            ContextName::Global => 0b0001,
            ContextName::Focus => 0b0010,
            ContextName::Browse => 0b0100,
            ContextName::Review => 0b1000,
        };
        self.0 & bit != 0
    }
}

#[derive(Copy, Clone, Debug)]
pub struct CmdSpec {
    pub cmd: CoreCmd,
    /// Stable TOML name, for example `speech.rate_up`.
    pub name: &'static str,
    pub available: Availability,
    pub contexts: ContextMask,
    /// Highest press count the command distinguishes.
    pub max_press: u8,
    /// K-3: pressing the chord advances the epoch (default true).
    pub interrupts: bool,
    /// Runs even in learn mode.
    pub input_help_bypass: bool,
    /// Learn-mode description (catalog `cmd-<name>`).
    pub help: gorca_i18n::MsgId,
}

impl CmdSpec {
    /// The learn-mode description in the UI language.
    pub fn help_text(&self) -> &'static str {
        gorca_i18n::t(self.help)
    }
}

impl CoreCmd {
    pub fn id(self) -> CmdId {
        CmdId(self as u32)
    }

    pub fn spec(self) -> &'static CmdSpec {
        &SPECS[self as usize]
    }

    pub fn name(self) -> &'static str {
        self.spec().name
    }

    pub fn from_id(id: CmdId) -> Option<CoreCmd> {
        let i = usize::try_from(id.0).ok()?;
        CoreCmd::ALL.get(i).copied()
    }

    pub fn is_implemented(self) -> bool {
        self.spec().available == Availability::Implemented
    }

    /// Voice settings and review moves repeat while the key is held (ADR-0006 RS-3: a held
    /// review key keeps moving, NVDA parity; the input lane debounces the rate).
    pub fn repeatable(self) -> bool {
        matches!(
            self,
            CoreCmd::ReviewPrevLine
                | CoreCmd::ReviewNextLine
                | CoreCmd::ReviewPrevWord
                | CoreCmd::ReviewNextWord
                | CoreCmd::ReviewPrevChar
                | CoreCmd::ReviewNextChar
                | CoreCmd::ObjectNext
                | CoreCmd::ObjectPrev
                | CoreCmd::ObjectNextInFlow
                | CoreCmd::ObjectPrevInFlow
                | CoreCmd::BrowseCursorNextLine
                | CoreCmd::BrowseCursorPrevLine
                | CoreCmd::BrowseCursorNextChar
                | CoreCmd::BrowseCursorPrevChar
                | CoreCmd::BrowseCursorNextWord
                | CoreCmd::BrowseCursorPrevWord
                | CoreCmd::BrowseCursorPageUp
                | CoreCmd::BrowseCursorPageDown
                | CoreCmd::BrowseCursorNextParagraph
                | CoreCmd::BrowseCursorPrevParagraph
                | CoreCmd::BrowseCursorNextSentence
                | CoreCmd::BrowseCursorPrevSentence
                | CoreCmd::BrowseSelectNextChar
                | CoreCmd::BrowseSelectPrevChar
                | CoreCmd::BrowseSelectNextWord
                | CoreCmd::BrowseSelectPrevWord
                | CoreCmd::BrowseSelectNextLine
                | CoreCmd::BrowseSelectPrevLine
                | CoreCmd::BrowseSelectPageUp
                | CoreCmd::BrowseSelectPageDown
                | CoreCmd::SpeechRingInc
                | CoreCmd::SpeechRingDec
                | CoreCmd::SpeechRingIncLarge
                | CoreCmd::SpeechRingDecLarge
                | CoreCmd::SpeechRateUp
                | CoreCmd::SpeechRateDown
                | CoreCmd::SpeechPitchUp
                | CoreCmd::SpeechPitchDown
                | CoreCmd::SpeechVolumeUp
                | CoreCmd::SpeechVolumeDown
        )
    }
}

/// Name lookup. Built once; commands never change at run time in M1 (extensions add theirs in M5).
#[derive(Debug)]
pub struct CommandTable {
    by_name: HashMap<&'static str, CoreCmd>,
}

impl CommandTable {
    pub fn get() -> &'static CommandTable {
        static T: OnceLock<CommandTable> = OnceLock::new();
        T.get_or_init(|| CommandTable {
            by_name: SPECS.iter().map(|s| (s.name, s.cmd)).collect(),
        })
    }

    pub fn by_name(&self, name: &str) -> Option<CoreCmd> {
        self.by_name.get(name).copied()
    }

    pub fn names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.by_name.keys().copied()
    }

    pub fn len(&self) -> usize {
        COUNT
    }

    pub fn is_empty(&self) -> bool {
        COUNT == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_has_a_consistent_spec() {
        for (i, c) in CoreCmd::ALL.iter().enumerate() {
            assert_eq!(*c as usize, i, "discriminants are dense");
            let s = c.spec();
            assert_eq!(s.cmd, *c);
            assert!(!s.help_text().is_empty() && !s.name.is_empty(), "{}", s.name);
            assert!((1..=3).contains(&s.max_press));
            assert_eq!(CoreCmd::from_id(c.id()), Some(*c));
            assert_eq!(CommandTable::get().by_name(s.name), Some(*c));
        }
        assert_eq!(CommandTable::get().len(), CoreCmd::ALL.len());
        assert_eq!(CoreCmd::from_id(CmdId(u32::MAX)), None);
    }

    #[test]
    fn names_are_unique_dotted_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for s in &SPECS {
            assert!(seen.insert(s.name), "{}", s.name);
            assert!(s.name.contains('.') && s.name == s.name.to_ascii_lowercase());
        }
    }

    #[test]
    fn m1_m3_and_m4a_commands_are_exactly_the_implemented_ones() {
        let impl_names: Vec<_> = SPECS
            .iter()
            .filter(|s| s.available == Availability::Implemented)
            .map(|s| s.name)
            .collect();
        assert!(impl_names.contains(&"info.where_am_i"));
        assert!(impl_names.contains(&"settings.save"));
        // M4a: object navigation, line/word/char/top/bottom review, focus and activation.
        assert!(impl_names.contains(&"review.cur_line"));
        assert!(impl_names.contains(&"object.next_in_flow"));
        assert!(impl_names.contains(&"info.status_bar"));
        assert!(!impl_names.contains(&"review.say_all"), "M4b");
        assert!(!impl_names.contains(&"review.above"), "M4b");
        // M7a: browse mode on static pages (cursor keys, H/1-6/K and the other quick-nav letters
        // with a default binding in every preset, Enter, toggle, Escape in focus mode).
        assert!(impl_names.contains(&"browse.cursor_next_line"));
        assert!(impl_names.contains(&"browse.escape"));
        assert!(impl_names.contains(&"quicknav.next.heading3"));
        assert!(
            !impl_names.contains(&"quicknav.next.heading7"),
            "H7 to H9 are not HTML"
        );
        assert!(impl_names.contains(&"quicknav.next.frame"), "M7b");
        assert!(impl_names.contains(&"quicknav.prev.article"), "M7b");
        assert!(!impl_names.contains(&"browse.find"), "M8");
        // M3: caret and text commands, selection, say-all and the punctuation cycle.
        assert!(impl_names.contains(&"text.say_all"));
        assert!(impl_names.contains(&"info.selection"));
        assert!(impl_names.contains(&"info.formatting"));
        assert_eq!(impl_names.len(), 184);
    }

    #[test]
    fn context_masks() {
        assert!(ContextMask::ANY.allows(ContextName::Review));
        assert!(ContextMask::BROWSE_ONLY.allows(ContextName::Browse));
        assert!(!ContextMask::BROWSE_ONLY.allows(ContextName::Focus));
    }
}
