//! Hot-path presentation: a pure function over a snapshot (ADR-0001 section 3.3).
//!
//! > "Hot-path presentation (focus, one nav step) runs inline as a pure function over snapshots.
//! > Bulk presentation (say-all chunks, tables, live-region diff text) runs as `PresentJob`s on
//! > `cpu`, and the results go straight to the speech lane with their epoch, without passing
//! > back through the Reader."

mod echo;
mod flow;
mod job;
mod log;
mod utterance;

pub use echo::{key_echo, key_echo_text, message};
pub use flow::{FOCUS_FIELDS, Presented, REFINE_WINDOW, focus_need};
pub use job::{PresentJob, PresentKind, SpeechIntent};
pub use log::{NoLabels, NodeLabel, PresentLog, PresentRecord, PresentSource};
pub(crate) use utterance::{changed, log_states, present_change};
pub use utterance::{present_focus, present_selected_item};

use gorca_model::{Epoch, NodeSnapshot, Role, UttId, Utterance};

/// Why a node is being presented. A presenter is looked up by this and the node's role, so an
/// extension or a milestone (text, windows, notifications) adds its own without editing the
/// others.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PresentContext {
    /// The node just took focus.
    Focus,
    /// An item became selected without holding focus (a switcher entry).
    SelectedItem,
}

/// A hot-path presenter: pure over the snapshot.
pub type Presenter = fn(&NodeSnapshot, UttId, Epoch) -> Utterance;

/// Presenters keyed by (context, role). The per-context default covers every role that has no
/// override; overrides are few, so the lookup is a short linear scan with no allocation.
#[derive(Clone, Debug)]
pub struct PresenterRegistry {
    focus: Presenter,
    selected_item: Presenter,
    overrides: Vec<(PresentContext, Role, Presenter)>,
}

impl PresenterRegistry {
    /// The built-in presenters: one default per context, no role overrides.
    pub fn builtin() -> Self {
        Self {
            focus: present_focus,
            selected_item: present_selected_item,
            overrides: Vec::new(),
        }
    }

    /// Install `presenter` for nodes of `role` in `ctx`, replacing an earlier override.
    pub fn register(&mut self, ctx: PresentContext, role: Role, presenter: Presenter) {
        if let Some(slot) = self
            .overrides
            .iter_mut()
            .find(|(c, r, _)| *c == ctx && *r == role)
        {
            slot.2 = presenter;
        } else {
            self.overrides.push((ctx, role, presenter));
        }
    }

    /// The presenter for `role` in `ctx`.
    pub fn get(&self, ctx: PresentContext, role: Role) -> Presenter {
        self.overrides
            .iter()
            .find(|(c, r, _)| *c == ctx && *r == role)
            .map_or(
                match ctx {
                    PresentContext::Focus => self.focus,
                    PresentContext::SelectedItem => self.selected_item,
                },
                |(_, _, p)| *p,
            )
    }

    /// Present `n` in `ctx`.
    pub fn present(
        &self,
        ctx: PresentContext,
        n: &NodeSnapshot,
        id: UttId,
        epoch: Epoch,
    ) -> Utterance {
        self.get(ctx, n.role)(n, id, epoch)
    }
}

impl Default for PresenterRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::Modifiers;
    use gorca_model::{AppId, ChildList, ConnectorId, Ifaces, LocalId, Role};
    use gorca_model::{Freshness, NodeKey, Prio, StateSet, Value};
    use std::sync::Arc;
    use std::time::Instant;

    fn snap(role: Role, name: Option<&str>, states: StateSet, fresh: Freshness) -> NodeSnapshot {
        NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            role,
            states,
            name: name.map(Arc::from),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh,
        }
    }

    fn texts(u: &Utterance) -> Vec<&str> {
        u.segments.iter().map(|s| &*s.text).collect()
    }

    #[test]
    fn name_role_and_states_in_order() {
        let n = snap(
            Role::CheckBox,
            Some("Remember me"),
            StateSet::CHECKED | StateSet::DISABLED,
            Freshness::Fresh { at: Instant::now() },
        );
        let u = present_focus(&n, UttId(1), Epoch(4));
        assert_eq!(
            texts(&u),
            ["Remember me", "check box", "unavailable", "checked"]
        );
        assert_eq!(u.prio, Prio::Interrupt);
        assert_eq!(u.epoch, Epoch(4));
        assert!(!u.may_refine);
    }

    #[test]
    fn stale_nodes_are_tagged_may_refine_and_empty_names_are_skipped() {
        let n = snap(
            Role::Button,
            Some(""),
            StateSet::empty(),
            Freshness::Stale {
                since: Instant::now(),
                mask: gorca_model::PropMask::NAME,
            },
        );
        let u = present_focus(&n, UttId(1), Epoch(1));
        assert_eq!(texts(&u), ["button"]);
        assert!(u.may_refine);
    }

    #[test]
    fn values_are_spoken_last() {
        let mut n = snap(
            Role::Slider,
            Some("Volume"),
            StateSet::empty(),
            Freshness::Fresh { at: Instant::now() },
        );
        n.value = Some(Value::Range {
            current: 40.0,
            min: 0.0,
            max: 100.0,
        });
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Volume", "slider", "40"]
        );
        n.value = Some(Value::Text(Arc::from("hello")));
        n.role = Role::TextEntry;
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Volume", "edit", "hello"]
        );
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
    fn selected_items_do_not_say_selected() {
        let n = snap(
            Role::Button,
            Some("Terminal"),
            StateSet::SELECTED,
            Freshness::Fresh { at: Instant::now() },
        );
        assert_eq!(
            texts(&present_focus(&n, UttId(1), Epoch(1))),
            ["Terminal", "button", "selected"]
        );
        assert_eq!(
            texts(&present_selected_item(&n, UttId(1), Epoch(1))),
            ["Terminal", "button"]
        );
    }

    #[test]
    fn message_has_message_priority() {
        let u = message("application not responding", UttId(2), Epoch(3));
        assert_eq!(u.prio, Prio::Message);
        assert_eq!(texts(&u), ["application not responding"]);
    }

    fn shouting(_n: &NodeSnapshot, id: UttId, epoch: Epoch) -> Utterance {
        message("override", id, epoch)
    }

    #[test]
    fn registry_falls_back_to_the_context_default_and_honours_role_overrides() {
        let mut reg = PresenterRegistry::builtin();
        let button = snap(
            Role::Button,
            Some("OK"),
            StateSet::SELECTED,
            Freshness::Fresh { at: Instant::now() },
        );
        let link = snap(
            Role::Link,
            Some("Home"),
            StateSet::empty(),
            Freshness::Fresh { at: Instant::now() },
        );
        let id = UttId(1);
        let e = Epoch(1);
        assert_eq!(
            texts(&reg.present(PresentContext::Focus, &button, id, e)),
            ["OK", "button", "selected"]
        );
        assert_eq!(
            texts(&reg.present(PresentContext::SelectedItem, &button, id, e)),
            ["OK", "button"]
        );
        reg.register(PresentContext::Focus, Role::Link, shouting);
        reg.register(PresentContext::Focus, Role::Link, shouting);
        assert_eq!(
            texts(&reg.present(PresentContext::Focus, &link, id, e)),
            ["override"]
        );
        assert_eq!(reg.overrides.len(), 1, "re-registering replaces");
        assert_eq!(
            texts(&reg.present(PresentContext::SelectedItem, &link, id, e)),
            ["Home", "link"],
            "other contexts keep the default"
        );
        assert_eq!(
            texts(&reg.present(PresentContext::Focus, &button, id, e)),
            ["OK", "button", "selected"],
            "other roles keep the default"
        );
    }
}
