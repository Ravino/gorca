// SPDX-License-Identifier: GPL-3.0-or-later
//! `compose`: `SemEntry` + `Reason` to `PresIr` (ADR-0022 section 2.5). Pure; reads nothing but
//! the entry and the context, never the mirror or IPC.
//!
//! Slice 1 implements one rule table row: **`native.focus`** (`Reason::Focus` of a node that is
//! not web content). Field order is the legacy presenter's, so the shadow diff is meaningful:
//! name, role, states, value, help text.

use gorca_i18n::MsgId as M;
use gorca_model::NodeKey;
use gorca_model::sem::{
    Avail, Check, Expand, Group, Invalid, Press, Select, SemEntry, SemFlags, SemRole,
};
use smallvec::SmallVec;

use std::sync::Arc;

use crate::ir::{Content, Field, FieldFlags, FieldKind, IrFlags, Origin, PresIr, Reason, RowId};
use crate::roles::{Class, native_role};

/// Rule row of the native focus utterance.
pub const ROW_NATIVE_FOCUS: RowId = RowId(1);

/// Interim report-flag set (ADR-0016 adopts it unchanged). Slice 1 has no verbosity bits in use;
/// the type exists so `compose` has its final signature.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ReportFlags(pub u32);

/// Everything `compose` needs besides the entry.
#[derive(Copy, Clone, Debug)]
pub struct ComposeCtx {
    pub flags: ReportFlags,
    /// The entry was composed from stale data.
    pub may_refine: bool,
}

/// Why a composition did not happen.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum ComposeError {
    /// No rule row exists for the reason yet (later M22b slices add them).
    #[error("no rule row for reason code {0}")]
    Unsupported(u16),
}

fn origin(group: Group) -> Origin {
    Origin {
        row: ROW_NATIVE_FOCUS,
        group: group.slot(),
        via: 0,
    }
}

fn word(kind: FieldKind, id: M, group: Group) -> Field {
    Field::new(kind, Content::word(id), origin(group))
}

/// What a mixed (tri-state) value says for a role class.
const fn tri_state_word(class: Class) -> Option<M> {
    match class {
        Class::Checkable => Some(M::StateHalfChecked),
        Class::Toggle => Some(M::StateHalfPressed),
        Class::Progress => Some(M::StateIndeterminate),
        Class::Switch | Class::Field | Class::Other => None,
    }
}

/// Compose the IR of `entry` for `reason`.
///
/// # Errors
/// [`ComposeError::Unsupported`] for a reason without a rule row.
pub fn compose(
    entry: &SemEntry,
    node: NodeKey,
    reason: Reason,
    ctx: ComposeCtx,
) -> Result<PresIr, ComposeError> {
    match reason {
        Reason::Focus => Ok(native_focus(entry, node, ctx)),
        Reason::FocusAncestor
        | Reason::BrowseLine { .. }
        | Reason::QuickNav
        | Reason::SayAll
        | Reason::Review
        | Reason::Change { .. }
        | Reason::ListEntry
        | Reason::WhereAmI
        | Reason::Unknown(_) => Err(ComposeError::Unsupported(reason.code())),
    }
}

fn native_focus(entry: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let sem = &entry.sem;
    let info = native_role(sem.role, sem.press);
    let class = info.class;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();

    if let Some(name) = entry.name.as_ref().filter(|s| !s.is_empty()) {
        fields.push(Field::new(
            FieldKind::NAME,
            Content::Text(Arc::clone(name)),
            origin(Group::Name),
        ));
    }
    if let Some(role_word) = info.word {
        fields.push(word(FieldKind::ROLE, role_word, Group::Role));
    }

    // States, in the legacy speaking order.
    if sem.avail == Avail::Disabled {
        fields.push(word(FieldKind::STATE, M::StateUnavailable, Group::Avail));
    }
    let mixed = (sem.check == Check::Mixed || sem.press == Press::Mixed)
        .then(|| tri_state_word(class))
        .flatten();
    if let Some(m) = mixed {
        let g = if sem.check == Check::Mixed {
            Group::Check
        } else {
            Group::Press
        };
        fields.push(word(FieldKind::STATE, m, g));
    } else if class == Class::Switch {
        // A switch says on or off, whichever group carries the state.
        let on = sem.check == Check::On || sem.press == Press::On;
        let group = if sem.press == Press::NotToggle {
            Group::Check
        } else {
            Group::Press
        };
        let id = if on { M::StateOn } else { M::StateOff };
        fields.push(word(FieldKind::STATE, id, group));
    } else {
        if sem.check == Check::On {
            fields.push(word(FieldKind::STATE, M::StateChecked, Group::Check));
        }
        if sem.press == Press::On {
            fields.push(word(FieldKind::STATE, M::StatePressed, Group::Press));
        }
    }
    if sem.select == Select::On {
        fields.push(word(FieldKind::STATE, M::StateSelected, Group::Select));
    }
    match sem.expand {
        Expand::Expanded => fields.push(word(FieldKind::STATE, M::StateExpanded, Group::Expand)),
        Expand::Collapsed => fields.push(word(FieldKind::STATE, M::StateCollapsed, Group::Expand)),
        Expand::NotExpandable => {}
    }
    if sem.flags.contains(SemFlags::REQUIRED) {
        fields.push(word(FieldKind::STATE, M::StateRequired, Group::Flags));
    }
    if sem.invalid != Invalid::No {
        fields.push(word(FieldKind::STATE, M::StateInvalidEntry, Group::Invalid));
    }
    // A checkable control says its off state too.
    if mixed.is_none() {
        if class == Class::Checkable && sem.check != Check::On {
            fields.push(word(FieldKind::STATE, M::StateNotChecked, Group::Check));
        } else if class == Class::Toggle && sem.press != Press::On && sem.check != Check::On {
            fields.push(word(FieldKind::STATE, M::StateNotPressed, Group::Press));
        }
    }

    let secret = sem.role == SemRole::PasswordBox;
    if let Some(v) = sem
        .text
        .as_deref()
        .and_then(|t| t.value_text.as_ref())
        .filter(|s| !s.is_empty())
    {
        let mut f = Field::new(
            FieldKind::VALUE,
            Content::Text(Arc::clone(v)),
            origin(Group::ValueText),
        );
        if secret {
            f = f.with_flags(FieldFlags::SECRET);
        }
        fields.push(f);
    }
    if class == Class::Field
        && let Some(d) = sem
            .text
            .as_deref()
            .and_then(|t| t.description.as_ref())
            .filter(|s| !s.is_empty())
    {
        fields.push(Field::new(
            FieldKind::DESCRIPTION,
            Content::Text(Arc::clone(d)),
            origin(Group::Description),
        ));
    }

    let mut ir = PresIr::new(Reason::Focus, node, entry.sem_rev, fields);
    ir.may_refine = ctx.may_refine;
    if secret {
        ir.flags = IrFlags::SECRET;
    }
    ir
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::SemText;
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;

    pub(crate) fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(7),
        }
    }

    const CTX: ComposeCtx = ComposeCtx {
        flags: ReportFlags(0),
        may_refine: false,
    };

    fn entry(role: SemRole, name: &str) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.name = Some(Arc::from(name));
        e.sem_rev = 9;
        e
    }

    fn kinds(ir: &PresIr) -> Vec<FieldKind> {
        ir.fields.iter().map(|f| f.kind).collect()
    }

    #[test]
    fn a_button_is_name_then_role() {
        let ir = compose(&entry(SemRole::Button, "OK"), key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(kinds(&ir), [FieldKind::NAME, FieldKind::ROLE]);
        assert_eq!(ir.sem_rev, 9);
        assert_eq!(ir.node, key());
        assert_eq!(ir.fields[0].origin.row, ROW_NATIVE_FOCUS);
        assert_eq!(ir.fields[0].origin.group, Group::Name.slot());
    }

    #[test]
    fn a_checked_check_box_says_checked_and_an_unchecked_one_says_not_checked() {
        let mut e = entry(SemRole::Checkbox, "Mute");
        e.sem.check = Check::On;
        let on = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(
            on.fields.last().map(|f| f.content.clone()),
            Some(Content::word(M::StateChecked))
        );
        e.sem.check = Check::Off;
        let off = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(
            off.fields.last().map(|f| f.content.clone()),
            Some(Content::word(M::StateNotChecked))
        );
    }

    #[test]
    fn a_switch_says_switch_and_on_or_off() {
        let mut e = entry(SemRole::Switch, "Wi-Fi");
        e.sem.check = Check::On;
        let on = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        let words: Vec<_> = on.fields.iter().map(|f| f.content.clone()).collect();
        assert_eq!(
            words,
            [
                Content::Text(Arc::from("Wi-Fi")),
                Content::word(M::RoleSwitch),
                Content::word(M::StateOn)
            ]
        );
        e.sem.check = Check::Off;
        let off = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(
            off.fields.last().map(|f| f.content.clone()),
            Some(Content::word(M::StateOff))
        );
        assert!(
            off.fields
                .iter()
                .all(|f| f.content != Content::word(M::StateNotPressed)),
            "never 'not pressed'"
        );
    }

    #[test]
    fn mixed_replaces_checked_and_not_checked() {
        let mut e = entry(SemRole::Checkbox, "All");
        e.sem.check = Check::Mixed;
        let ir = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        let states: Vec<_> = ir
            .of_kind(FieldKind::STATE)
            .map(|f| f.content.clone())
            .collect();
        assert_eq!(states, [Content::word(M::StateHalfChecked)]);
    }

    #[test]
    fn an_empty_name_adds_no_field_and_a_value_text_follows_the_states() {
        let mut e = entry(SemRole::Slider, "");
        e.sem.text = Some(Arc::new(SemText {
            value_text: Some(Arc::from("50 percent")),
            ..SemText::default()
        }));
        let ir = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(kinds(&ir), [FieldKind::ROLE, FieldKind::VALUE]);
    }

    #[test]
    fn help_text_is_spoken_for_fields_only() {
        let mut e = entry(SemRole::TextBox, "Email");
        e.sem.text = Some(Arc::new(SemText {
            description: Some(Arc::from("Required")),
            ..SemText::default()
        }));
        assert!(
            compose(&e, key(), Reason::Focus, CTX)
                .expect("focus")
                .of_kind(FieldKind::DESCRIPTION)
                .next()
                .is_some()
        );
        e.sem.role = SemRole::Button;
        assert!(
            compose(&e, key(), Reason::Focus, CTX)
                .expect("focus")
                .of_kind(FieldKind::DESCRIPTION)
                .next()
                .is_none()
        );
    }

    #[test]
    fn a_password_value_is_marked_secret_and_survives_redaction_as_empty() {
        let mut e = entry(SemRole::PasswordBox, "Password");
        e.sem.text = Some(Arc::new(SemText {
            value_text: Some(Arc::from("hunter2")),
            ..SemText::default()
        }));
        let ir = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert!(ir.flags.contains(IrFlags::SECRET));
        let v = ir.of_kind(FieldKind::VALUE).next().expect("value field");
        assert!(v.is_secret());
        let red = ir.redacted();
        assert_eq!(
            red.of_kind(FieldKind::VALUE)
                .next()
                .map(|f| f.content.clone()),
            Some(Content::Text(Arc::from("")))
        );
    }

    #[test]
    fn name_text_and_entry_share_one_allocation() {
        let e = entry(SemRole::Button, "OK");
        let ir = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        let (Some(Content::Text(a)), Some(b)) = (ir.fields.first().map(|f| &f.content), &e.name)
        else {
            unreachable!("a name field");
        };
        assert!(Arc::ptr_eq(a, b), "compose clones the Arc, not the text");
    }

    #[test]
    fn an_unknown_reason_has_no_row() {
        let e = entry(SemRole::Button, "OK");
        assert_eq!(
            compose(&e, key(), Reason::Unknown(999), CTX),
            Err(ComposeError::Unsupported(999))
        );
    }

    #[test]
    fn other_reasons_have_no_row_yet() {
        let e = entry(SemRole::Button, "OK");
        assert_eq!(
            compose(&e, key(), Reason::QuickNav, CTX),
            Err(ComposeError::Unsupported(Reason::QuickNav.code()))
        );
    }

    #[test]
    fn composition_is_a_function_of_entry_and_reason() {
        let mut e = entry(SemRole::Checkbox, "Mute");
        e.sem.check = Check::On;
        e.sem.flags = SemFlags::REQUIRED;
        let a = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        let b = compose(&e.clone(), key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(a, b);
    }
}
