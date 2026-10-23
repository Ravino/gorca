// SPDX-License-Identifier: GPL-3.0-or-later
//! The web focus row (`web.focus`, ADR-0022 section 2.5): NVDA's words and state order for web
//! content, composed from the fused entry alone. The legacy presenter reads ARIA attributes off
//! the snapshot; here every fact is a fused group, so the same rule serves every web source.
//!
//! Field order: name, role, states (NVDA order), current, value, description, level, error text.

use std::sync::Arc;

use gorca_i18n::MsgId as M;
use gorca_model::NodeKey;
use gorca_model::sem::{
    Avail, Check, Current, Expand, Group, Invalid, Popup, Press, Select, SemEntry, SemFlags,
    SemRole,
};
use smallvec::SmallVec;

use crate::compose::ComposeCtx;
use crate::ir::{Content, Field, FieldFlags, FieldKind, IrFlags, Origin, PresIr, Reason, RowId};
use crate::roles::{Class, native_role};

/// Rule row of the web focus utterance.
pub const ROW_WEB_FOCUS: RowId = RowId(2);

fn origin(group: Group) -> Origin {
    Origin {
        row: ROW_WEB_FOCUS,
        group: group.slot(),
        via: 0,
    }
}

fn word(kind: FieldKind, id: M, group: Group) -> Field {
    Field::new(kind, Content::word(id), origin(group))
}

/// NVDA's label for an ARIA role that differs from the coarse native word (`controlTypes/role.py`).
const fn aria_word(role: SemRole) -> Option<M> {
    Some(match role {
        SemRole::Switch => M::AriaSwitch,
        SemRole::Tab => M::AriaTab,
        SemRole::TabList => M::AriaTabList,
        SemRole::TabPanel => M::AriaTabPanel,
        SemRole::Tree => M::AriaTree,
        SemRole::TreeItem => M::AriaTreeItem,
        SemRole::Menu => M::AriaMenu,
        SemRole::Menubar => M::AriaMenuBar,
        SemRole::MenuItem => M::AriaMenuItem,
        SemRole::MenuItemCheckbox => M::AriaCheckMenuItem,
        SemRole::MenuItemRadio => M::AriaRadioMenuItem,
        SemRole::Toolbar => M::AriaToolbar,
        SemRole::Group | SemRole::RadioGroup => M::AriaGroup,
        _ => return None,
    })
}

/// The role word of web content: the ARIA word, else the coarse native word, with the web
/// differences: static text says "text" and a caption "label", and a *named* form is a landmark.
/// Roles without a word (marquee, note, inline text roles) say nothing, as NVDA does.
fn role_word(role: SemRole, press: Press, named: bool) -> Option<M> {
    if let Some(w) = aria_word(role) {
        return Some(w);
    }
    match role {
        SemRole::StaticText => Some(M::RoleStaticText),
        SemRole::Caption => Some(M::RoleLabel),
        SemRole::Form => named.then_some(M::RoleLandmark),
        _ => native_role(role, press).word,
    }
}

/// Roles that are never "unavailable" in web content (static structure: Gecko reports no
/// Sensitive on it). The legacy presenter's `never_disabled` set, through the legacy family.
fn never_disabled(role: SemRole) -> bool {
    use gorca_model::Role as R;
    matches!(
        role.legacy_family(),
        R::DocumentWeb
            | R::Document
            | R::Heading
            | R::Paragraph
            | R::Image
            | R::List
            | R::ListItem
            | R::Table
            | R::Row
            | R::Cell
            | R::ColumnHeader
            | R::RowHeader
            | R::Landmark
            | R::StaticText
            | R::Separator
    ) || matches!(role, SemRole::Form)
}

/// How the checked state is said for a role.
#[derive(Copy, Clone, Eq, PartialEq)]
enum CheckStyle {
    None,
    Checked,
    Pressed,
    OnOff,
}

fn check_style(role: SemRole, press: Press) -> CheckStyle {
    match role {
        SemRole::Switch => CheckStyle::OnOff,
        SemRole::MenuItemCheckbox | SemRole::MenuItemRadio | SemRole::Checkbox | SemRole::Radio => {
            CheckStyle::Checked
        }
        _ if native_role(role, press).class == Class::Toggle => CheckStyle::Pressed,
        _ => CheckStyle::None,
    }
}

fn popup_word(popup: Popup, role: SemRole) -> Option<M> {
    match popup {
        Popup::No => None,
        Popup::Menu => (role != SemRole::Combobox).then_some(M::StateSubmenu),
        Popup::Dialog => Some(M::StateOpensDialog),
        Popup::Grid => Some(M::StateOpensGrid),
        Popup::Listbox => (role != SemRole::Combobox).then_some(M::StateOpensList),
        Popup::Tree => Some(M::StateOpensTree),
    }
}

fn current_word(c: Current) -> Option<M> {
    match c {
        Current::No => None,
        Current::True => Some(M::StateCurrent),
        Current::Page => Some(M::StateCurrentPage),
        Current::Step => Some(M::StateCurrentStep),
        Current::Location => Some(M::StateCurrentLocation),
        Current::Date => Some(M::StateCurrentDate),
        Current::Time => Some(M::StateCurrentTime),
    }
}

/// Compose the web focus IR of `entry`.
pub(crate) fn web_focus(entry: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let sem = &entry.sem;
    let role = sem.role;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();

    let name = entry.name.as_ref().filter(|s| !s.is_empty());
    if let Some(name) = name {
        fields.push(Field::new(
            FieldKind::NAME,
            Content::Text(Arc::clone(name)),
            origin(Group::Name),
        ));
    }
    if let Some(w) = role_word(role, sem.press, name.is_some()) {
        fields.push(word(FieldKind::ROLE, w, Group::Role));
    }

    // States in NVDA's `State` order; some sort after required/invalid (`late`).
    let mut late: SmallVec<[Field; 3]> = SmallVec::new();
    let menu_item = matches!(
        role,
        SemRole::MenuItem | SemRole::MenuItemCheckbox | SemRole::MenuItemRadio
    );
    let quiet_selected = menu_item
        || matches!(
            role,
            SemRole::ListItem | SemRole::Option | SemRole::TreeItem
        );
    let selectable = sem.select != Select::NotSelectable;
    if sem.avail == Avail::Disabled && !never_disabled(role) {
        fields.push(word(FieldKind::STATE, M::StateUnavailable, Group::Avail));
    }
    if sem.select == Select::On && !(quiet_selected && selectable) {
        fields.push(word(FieldKind::STATE, M::StateSelected, Group::Select));
    } else if quiet_selected
        && !menu_item
        && sem.select == Select::Off
        && sem.flags.contains(SemFlags::FOCUSABLE)
    {
        // NVDA says what is not selected only when the node takes the focus.
        fields.push(word(FieldKind::STATE, M::StateNotSelected, Group::Select));
    }
    if sem.flags.contains(SemFlags::BUSY) {
        fields.push(word(FieldKind::STATE, M::StateBusy, Group::Flags));
    }
    let mixed = sem.check == Check::Mixed || sem.press == Press::Mixed;
    let on = sem.check == Check::On;
    let pressed = sem.press == Press::On;
    match check_style(role, sem.press) {
        CheckStyle::OnOff => late.push(word(
            FieldKind::STATE,
            if on || pressed {
                M::StateOn
            } else {
                M::StateOff
            },
            Group::Check,
        )),
        CheckStyle::Checked => fields.push(word(
            FieldKind::STATE,
            if mixed {
                M::StateHalfChecked
            } else if on {
                M::StateChecked
            } else {
                M::StateNotChecked
            },
            Group::Check,
        )),
        CheckStyle::Pressed => {
            if mixed {
                late.push(word(FieldKind::STATE, M::StateHalfPressed, Group::Press));
            } else if pressed || on {
                fields.push(word(FieldKind::STATE, M::StatePressed, Group::Press));
            } else {
                fields.push(word(FieldKind::STATE, M::StateNotPressed, Group::Press));
            }
        }
        CheckStyle::None => {
            if pressed {
                fields.push(word(FieldKind::STATE, M::StatePressed, Group::Press));
            }
            if on {
                fields.push(word(FieldKind::STATE, M::StateChecked, Group::Check));
            }
            if mixed && matches!(role, SemRole::ProgressBar | SemRole::Meter) {
                late.push(word(FieldKind::STATE, M::StateIndeterminate, Group::Check));
            }
        }
    }
    if sem.flags.contains(SemFlags::READ_ONLY)
        && matches!(
            role,
            SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox | SemRole::Checkbox
        )
    {
        fields.push(word(FieldKind::STATE, M::StateReadOnly, Group::Flags));
    }
    let popup = popup_word(sem.popup, role);
    if !(menu_item && popup.is_some()) {
        match sem.expand {
            Expand::Expanded => {
                fields.push(word(FieldKind::STATE, M::StateExpanded, Group::Expand))
            }
            Expand::Collapsed => {
                fields.push(word(FieldKind::STATE, M::StateCollapsed, Group::Expand));
            }
            Expand::NotExpandable => {}
        }
    }
    if let Some(p) = popup {
        let f = word(FieldKind::STATE, p, Group::Popup);
        if p == M::StateSubmenu {
            fields.push(f);
        } else {
            late.push(f);
        }
    }
    if sem.flags.contains(SemFlags::REQUIRED) {
        fields.push(word(FieldKind::STATE, M::StateRequired, Group::Flags));
    }
    let landmark_or_panel = matches!(
        role,
        SemRole::Region
            | SemRole::Banner
            | SemRole::Complementary
            | SemRole::ContentInfo
            | SemRole::Main
            | SemRole::Navigation
            | SemRole::Search
            | SemRole::Article
            | SemRole::Generic
            | SemRole::Group
            | SemRole::Presentation
            | SemRole::Form
    );
    if sem.invalid != Invalid::No && !landmark_or_panel {
        fields.push(word(FieldKind::STATE, M::StateInvalidEntry, Group::Invalid));
    }
    fields.extend(late);

    if let Some(c) = current_word(sem.current) {
        fields.push(word(FieldKind::STATE, c, Group::Current));
    }
    let secret = role == SemRole::PasswordBox;
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
    // A description worth saying: not empty and not a repeat of the name.
    if let Some(d) = sem.text.as_deref().and_then(|t| t.description.as_ref()) {
        let trimmed = d.trim();
        let repeat = name.is_some_and(|n| n.trim().eq_ignore_ascii_case(trimmed));
        if !trimmed.is_empty() && !repeat {
            fields.push(Field::new(
                FieldKind::DESCRIPTION,
                Content::Text(Arc::clone(d)),
                origin(Group::Description),
            ));
        }
    }
    if sem.level > 0 {
        // Tree items and headings say their level; other roles keep it to themselves.
        if matches!(role, SemRole::TreeItem | SemRole::Heading) {
            fields.push(Field::new(
                FieldKind::LEVEL,
                Content::msg(
                    M::StateLevel,
                    vec![crate::ir::Arg::num("l", i64::from(sem.level))],
                ),
                origin(Group::Level),
            ));
        }
    }
    if sem.invalid != Invalid::No
        && let Some(e) = sem
            .text
            .as_deref()
            .and_then(|t| t.error_text.as_ref())
            .filter(|t| !t.trim().is_empty())
    {
        fields.push(Field::new(
            FieldKind::ERROR_TEXT,
            Content::Text(Arc::clone(e)),
            origin(Group::Invalid),
        ));
    }

    let mut ir = PresIr::new(Reason::Focus, node, entry.sem_rev, fields);
    ir.may_refine = ctx.may_refine;
    if secret {
        ir.flags = IrFlags::SECRET;
    }
    ir
}
