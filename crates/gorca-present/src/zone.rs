// SPDX-License-Identifier: GPL-3.0-or-later
//! Review words (ADR-0022 section 2.5, M22b3): what flat review and object review say for a whole
//! object (ADR-0006 6.3 rule 2): the name, a role word for controls, the state words and the
//! value. "Wi-Fi toggle button checked", "Apply button", "Volume slider 40".
//!
//! The role word of a node whose own text is the content (a label, a paragraph, a list item, a
//! table cell) adds nothing in a line of text and is left out. Only the positive states are said
//! ("checked", never "not checked"): a zone is a line of text, not a focus announcement.

use std::sync::Arc;

use gorca_i18n::MsgId as M;
use gorca_model::NodeKey;
use gorca_model::sem::{
    Avail, Check, Expand, Group, Invalid, Press, Select, SemEntry, SemFlags, SemRole,
};
use smallvec::SmallVec;

use crate::compose::{ComposeCtx, name_field};
use crate::ir::{Content, Field, FieldFlags, FieldKind, IrFlags, Origin, PresIr, Reason, RowId};
use crate::roles::native_role;

/// Rule row of the review words.
pub const ROW_REVIEW: RowId = RowId(6);

fn origin(group: Group) -> Origin {
    Origin {
        row: ROW_REVIEW,
        group: group.slot(),
        via: 0,
    }
}

fn word(kind: FieldKind, id: M, group: Group) -> Field {
    Field::new(kind, Content::word(id), origin(group))
}

/// Roles whose own name is the content: the role word adds nothing in a line of text.
pub const fn plain_text_role(role: SemRole) -> bool {
    match role {
        SemRole::Unknown
        | SemRole::Caption
        | SemRole::StaticText
        | SemRole::Paragraph
        | SemRole::ListItem
        | SemRole::Option
        | SemRole::Term
        | SemRole::Definition
        | SemRole::Cell
        | SemRole::GridCell
        | SemRole::Row
        | SemRole::ColumnHeader
        | SemRole::RowHeader => true,
        SemRole::Generic
        | SemRole::Alert
        | SemRole::AlertDialog
        | SemRole::Application
        | SemRole::Article
        | SemRole::Banner
        | SemRole::Blockquote
        | SemRole::Button
        | SemRole::Checkbox
        | SemRole::Code
        | SemRole::Combobox
        | SemRole::Complementary
        | SemRole::ContentInfo
        | SemRole::Deletion
        | SemRole::Dialog
        | SemRole::Document
        | SemRole::DocumentWeb
        | SemRole::Feed
        | SemRole::Figure
        | SemRole::Form
        | SemRole::Grid
        | SemRole::Group
        | SemRole::Heading
        | SemRole::Img
        | SemRole::Insertion
        | SemRole::Link
        | SemRole::List
        | SemRole::Listbox
        | SemRole::Log
        | SemRole::Main
        | SemRole::Mark
        | SemRole::Marquee
        | SemRole::Math
        | SemRole::Menu
        | SemRole::Menubar
        | SemRole::MenuItem
        | SemRole::MenuItemCheckbox
        | SemRole::MenuItemRadio
        | SemRole::Meter
        | SemRole::Navigation
        | SemRole::Note
        | SemRole::ProgressBar
        | SemRole::Radio
        | SemRole::RadioGroup
        | SemRole::Region
        | SemRole::RowGroup
        | SemRole::Scrollbar
        | SemRole::Search
        | SemRole::SearchBox
        | SemRole::Separator
        | SemRole::Slider
        | SemRole::SpinButton
        | SemRole::Status
        | SemRole::Switch
        | SemRole::Tab
        | SemRole::Table
        | SemRole::TabList
        | SemRole::TabPanel
        | SemRole::TextBox
        | SemRole::PasswordBox
        | SemRole::Timer
        | SemRole::Toolbar
        | SemRole::Tooltip
        | SemRole::Tree
        | SemRole::TreeGrid
        | SemRole::TreeItem
        | SemRole::Terminal
        | SemRole::Window
        | SemRole::Popover
        | SemRole::Comment
        | SemRole::Suggestion
        | SemRole::Details
        | SemRole::Summary
        | SemRole::DescriptionList
        | SemRole::Time
        | SemRole::Iframe
        | SemRole::Emphasis
        | SemRole::Strong
        | SemRole::Subscript
        | SemRole::Superscript
        | SemRole::Presentation => false,
    }
}

/// The review IR of `entry`: the name, the role word of a control, the positive state words and
/// the value. The words are the native role table's for every source: review has always read an
/// object with the coarse words ("tab list", "tree item"), and a web page is no different from
/// an application there; the NVDA words are the web focus row's and browse mode's.
pub(crate) fn review(entry: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let sem = &entry.sem;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    // No fallback name from the description or the placeholder: review says what the object is
    // called, not what focus would call it.
    let named = name_field(entry, ComposeCtx { web: false, ..ctx }, ROW_REVIEW);
    if let Some(mut f) = named.field {
        f.origin.row = ROW_REVIEW;
        fields.push(f);
    }
    if !plain_text_role(sem.role)
        && let Some(w) = review_role_word(sem.role, sem.press)
    {
        fields.push(word(FieldKind::ROLE, w, Group::Role));
    }
    if sem.avail == Avail::Disabled {
        fields.push(word(FieldKind::STATE, M::StateUnavailable, Group::Avail));
    }
    if sem.check == Check::Mixed || sem.press == Press::Mixed {
        let g = if sem.check == Check::Mixed {
            Group::Check
        } else {
            Group::Press
        };
        fields.push(word(FieldKind::STATE, M::StateHalfChecked, g));
    }
    if sem.check == Check::On {
        fields.push(word(FieldKind::STATE, M::StateChecked, Group::Check));
    }
    if sem.press == Press::On {
        fields.push(word(FieldKind::STATE, M::StatePressed, Group::Press));
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
    let secret = crate::compose::is_secret_entry(entry);
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
    let mut ir = PresIr::new(Reason::Review, node, entry.sem_rev, fields);
    ir.may_refine = ctx.may_refine;
    if secret {
        ir.flags = IrFlags::SECRET;
    }
    ir
}

/// The catalog word of the role of `entry`, as review says it for a nameless object ("button",
/// "slider"). `None` when the role has no word.
pub fn role_word_of(entry: &SemEntry, web: bool) -> Option<M> {
    if web && entry.sem.role.is_inline_text() {
        return Some(M::RoleStaticText);
    }
    review_role_word(entry.sem.role, entry.sem.press)
}

/// The role word review says for a role: the native table's, with the role the legacy
/// mapping spoke differently (the engine exports a form as a landmark).
pub fn review_role_word(role: SemRole, press: Press) -> Option<M> {
    match role {
        SemRole::Form => Some(M::RoleLandmark),
        _ => native_role(role, press).word,
    }
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::SemText;
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;
    use crate::compose::{ReportFlags, compose};
    use crate::render::{RenderOpts, render};

    fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(3),
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
        e
    }

    fn say_in(e: &SemEntry, ctx: ComposeCtx, lang: gorca_i18n::Lang) -> String {
        let ir = compose(e, key(), Reason::Review, ctx).expect("review row");
        let opts = RenderOpts {
            lang,
            ..RenderOpts::default()
        };
        render(&ir, opts)
            .iter()
            .map(|t| t.text.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn say(e: &SemEntry) -> String {
        say_in(e, CTX, gorca_i18n::Lang::En)
    }

    #[test]
    fn a_control_says_name_role_and_state() {
        let mut e = entry(SemRole::Checkbox, "Wi-Fi");
        e.sem.check = Check::On;
        assert_eq!(say(&e), "Wi-Fi check box checked");
        assert_eq!(say(&entry(SemRole::Button, "Apply")), "Apply button");
    }

    #[test]
    fn only_positive_states_are_said() {
        let mut e = entry(SemRole::Checkbox, "Mute");
        e.sem.check = Check::Off;
        assert_eq!(
            say(&e),
            "Mute check box",
            "no 'not checked' in a line of text"
        );
        e.sem.check = Check::Mixed;
        assert_eq!(say(&e), "Mute check box half checked");
        let mut s = entry(SemRole::Switch, "Dark");
        s.sem.check = Check::Off;
        assert_eq!(say(&s), "Dark switch", "no 'off' either");
    }

    #[test]
    fn states_come_in_the_legacy_order() {
        let mut e = entry(SemRole::TreeItem, "src");
        e.sem.avail = Avail::Disabled;
        e.sem.select = Select::On;
        e.sem.expand = Expand::Expanded;
        e.sem.flags = SemFlags::REQUIRED;
        e.sem.invalid = Invalid::True;
        assert_eq!(
            say(&e),
            "src tree item unavailable selected expanded required invalid entry"
        );
    }

    #[test]
    fn a_range_says_its_value_after_the_role() {
        let mut e = entry(SemRole::Slider, "Volume");
        e.sem.text = Some(Arc::new(SemText {
            value_text: Some(Arc::from("40")),
            ..SemText::default()
        }));
        assert_eq!(say(&e), "Volume slider 40");
    }

    #[test]
    fn a_plain_text_role_says_no_role_word_and_help_text_is_not_part_of_a_zone() {
        let e = entry(SemRole::StaticText, "Hello");
        assert_eq!(say(&e), "Hello");
        let mut e = entry(SemRole::TextBox, "Email");
        e.sem.text = Some(Arc::new(SemText {
            description: Some(Arc::from("help me")),
            ..SemText::default()
        }));
        let ir = compose(&e, key(), Reason::Review, CTX).expect("review row");
        assert!(ir.of_kind(FieldKind::DESCRIPTION).next().is_none());
        assert_eq!(ir.reason, Reason::Review);
        assert!(ir.fields.iter().all(|f| f.origin.row == ROW_REVIEW));
        assert_eq!(say(&e), "Email edit");
    }

    #[test]
    fn a_nameless_object_has_a_role_word_and_a_label_has_none() {
        assert_eq!(
            role_word_of(&entry(SemRole::Button, ""), false),
            Some(M::RoleButton)
        );
        let e = entry(SemRole::Unknown, "");
        assert_eq!(role_word_of(&e, false), None);
        assert!(say(&e).is_empty());
    }

    #[test]
    fn web_and_native_content_are_spoken_alike() {
        let web = ComposeCtx { web: true, ..CTX };
        let mut e = entry(SemRole::TabList, "Pages");
        e.sem.expand = Expand::NotExpandable;
        assert_eq!(
            say_in(&e, web, gorca_i18n::Lang::En),
            say_in(&e, CTX, gorca_i18n::Lang::En)
        );
    }

    #[test]
    fn a_password_value_is_secret_and_the_ir_is_marked() {
        let mut e = entry(SemRole::PasswordBox, "Password");
        e.sem.text = Some(Arc::new(SemText {
            value_text: Some(Arc::from("hunter2")),
            ..SemText::default()
        }));
        let ir = compose(&e, key(), Reason::Review, CTX).expect("review row");
        assert!(ir.flags.contains(IrFlags::SECRET));
        assert!(ir.of_kind(FieldKind::VALUE).all(Field::is_secret));
    }

    #[test]
    fn the_russian_catalog_is_used() {
        let ctx = ComposeCtx {
            lang: gorca_i18n::Lang::Ru,
            ..CTX
        };
        assert_eq!(
            say_in(&entry(SemRole::Button, "OK"), ctx, gorca_i18n::Lang::Ru),
            format!(
                "OK {}",
                gorca_i18n::t_in(gorca_i18n::Lang::Ru, M::RoleButton)
            )
        );
    }
}
