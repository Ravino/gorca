// SPDX-License-Identifier: GPL-3.0-or-later
//! Browse-mode words (ADR-0022 section 2.5, M22b2): what a node says when the browse cursor
//! enters it, when it is a control on its own line, when it is a container, and when quick
//! navigation lands on it. Every function reads the fused entry alone; the document walk (which
//! nodes a line covers, where their text starts) stays in `gorca-core`, which passes the few
//! structural facts an entry cannot hold (the number of items of a list, the size of a table) in
//! [`ContainerFacts`].
//!
//! The words are the web focus row's (NVDA's labels and state order, `web.rs`); only the choice of
//! words differs: a link or a heading says less than a button, a container says its size.

use std::sync::Arc;

use gorca_i18n::MsgId as M;
use gorca_model::sem::{
    Current, Expand, Group, Invalid, NameFrom, Press, SemEntry, SemFlags, SemRole,
};
use smallvec::SmallVec;

use crate::ir::{Arg, Content, Field, FieldKind, Origin, RowId};
use crate::roles::native_role;
use crate::web::{aria_word, web_state_fields};

/// Rule row of the browse words.
pub const ROW_BROWSE: RowId = RowId(5);

fn origin(group: Group) -> Origin {
    Origin {
        row: ROW_BROWSE,
        group: group.slot(),
        via: 0,
    }
}

fn word(kind: FieldKind, id: M, group: Group) -> Field {
    Field::new(kind, Content::word(id), origin(group))
}

fn text_field(kind: FieldKind, text: &Arc<str>, group: Group) -> Field {
    Field::new(kind, Content::Text(Arc::clone(text)), origin(group))
}

/// Fields a function of this module produces.
pub type Fields = SmallVec<[Field; 6]>;

/// Is the node announced where it starts, by role: links, headings, controls, graphics and the
/// ARIA items (tab, tree item, menu item, switch)?
pub fn announced(entry: &SemEntry) -> bool {
    // A plain menu item the engine made (the options of a select) is not announced; one the
    // author wrote is.
    if entry.sem.role == SemRole::MenuItem {
        return entry.sem.flags.contains(SemFlags::ROLE_AUTHORED);
    }
    matches!(
        entry.sem.role,
        SemRole::Link
            | SemRole::Heading
            | SemRole::Button
            | SemRole::Checkbox
            | SemRole::Radio
            | SemRole::Combobox
            | SemRole::TextBox
            | SemRole::SearchBox
            | SemRole::PasswordBox
            | SemRole::Slider
            | SemRole::SpinButton
            | SemRole::Img
            | SemRole::ProgressBar
            | SemRole::Switch
            | SemRole::Tab
            | SemRole::TreeItem
            | SemRole::MenuItemCheckbox
            | SemRole::MenuItemRadio
    )
}

/// A form control spoken on its own (quick navigation, focus follow): its accessible name first.
pub fn is_named_control(entry: &SemEntry) -> bool {
    matches!(
        entry.sem.role,
        SemRole::TextBox
            | SemRole::SearchBox
            | SemRole::PasswordBox
            | SemRole::Combobox
            | SemRole::Checkbox
            | SemRole::Radio
            | SemRole::Slider
            | SemRole::SpinButton
            | SemRole::Switch
            | SemRole::MenuItemCheckbox
            | SemRole::MenuItemRadio
    ) || (entry.sem.role == SemRole::Button && entry.sem.press != Press::NotToggle)
}

/// Roles whose accessible name the author may replace: the name is spoken instead of the content
/// text (NVDA's `nameIsContent`: link, button, toggle button, menu item, heading, page tab).
pub fn takes_authored_name(entry: &SemEntry) -> bool {
    entry.sem.name_from == NameFrom::Author
        && matches!(
            entry.sem.role,
            SemRole::Link
                | SemRole::Button
                | SemRole::Tab
                | SemRole::Heading
                | SemRole::MenuItem
                | SemRole::MenuItemCheckbox
                | SemRole::MenuItemRadio
        )
}

/// The role phrase: the ARIA word, "visited link", "heading level 2", "graphic", else the coarse
/// word of the role.
pub fn role_phrase(entry: &SemEntry) -> Option<Field> {
    let sem = &entry.sem;
    let role = sem.role;
    if role != SemRole::Heading
        && let Some(w) = aria_word(role)
    {
        return Some(word(FieldKind::ROLE, w, Group::Role));
    }
    Some(match role {
        SemRole::Link if sem.flags.contains(SemFlags::VISITED) => {
            word(FieldKind::ROLE, M::WebVisitedLink, Group::Role)
        }
        SemRole::Heading if (1..=6).contains(&sem.level) => Field::new(
            FieldKind::ROLE,
            Content::msg(
                M::WebHeadingLevel,
                vec![Arg::num("n", i64::from(sem.level))],
            ),
            origin(Group::Level),
        ),
        SemRole::Img => word(FieldKind::ROLE, M::WebGraphic, Group::Role),
        _ => word(
            FieldKind::ROLE,
            native_role(role, sem.press).word?,
            Group::Role,
        ),
    })
}

fn current_field(entry: &SemEntry) -> Option<Field> {
    let w = match entry.sem.current {
        Current::No => return None,
        Current::True => M::StateCurrent,
        Current::Page => M::StateCurrentPage,
        Current::Step => M::StateCurrentStep,
        Current::Location => M::StateCurrentLocation,
        Current::Date => M::StateCurrentDate,
        Current::Time => M::StateCurrentTime,
    };
    Some(word(FieldKind::STATE, w, Group::Current))
}

fn level_field(entry: &SemEntry) -> Option<Field> {
    (entry.sem.role == SemRole::TreeItem && entry.sem.level > 0).then(|| {
        Field::new(
            FieldKind::LEVEL,
            Content::msg(
                M::StateLevel,
                vec![Arg::num("l", i64::from(entry.sem.level))],
            ),
            origin(Group::Level),
        )
    })
}

fn error_field(entry: &SemEntry) -> Option<Field> {
    if entry.sem.invalid == Invalid::No {
        return None;
    }
    let e = entry
        .sem
        .text
        .as_deref()
        .and_then(|t| t.error_text.as_ref())
        .filter(|t| !t.trim().is_empty())?;
    Some(text_field(FieldKind::ERROR_TEXT, e, Group::Invalid))
}

/// What entering a control or link inside a line says: the role phrase, then the states. A link
/// or a heading says only whether it is expanded and which one is current; any other control
/// carries a control's states, its level and its error text.
pub fn enter_fields(entry: &SemEntry) -> Fields {
    let mut out = Fields::new();
    out.extend(role_phrase(entry));
    if matches!(entry.sem.role, SemRole::Link | SemRole::Heading) {
        match entry.sem.expand {
            Expand::Expanded => out.push(word(FieldKind::STATE, M::StateExpanded, Group::Expand)),
            Expand::Collapsed => {
                out.push(word(FieldKind::STATE, M::StateCollapsed, Group::Expand));
            }
            Expand::NotExpandable => {}
        }
        out.extend(current_field(entry));
    } else {
        push_states(entry, &mut out);
        out.extend(error_field(entry));
    }
    out
}

fn push_states(entry: &SemEntry, out: &mut Fields) {
    web_state_fields(entry, false, out);
    // `web_state_fields` ends with the late words; NVDA's order puts the current word after them.
    out.extend(current_field(entry));
    out.extend(level_field(entry));
}

/// A control with no text of its own: its name where the name is all there is, its role, states,
/// "blank" for an empty edit, "unlabeled" for a graphic without a name, and what is wrong with an
/// invalid field.
pub fn marker_fields(entry: &SemEntry) -> Fields {
    let sem = &entry.sem;
    let role = sem.role;
    let mut out = Fields::new();
    let named = matches!(
        role,
        SemRole::Img | SemRole::Button | SemRole::ProgressBar | SemRole::Slider
    ) || (matches!(role, SemRole::Checkbox | SemRole::Radio)
        && sem.name_from == NameFrom::Author)
        || role == SemRole::Switch;
    let name = entry.name.as_ref().filter(|n| !n.is_empty());
    if named && let Some(n) = name {
        out.push(text_field(FieldKind::NAME, n, Group::Role));
    }
    out.extend(role_phrase(entry));
    push_states(entry, &mut out);
    if matches!(
        role,
        SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox
    ) {
        out.push(word(FieldKind::STATE, M::WebBlank, Group::Role));
    }
    out.extend(error_field(entry));
    out
}

/// The word an unnamed graphic adds to its line: it leads the whole description (the position
/// it has always had), not just the graphic's own words.
pub fn unlabeled_graphic(entry: &SemEntry) -> Option<Field> {
    (entry.sem.role == SemRole::Img && entry.name.as_ref().is_none_or(|n| n.is_empty()))
        .then(|| word(FieldKind::STATE, M::WebUnlabeled, Group::Role))
}

/// Structural facts an entry cannot hold.
#[derive(Copy, Clone, Debug, Default)]
pub struct ContainerFacts {
    /// `ListItem` children of a list.
    pub list_items: usize,
    /// Rows and columns of a table.
    pub rows: usize,
    pub cols: usize,
}

fn landmark_word(role: SemRole) -> Option<M> {
    Some(match role {
        SemRole::Banner => M::WebLandmarkBanner,
        SemRole::Main => M::WebLandmarkMain,
        SemRole::Navigation => M::WebLandmarkNavigation,
        SemRole::Search => M::WebLandmarkSearch,
        SemRole::Complementary => M::WebLandmarkComplementary,
        SemRole::ContentInfo => M::WebLandmarkContentInfo,
        SemRole::Form => M::WebLandmarkForm,
        SemRole::Region => M::WebLandmarkRegion,
        _ => return None,
    })
}

/// What entering a container says: lists and tables with their sizes, block quotes, landmarks and
/// regions with their names, dialogs, embedded objects and the ARIA containers. `None` for a node
/// that is not announced as a container.
pub fn container_fields(entry: &SemEntry, facts: ContainerFacts) -> Option<Fields> {
    let sem = &entry.sem;
    let name = entry.name.as_ref().filter(|n| !n.is_empty());
    let authored = sem.flags.contains(SemFlags::ROLE_AUTHORED);
    // The declared size (`aria-rowcount`, `aria-colcount`) wins over the counted one.
    let declared = |n: u32, counted: usize| if n > 0 { n as usize } else { counted };
    let (rows, cols) = match sem.grid {
        Some(g) => (declared(g.row, facts.rows), declared(g.col, facts.cols)),
        None => (facts.rows, facts.cols),
    };
    let mut out = Fields::new();
    let named = |out: &mut Fields| {
        if let Some(n) = name {
            out.push(text_field(FieldKind::NAME, n, Group::Role));
        }
    };
    match sem.role {
        SemRole::List | SemRole::Listbox => out.push(Field::new(
            FieldKind::ROLE,
            Content::msg(
                M::WebListWith,
                vec![Arg::num(
                    "n",
                    i64::try_from(facts.list_items).unwrap_or(i64::MAX),
                )],
            ),
            origin(Group::Role),
        )),
        SemRole::Table | SemRole::Grid | SemRole::TreeGrid => {
            named(&mut out);
            out.push(Field::new(
                FieldKind::ROLE,
                Content::msg(
                    M::WebTableWith,
                    vec![
                        Arg::num("c", i64::try_from(cols).unwrap_or(i64::MAX)),
                        Arg::num("r", i64::try_from(rows).unwrap_or(i64::MAX)),
                    ],
                ),
                origin(Group::Role),
            ));
        }
        // Only the element: the author's `role="blockquote"` is not announced.
        SemRole::Blockquote if !authored => {
            out.push(word(FieldKind::ROLE, M::WebBlockQuote, Group::Role));
        }
        SemRole::Banner
        | SemRole::Main
        | SemRole::Navigation
        | SemRole::Search
        | SemRole::Complementary
        | SemRole::ContentInfo
        | SemRole::Form
        | SemRole::Region => {
            // A form is a landmark only when it has a name.
            if sem.role == SemRole::Form && name.is_none() {
                return None;
            }
            let kind = landmark_word(sem.role)?;
            named(&mut out);
            if matches!(sem.role, SemRole::Region | SemRole::Form) {
                out.push(word(FieldKind::ROLE, kind, Group::Role));
            } else {
                out.push(Field::new(
                    FieldKind::ROLE,
                    Content::msg(M::WebLandmark, vec![Arg::text("kind", gorca_i18n::t(kind))]),
                    origin(Group::Role),
                ));
            }
        }
        SemRole::Dialog | SemRole::AlertDialog => {
            named(&mut out);
            out.push(word(FieldKind::ROLE, M::RoleDialog, Group::Role));
        }
        SemRole::Application => {
            named(&mut out);
            // `role="application"` is an application; an `<object>` or `<embed>` is an embedded
            // object (both are the engine's embedded role).
            out.push(word(
                FieldKind::ROLE,
                if authored {
                    M::RoleApplication
                } else {
                    M::WebEmbeddedObject
                },
                Group::Role,
            ));
        }
        SemRole::TabList
        | SemRole::TabPanel
        | SemRole::Tree
        | SemRole::Menu
        | SemRole::Menubar
        | SemRole::Toolbar
        | SemRole::Group
        | SemRole::RadioGroup => {
            // The containers of the author's ARIA roles; a `<fieldset>` or a native tab or menu
            // is not announced as one.
            if !authored {
                return None;
            }
            if name.is_none() && matches!(sem.role, SemRole::Group | SemRole::TabPanel) {
                return None;
            }
            named(&mut out);
            if let Some(w) = aria_word(sem.role) {
                out.push(word(FieldKind::ROLE, w, Group::Role));
            }
        }
        _ => return None,
    }
    Some(out)
}

/// The word of "out of <container>" for a container the cursor leaves.
pub fn leaving_word(entry: &SemEntry) -> Option<M> {
    match entry.sem.role {
        SemRole::Table | SemRole::Grid | SemRole::TreeGrid => Some(M::WebOutOfTable),
        SemRole::List | SemRole::Listbox => Some(M::WebOutOfList),
        SemRole::Blockquote if !entry.sem.flags.contains(SemFlags::ROLE_AUTHORED) => {
            Some(M::WebOutOfBlockQuote)
        }
        _ => None,
    }
}

/// A role phrase field for a plain word (frames, articles, separators).
pub fn role_word_field(w: M) -> Field {
    word(FieldKind::ROLE, w, Group::Role)
}

/// A description worth saying: not empty and not a repeat of the name.
pub fn speakable_description(entry: &SemEntry) -> Option<Field> {
    let d = entry
        .sem
        .text
        .as_deref()
        .and_then(|t| t.description.as_ref())?;
    let d_trim = d.trim();
    if d_trim.is_empty() {
        return None;
    }
    if entry
        .name
        .as_deref()
        .is_some_and(|n| n.trim().eq_ignore_ascii_case(d_trim))
    {
        return None;
    }
    Some(Field::new(
        FieldKind::DESCRIPTION,
        Content::Text(Arc::from(d_trim)),
        origin(Group::Description),
    ))
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::{Check, GridPos, Press};

    use super::*;
    use crate::ir::{PresIr, Reason};
    use crate::render::{RenderOpts, render};

    fn entry(role: SemRole) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e
    }

    fn named(role: SemRole, name: &str) -> SemEntry {
        let mut e = entry(role);
        e.name = Some(Arc::from(name));
        e
    }

    fn say(fields: Fields) -> String {
        let key = gorca_model::NodeKey {
            app: gorca_model::AppId {
                connector: gorca_model::ConnectorId(0),
                serial: 0,
            },
            local: gorca_model::LocalId(0),
        };
        let ir = PresIr::new(Reason::QuickNav, key, 0, fields);
        render(&ir, RenderOpts::default())
            .iter()
            .map(|t| t.text.as_str().to_owned())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn links_headings_and_controls_are_announced_a_plain_menu_item_only_when_authored() {
        for role in [
            SemRole::Link,
            SemRole::Heading,
            SemRole::Button,
            SemRole::Checkbox,
            SemRole::Img,
            SemRole::Tab,
            SemRole::Switch,
        ] {
            assert!(announced(&entry(role)), "{role:?}");
        }
        assert!(!announced(&entry(SemRole::Paragraph)));
        let mut item = entry(SemRole::MenuItem);
        assert!(!announced(&item), "a select's option is its value");
        item.sem.flags |= SemFlags::ROLE_AUTHORED;
        assert!(announced(&item));
    }

    #[test]
    fn the_role_phrase_carries_level_visited_and_graphic() {
        let mut h = entry(SemRole::Heading);
        h.sem.level = 2;
        assert_eq!(say(enter_fields(&h)), "heading level 2");
        let mut l = entry(SemRole::Link);
        l.sem.flags |= SemFlags::VISITED;
        assert_eq!(say(enter_fields(&l)), "visited link");
        l.sem.expand = Expand::Expanded;
        assert_eq!(say(enter_fields(&l)), "visited link expanded");
        assert_eq!(say(enter_fields(&entry(SemRole::Img))), "graphic");
    }

    #[test]
    fn a_control_says_its_states_after_the_role() {
        let mut c = entry(SemRole::Checkbox);
        c.sem.check = Check::On;
        assert_eq!(say(enter_fields(&c)), "check box checked");
        let mut t = entry(SemRole::Button);
        t.sem.press = Press::Off;
        assert_eq!(say(enter_fields(&t)), "toggle button not pressed");
    }

    #[test]
    fn a_marker_names_what_has_no_text_and_an_edit_is_blank() {
        let sw = named(SemRole::Switch, "Aria switch");
        assert_eq!(say(marker_fields(&sw)), "Aria switch switch off");
        assert_eq!(say(marker_fields(&entry(SemRole::TextBox))), "edit blank");
        assert!(unlabeled_graphic(&entry(SemRole::Img)).is_some());
        assert!(unlabeled_graphic(&named(SemRole::Img, "logo")).is_none());
    }

    #[test]
    fn containers_say_what_they_are_and_how_big() {
        let facts = ContainerFacts {
            list_items: 3,
            rows: 2,
            cols: 4,
        };
        let say_c = |e: &SemEntry| container_fields(e, facts).map(say);
        assert_eq!(
            say_c(&entry(SemRole::List)).as_deref(),
            Some("list with 3 items")
        );
        assert_eq!(
            say_c(&named(SemRole::Table, "Fruit")).as_deref(),
            Some("Fruit table with 4 columns and 2 rows")
        );
        // The author's declared size wins over the counted one.
        let mut big = named(SemRole::Table, "Big");
        big.sem.grid = Some(GridPos {
            row: 100,
            col: 5,
            row_span: 1,
            col_span: 1,
        });
        assert_eq!(
            say_c(&big).as_deref(),
            Some("Big table with 5 columns and 100 rows")
        );
        assert_eq!(
            say_c(&entry(SemRole::Form)),
            None,
            "an unnamed form is no landmark"
        );
        assert_eq!(
            say_c(&named(SemRole::Form, "Profile")).as_deref(),
            Some("Profile form")
        );
        assert_eq!(
            say_c(&named(SemRole::Main, "Main content")).as_deref(),
            Some("Main content main landmark")
        );
        // A fieldset is no container of its own; the author's group is.
        let mut group = named(SemRole::Group, "Shipping");
        assert_eq!(say_c(&group), None);
        group.sem.flags |= SemFlags::ROLE_AUTHORED;
        assert_eq!(say_c(&group).as_deref(), Some("Shipping grouping"));
        // `role=application` is an application, an `<object>` an embedded object.
        let mut app = named(SemRole::Application, "Plain object");
        assert_eq!(say_c(&app).as_deref(), Some("Plain object embedded object"));
        app.sem.flags |= SemFlags::ROLE_AUTHORED;
        assert_eq!(say_c(&app).as_deref(), Some("Plain object application"));
    }

    #[test]
    fn an_invalid_field_says_what_is_wrong_with_it() {
        let mut e = entry(SemRole::TextBox);
        e.sem.invalid = Invalid::True;
        e.sem.text = Some(Arc::new(gorca_model::sem::SemText {
            error_text: Some(Arc::from("Enter a valid address")),
            ..gorca_model::sem::SemText::default()
        }));
        assert_eq!(
            say(marker_fields(&e)),
            "edit invalid entry blank Enter a valid address"
        );
    }

    #[test]
    fn an_authored_name_replaces_the_content_of_a_link_or_button() {
        let mut b = named(SemRole::Button, "Search");
        assert!(!takes_authored_name(&b));
        b.sem.name_from = NameFrom::Author;
        assert!(takes_authored_name(&b));
        assert!(!takes_authored_name(&named(SemRole::Paragraph, "x")));
    }
}
