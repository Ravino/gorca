// SPDX-License-Identifier: GPL-3.0-or-later
//! Hot-path presentation: a pure function over a snapshot (ADR-0001 section 3.3).
//!
//! > "Hot-path presentation (focus, one nav step) runs inline as a pure function over snapshots.
//! > Bulk presentation (say-all chunks, tables, live-region diff text) runs as `PresentJob`s on
//! > `cpu`, and the results go straight to the speech lane with their epoch, without passing
//! > back through the Reader."

mod echo;
mod fair;
mod flow;
mod job;
mod log;
mod node;
mod notification;
mod utterance;
mod window;

pub use echo::{key_echo, key_echo_is_action, key_echo_spoken, key_echo_text, message};
pub(crate) use flow::name_is_blank;
#[cfg(test)]
pub(crate) use flow::needs_name;
pub use flow::{
    FOCUS_FIELDS, FOCUS_FIELDS_LAZY, FocusBackoff, NameRetry, Presented, RAPID_FOCUS,
    REFINE_WINDOW, STORM_HOLD, focus_fields_for, focus_need,
};
pub use job::{PresentJob, PresentKind, SpeechIntent};
pub use log::{NoLabels, NodeLabel, PresentLog, PresentRecord, PresentSource};
pub(crate) use notification::NoticeCtx;
pub use notification::{announcement_utterance, clean_announcement, is_notice_role};
pub use utterance::{flat_zone_text, log_states, value_text};
pub(crate) use window::WindowCtx;
pub use window::{
    Content, DesktopEffect, WindowEffect, WindowInfo, WindowMsg, WindowState, dialog_content,
    fetch_level, fetch_window, reads_content,
};

use gorca_model::{Epoch, NodeSnapshot};

/// Why a node is being presented. A presentation is made for this and the node's fused entry
/// (`present::node`), so an extension or a milestone (text, windows, notifications) adds its own
/// without editing the others.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PresentContext {
    /// The node just took focus.
    Focus,
    /// An item became selected without holding focus (a switcher entry).
    SelectedItem,
}

/// The extension hook of the focus presentation (ADR-0001 section 10): the extensions of the node's
/// application are asked first and, if one answers, its words replace the composed ones (they
/// enter the IR as adapted tokens). The words of the built-in presenter come from `gorca-present`.
#[derive(Clone, Debug, Default)]
pub struct PresenterRegistry {
    ext: Option<std::sync::Arc<dyn crate::ExtSource>>,
}

impl PresenterRegistry {
    /// No extensions.
    pub fn builtin() -> Self {
        Self::default()
    }

    /// Ask the extensions of the node's application first.
    #[must_use]
    pub fn with_ext(mut self, src: std::sync::Arc<dyn crate::ExtSource>) -> Self {
        self.ext = Some(src);
        self
    }

    /// The extension's presentation of `n` in `ctx`, if its application has one.
    pub fn ext_segments(
        &self,
        ctx: PresentContext,
        n: &NodeSnapshot,
        epoch: Epoch,
    ) -> Option<Vec<gorca_model::Segment>> {
        self.ext
            .as_ref()
            .and_then(|src| src.for_app(n.key.app))
            .and_then(|app| app.present(n, ctx, epoch.0))
            .filter(|segs| !segs.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::Modifiers;
    use gorca_model::{Prio, UttId, Utterance};

    fn texts(u: &Utterance) -> Vec<&str> {
        u.segments.iter().map(|s| &*s.text).collect()
    }

    #[test]
    fn key_echo_names_action_keys_and_skips_lone_modifiers() {
        let none = Modifiers::empty();
        assert_eq!(key_echo_text('a' as u32, 0x61, none).as_deref(), Some("a"));
        assert_eq!(key_echo_text(0x20, 0x20, none).as_deref(), Some("space"));
        assert_eq!(key_echo_text(0, 0xff0d, none).as_deref(), Some("enter"));
        assert_eq!(key_echo_text(0, 0xff09, none).as_deref(), Some("tab"));
        assert_eq!(
            key_echo_text(0, 0xffe1, none),
            None,
            "shift alone is silent"
        );
        assert_eq!(key_echo_text(0, 0xffe9, Modifiers::MOD1), None, "so is alt");
        assert_eq!(
            key_echo_text(0x07, 0x07, none),
            None,
            "control characters are not echoed"
        );
        assert_eq!(key_echo_text('é' as u32, 0xe9, none).as_deref(), Some("é"));
        assert_eq!(key_echo_text(0, 0xffbe, none).as_deref(), Some("f 1"));
    }

    #[test]
    fn navigation_keys_are_not_echoed() {
        for sym in [
            0xff50, 0xff51, 0xff52, 0xff53, 0xff54, 0xff55, 0xff56, 0xff57,
        ] {
            assert_eq!(key_echo_text(0, sym, Modifiers::empty()), None, "{sym:#x}");
        }
    }

    #[test]
    fn nothing_is_echoed_under_alt_control_or_super() {
        for m in [
            Modifiers::MOD1,
            Modifiers::CONTROL,
            Modifiers::MOD4,
            Modifiers::MOD1 | Modifiers::SHIFT,
            Modifiers::CONTROL | Modifiers::MOD2,
        ] {
            assert_eq!(key_echo_text(0, 0xff09, m), None, "tab under {m:?}");
            assert_eq!(key_echo_text('c' as u32, 0x63, m), None, "c under {m:?}");
        }
    }

    #[test]
    fn shift_locks_and_altgr_still_echo() {
        for m in [
            Modifiers::SHIFT,
            Modifiers::LOCK,
            Modifiers::MOD2,
            Modifiers::MOD5,
        ] {
            assert_eq!(key_echo_text('A' as u32, 0x41, m).as_deref(), Some("A"));
        }
        assert_eq!(
            key_echo_text(0, 0xfe20, Modifiers::SHIFT).as_deref(),
            Some("tab")
        );
    }

    #[test]
    fn message_has_message_priority() {
        let u = message("application not responding", UttId(2), Epoch(3));
        assert_eq!(u.prio, Prio::Message);
        assert_eq!(texts(&u), ["application not responding"]);
    }
}

#[cfg(test)]
mod attrs_mask_tests {
    use super::*;
    use gorca_model::PropMask;

    /// M22a C1a: the cold focus fetch asks for `ATTRS` (one more pipelined call), and the M-6b
    /// fallback switch takes it out again.
    #[test]
    fn focus_mask_carries_attrs_until_the_lazy_switch_is_set() {
        assert!(FOCUS_FIELDS.contains(PropMask::ATTRS));
        assert!(!FOCUS_FIELDS.contains(PropMask::DESCRIPTION));
        assert!(!FOCUS_FIELDS_LAZY.contains(PropMask::ATTRS));
        assert_eq!(focus_fields_for(false), FOCUS_FIELDS);
        assert_eq!(focus_fields_for(true), FOCUS_FIELDS_LAZY);
        assert!(crate::review::objnav::NAV_MASK.contains(PropMask::ATTRS));
    }
}
