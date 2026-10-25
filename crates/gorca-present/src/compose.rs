// SPDX-License-Identifier: GPL-3.0-or-later
//! `compose`: `SemEntry` + `Reason` to `PresIr` (ADR-0022 section 2.5). Pure; reads nothing but
//! the entry and the context, never the mirror or IPC.
//!
//! Slice 1 implements one rule table row: **`native.focus`** (`Reason::Focus` of a node that is
//! not web content). Field order is the legacy presenter's, so the shadow diff is meaningful:
//! name, role, states, value, help text.

use gorca_i18n::Lang;
use gorca_i18n::MsgId as M;
use gorca_model::NodeKey;
use gorca_model::sem::ProvCode;
use gorca_model::sem::{
    Avail, Check, Expand, Group, Invalid, Popup, Press, Select, SemEntry, SemFlags, SemRole,
};
use smallvec::SmallVec;

use std::sync::Arc;

use crate::ir::{
    Arg, Content, Field, FieldFlags, FieldKind, IrFlags, Origin, PresIr, Reason, RowId,
};
use crate::roles::{Class, native_role};

/// Rule row of the native focus utterance.
pub const ROW_NATIVE_FOCUS: RowId = RowId(1);

/// Interim report-flag set (ADR-0016 adopts it unchanged). Slice 1 has no verbosity bits in use;
/// the type exists so `compose` has its final signature.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ReportFlags(pub u32);

/// Everything `compose` needs besides the entry.
#[derive(Copy, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // one fact per bit of the call site
pub struct ComposeCtx {
    pub flags: ReportFlags,
    /// The entry was composed from stale data.
    pub may_refine: bool,
    /// The node is web content: the NVDA rows (`web.focus`) instead of the native ones.
    pub web: bool,
    /// A change in a switcher: being selected is not news (`Reason::Change` only).
    pub skip_selected: bool,
    /// `browse.hedge_inferred`: a name inferred from the layout is said as "probably ...".
    pub hedge_inferred: bool,
    /// The UI language the words that are baked into arguments (a negation) are taken in.
    pub lang: Lang,
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
/// The name field of an entry. A name the layout inference gave (provenance `Inferred`, the S4
/// lowest-trust fact) is an `INFERRED_LABEL` field, and `hedge_inferred` says it as "probably ...".
/// A name that says nothing: empty, white space, control characters, object markers or only icon
/// glyphs (a button named by its icon font). Counted as no name.
pub fn name_is_blank(name: Option<&str>) -> bool {
    name.is_none_or(|n| {
        n.chars().all(|c| {
            c.is_whitespace()
                || c.is_control()
                || gorca_model::text::is_object_marker(c)
                || gorca_model::text::is_icon_glyph(c)
        })
    })
}

/// Text as a one-line label: whitespace collapsed, icon glyphs and object markers dropped. `None`
/// when nothing speakable is left. The length is not limited (the renderer caps a spoken name).
pub fn collapse_label(s: &str) -> Option<String> {
    let kept: String = s
        .chars()
        .filter(|c| {
            !gorca_model::text::is_icon_glyph(*c) && !gorca_model::text::is_object_marker(*c)
        })
        .collect();
    let label = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name_is_blank(Some(&label))).then_some(label)
}

/// A name taken from the text around a control: [`collapse_label`] capped at [`LABEL_CAP`]
/// characters.
pub fn one_line_label(s: &str) -> Option<String> {
    let label = collapse_label(s)?;
    let capped: String = label.chars().take(LABEL_CAP).collect();
    (!name_is_blank(Some(&capped))).then_some(capped)
}

/// Longest label a nameless control takes from the text around it (a control is not read out as a
/// paragraph).
pub const LABEL_CAP: usize = 160;

/// Roles that are spoken with a name and take one from the description or the placeholder when
/// nothing gave one (Orca: `_generate_accessible_name` takes the description, the entry
/// presenters the placeholder).
const fn takes_fallback_name(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::Button
            | SemRole::Switch
            | SemRole::Checkbox
            | SemRole::MenuItemCheckbox
            | SemRole::Radio
            | SemRole::MenuItemRadio
            | SemRole::MenuItem
            | SemRole::Link
            | SemRole::Tab
            | SemRole::ListItem
            | SemRole::Option
            | SemRole::Term
            | SemRole::Definition
            | SemRole::Combobox
            | SemRole::TextBox
            | SemRole::SearchBox
            | SemRole::PasswordBox
            | SemRole::SpinButton
            | SemRole::Slider
            | SemRole::Scrollbar
    )
}

/// What the name field of an entry came to.
pub(crate) struct NameOut {
    pub field: Option<Field>,
    /// The description became the name: it is not said again after it.
    pub from_description: bool,
}

/// The name field of an entry. A name the layout inference gave (provenance `Inferred`, the S4
/// lowest-trust fact) is an `INFERRED_LABEL` field, and `hedge_inferred` says it as "probably ...".
/// In web content a nameless control that takes one gets the one-line description, else the
/// placeholder, as its name (the C3b ladder: description, placeholder, inferred label).
pub(crate) fn name_field(entry: &SemEntry, ctx: ComposeCtx, row: RowId) -> NameOut {
    let origin = Origin {
        row,
        group: Group::Name.slot(),
        via: 0,
    };
    let own = entry
        .name
        .as_ref()
        .filter(|s| !name_is_blank(Some(s.as_ref())));
    let Some(name) = own else {
        if ctx.web && takes_fallback_name(entry.sem.role) {
            let text = entry.sem.text.as_deref();
            let from_desc = text
                .and_then(|t| t.description.as_deref())
                .and_then(one_line_label);
            if let Some(n) = from_desc {
                return NameOut {
                    field: Some(Field::new(
                        FieldKind::NAME,
                        Content::Text(Arc::from(n)),
                        origin,
                    )),
                    from_description: true,
                };
            }
            let ph = text
                .and_then(|t| t.placeholder.as_deref())
                .and_then(one_line_label);
            if let Some(n) = ph {
                return NameOut {
                    field: Some(Field::new(
                        FieldKind::NAME,
                        Content::Text(Arc::from(n)),
                        origin,
                    )),
                    from_description: false,
                };
            }
        }
        return NameOut {
            field: None,
            from_description: false,
        };
    };
    // A name is spoken as a one-line label (collapsed white space, icon glyphs dropped, capped):
    // a name derived from content is a subtree's text, and the label does not depend on where the
    // name came from (P2.3: provenance is no input of the words).
    let text: Arc<str> = match collapse_label(name) {
        Some(t) if t != **name => Arc::from(t),
        _ => Arc::clone(name),
    };
    if entry.prov.get(Group::Name) != Some(ProvCode::Inferred) {
        return NameOut {
            field: Some(Field::new(FieldKind::NAME, Content::Text(text), origin)),
            from_description: false,
        };
    }
    let content = if ctx.hedge_inferred {
        Content::msg(M::HedgeProbably, vec![Arg::text("text", &text)])
    } else {
        Content::Text(text)
    };
    NameOut {
        field: Some(Field::new(FieldKind::INFERRED_LABEL, content, origin)),
        from_description: false,
    }
}

const fn tri_state_word(class: Class) -> Option<M> {
    match class {
        Class::Checkable => Some(M::StateHalfChecked),
        Class::Toggle => Some(M::StateHalfPressed),
        Class::Progress => Some(M::StateIndeterminate),
        Class::Switch | Class::Field | Class::Other => None,
    }
}

/// Whether the fused entry is a secret field (a password box): nothing that comes from its
/// contents may be spoken, logged or routed. The one test every path uses together with the native
/// role of the snapshot (`gorca-core`'s `is_secret`); a field of the IR carries
/// [`FieldFlags::SECRET`] and goes through [`PresIr::redacted`] before any sink that must not see it.
#[must_use]
pub fn is_secret_entry(entry: &SemEntry) -> bool {
    is_secret_role(entry.sem.role) || entry.sem.flags.contains(gorca_model::sem::SemFlags::SECRET)
}

/// The same rule for a bare fused role (plugin rows and source records are not entries yet).
#[must_use]
pub fn is_secret_role(role: SemRole) -> bool {
    role == SemRole::PasswordBox
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
        Reason::Focus if ctx.web => Ok(crate::web::web_focus(entry, node, ctx)),
        Reason::Focus => Ok(native_focus(entry, node, ctx)),
        Reason::WhereAmI => {
            // One node of the ancestry answer: what a focus on it would say, composed for this
            // reason (the question decides which parts are used, not the words).
            let mut ir = if ctx.web {
                crate::web::web_focus(entry, node, ctx)
            } else {
                native_focus(entry, node, ctx)
            };
            ir.reason = Reason::WhereAmI;
            // The question asks for the state of the node itself, whatever a focus on web content
            // keeps quiet about: a disabled item is unavailable, a selected item is selected.
            if entry.sem.avail == Avail::Disabled
                && !ir.fields.iter().any(
                    |f| matches!(&f.content, Content::Msg { id, .. } if *id == M::StateUnavailable),
                )
            {
                let at = ir
                    .fields
                    .iter()
                    .position(|f| f.kind == FieldKind::STATE)
                    .unwrap_or(ir.fields.len());
                ir.fields.insert(
                    at,
                    word(FieldKind::STATE, M::StateUnavailable, Group::Avail),
                );
            }
            if entry.sem.select == Select::On
                && !ir.fields.iter().any(
                    |f| matches!(&f.content, Content::Msg { id, .. } if *id == M::StateSelected),
                )
            {
                let at = ir
                    .fields
                    .iter()
                    .rposition(|f| f.kind == FieldKind::STATE)
                    .map_or(ir.fields.len(), |i| i + 1);
                ir.fields
                    .insert(at, word(FieldKind::STATE, M::StateSelected, Group::Select));
            }
            // Validity and a popup are facts of the node too: the focus rows keep them to a few
            // roles (a popup is expressed as expanded or collapsed, a container's validity is the
            // engine's guess), the question lists them whenever the model has them.
            let spoken = |ir: &PresIr, ids: &[M]| {
                ir.fields
                    .iter()
                    .any(|f| matches!(&f.content, Content::Msg { id, .. } if ids.contains(id)))
            };
            let after_states = |ir: &PresIr| {
                ir.fields
                    .iter()
                    .rposition(|f| f.kind == FieldKind::STATE)
                    .map_or(ir.fields.len(), |i| i + 1)
            };
            if entry.sem.invalid != Invalid::No && !spoken(&ir, &[M::StateInvalidEntry]) {
                let at = after_states(&ir);
                ir.fields.insert(
                    at,
                    word(FieldKind::STATE, M::StateInvalidEntry, Group::Invalid),
                );
            }
            if entry.sem.popup != Popup::No
                && !spoken(
                    &ir,
                    &[
                        M::StateHasPopup,
                        M::StateSubmenu,
                        M::StateOpensDialog,
                        M::StateOpensGrid,
                        M::StateOpensList,
                        M::StateOpensTree,
                    ],
                )
            {
                let at = after_states(&ir);
                ir.fields
                    .insert(at, word(FieldKind::STATE, M::StateHasPopup, Group::Popup));
            }
            // The answer to "where am I" is never empty: a node the focus rows say nothing for
            // (an unnamed generic container) still names its role.
            if ir.fields.is_empty() {
                // The engine's text runs have no fused role: in web content they are text.
                let w = native_role(entry.sem.role, entry.sem.press)
                    .word
                    .or_else(|| ctx.web.then_some(M::RoleStaticText));
                if let Some(w) = w {
                    ir.fields.push(word(FieldKind::ROLE, w, Group::Role));
                }
            }
            Ok(ir)
        }
        Reason::Review => Ok(crate::zone::review(entry, node, ctx)),
        Reason::Window => Ok(crate::desktop::compose_window(entry, node, ctx)),
        Reason::Notice => Ok(crate::desktop::compose_notice(entry, node, ctx, &[])),
        Reason::FocusAncestor
        | Reason::BrowseLine { .. }
        | Reason::QuickNav
        | Reason::SayAll
        | Reason::Change { .. }
        | Reason::ListEntry
        | Reason::Ext
        | Reason::Unknown { .. } => Err(ComposeError::Unsupported(reason.code())),
    }
}

fn native_focus(entry: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let sem = &entry.sem;
    let info = native_role(sem.role, sem.press);
    let class = info.class;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();

    if let Some(f) = name_field(entry, ctx, ROW_NATIVE_FOCUS).field {
        fields.push(f);
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

    let secret = is_secret_entry(entry);
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
    #[test]
    fn a_secret_mark_makes_an_entry_secret_whatever_its_fused_role() {
        let mut e = SemEntry::default();
        e.sem.role = SemRole::TextBox;
        assert!(!is_secret_entry(&e));
        e.sem.flags |= gorca_model::sem::SemFlags::SECRET;
        assert!(is_secret_entry(&e));
        let mut p = SemEntry::default();
        p.sem.role = SemRole::PasswordBox;
        assert!(is_secret_entry(&p));
    }

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
        web: false,
        skip_selected: false,
        hedge_inferred: false,
        lang: gorca_i18n::Lang::En,
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
    fn where_am_i_keeps_selected_and_unavailable_and_is_never_empty() {
        use gorca_model::sem::{Avail, Invalid, Popup, Select};
        let mut web = CTX;
        web.web = true;
        // A disabled, selected list item: a web focus keeps quiet about both, the question does not.
        let mut e = entry(SemRole::ListItem, "Pro");
        e.sem.avail = Avail::Disabled;
        e.sem.select = Select::On;
        let focus = compose(&e, key(), Reason::Focus, web).expect("focus");
        let ask = compose(&e, key(), Reason::WhereAmI, web).expect("where am i");
        assert_eq!(ask.reason, Reason::WhereAmI);
        let says = |ir: &PresIr, id: M| {
            ir.fields
                .iter()
                .any(|f| matches!(&f.content, Content::Msg { id: m, .. } if *m == id))
        };
        assert!(!says(&focus, M::StateSelected));
        assert!(says(&ask, M::StateSelected) && says(&ask, M::StateUnavailable));
        // A popup and a validity the focus rows keep quiet about are kept by the question.
        let mut f = entry(SemRole::Button, "Menu");
        f.sem.popup = Popup::Dialog;
        f.sem.invalid = Invalid::True;
        let ir = compose(&f, key(), Reason::WhereAmI, CTX).expect("where am i");
        assert!(says(&ir, M::StateHasPopup) && says(&ir, M::StateInvalidEntry));
        // A node the focus rows say nothing for still names its role.
        let bare = entry(SemRole::Generic, "");
        let ir = compose(&bare, key(), Reason::WhereAmI, CTX).expect("where am i");
        assert!(!ir.fields.is_empty());
        // A web text run with no fused role is text.
        let run = entry(SemRole::Unknown, "");
        let ir = compose(&run, key(), Reason::WhereAmI, web).expect("where am i");
        assert!(says(&ir, M::RoleStaticText));
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
    fn an_inferred_name_is_its_own_field_kind_and_hedges_on_request() {
        let mut e = entry(SemRole::TextBox, "First name");
        e.prov.set(Group::Name, ProvCode::Inferred);
        let plain = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert_eq!(plain.fields[0].kind, FieldKind::INFERRED_LABEL);
        assert_eq!(
            plain.fields[0].content,
            Content::Text(Arc::from("First name"))
        );
        let hedged = ComposeCtx {
            hedge_inferred: true,
            ..CTX
        };
        let ir = compose(&e, key(), Reason::Focus, hedged).expect("focus");
        let words = crate::render::render(&ir, crate::render::RenderOpts::default());
        assert_eq!(
            words[0].text.as_str(),
            gorca_i18n::tf(
                M::HedgeProbably,
                &[("text", gorca_i18n::Arg::Str("First name"))]
            )
        );
        assert!(words[0].text.as_str().contains("First name"));
        // The same through the web row, and an observed name is never hedged.
        let web = ComposeCtx {
            web: true,
            ..hedged
        };
        let ir = compose(&e, key(), Reason::Focus, web).expect("focus");
        assert_eq!(ir.fields[0].kind, FieldKind::INFERRED_LABEL);
        e.prov.set(Group::Name, ProvCode::ObservedPrimary);
        let ir = compose(&e, key(), Reason::Focus, hedged).expect("focus");
        assert_eq!(ir.fields[0].kind, FieldKind::NAME);
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

    /// Reviewer audit (M22c part 2): the source's secret mark on a fused text box must flag the
    /// value as secret too, as `is_secret_entry` says the entry is secret.
    #[test]
    fn audit_a_marked_text_box_value_is_secret_and_redacted() {
        let mut e = entry(SemRole::TextBox, "Password");
        e.sem.flags |= gorca_model::sem::SemFlags::SECRET;
        e.sem.text = Some(Arc::new(SemText {
            value_text: Some(Arc::from("hunter2")),
            ..SemText::default()
        }));
        assert!(is_secret_entry(&e));
        let ir = compose(&e, key(), Reason::Focus, CTX).expect("focus");
        assert!(ir.flags.contains(IrFlags::SECRET), "IR not flagged secret");
        let red = ir.redacted();
        assert!(
            !format!("{:?}", red.ir()).contains("hunter2"),
            "redacted IR still carries the value"
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
            compose(&e, key(), Reason::unknown(999), CTX),
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
