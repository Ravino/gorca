// SPDX-License-Identifier: GPL-3.0-or-later
//! Object navigation (ADR-0006 5): presentation classes, the simple-review walks of NVDA
//! (`simpleParent`, `simpleFirstChild`, `simpleNext`, `simplePrevious`, `nextInFlow`) and the raw
//! tree moves, all over mirror reads.
//!
//! A walk is a pure function of a [`NavSource`]. When a node it needs is not in the mirror it
//! stops with [`Stop::Miss`] naming the one fetch that would help; the Reader sends it, and
//! re-runs the step from the start when the fetch has been applied to the mirror (a walk touches
//! at most 512 nodes, so a re-run is cheap and has no state to get wrong).

use std::sync::Arc;

use gorca_model::sem::{SemEntry, SemFlags, SemRole};
use gorca_model::{
    ChildList, Lookup, NodeKey, NodeSnapshot, NodeView, PropMask, StructFlags, StructState,
};

pub use crate::nodekind::is_app_root;
use crate::present::entry_of;

use super::model::ReviewLimits;

/// What object navigation needs to classify and speak a node (ADR-0006 3).
pub const NAV_MASK: PropMask = PropMask::PRESENT
    .union(PropMask::ATTRS)
    .union(PropMask::CHILDREN)
    .union(PropMask::PARENT);

/// What decides a miss: a node cannot be classified or walked without these (ADR-0006 5.3). A
/// snapshot partial or stale in DESCRIPTION, VALUE or STATES only is a hit tagged `may_refine`.
const CRITICAL: PropMask = PropMask::NAME.union(PropMask::ROLE);

/// Where a mirror read comes from. The Reader implements it over the directory; tests over a map.
pub trait NavSource {
    fn view(&self, k: NodeKey) -> Lookup<NodeView>;
}

/// Presentation class of a node in simple review (NVDA `presentationType`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresClass {
    /// Not showing or defunct: skipped with its subtree.
    Unavailable,
    /// Layout only: never a stop, its children are spliced into the parent.
    Layout,
    Content,
}

fn blank(s: Option<&Arc<str>>) -> bool {
    s.is_none_or(|s| s.trim().is_empty())
}

/// The class of a node (ADR-0006 5.1) from its fused entry and its structural state (M22b3): the
/// name and the description the user would hear, the model role, and whether the node is on the
/// screen. The structural bits are the only thing read of the node's state set; nothing spoken is.
///
/// `web` is a node inside a web document: Gecko sets `SHOWING` on some of its nodes only (links
/// and controls, not containers, landmarks or text), so `VISIBLE` alone counts there; what is off
/// screen is clipped by its extents.
pub fn class_entry(entry: &SemEntry, st: StructState, web: bool) -> PresClass {
    let showing = !st.is_known()
        || (st.contains(StructFlags::SHOWING) && st.contains(StructFlags::VISIBLE))
        // Some toolkits set only one of the pair.
        || st.contains(StructFlags::SHOWING)
        || (web && st.contains(StructFlags::VISIBLE));
    if st.is_defunct() || !showing {
        return PresClass::Unavailable;
    }
    let description = entry
        .sem
        .text
        .as_deref()
        .and_then(|t| t.description.as_ref());
    let nameless = blank(entry.name.as_ref()) && blank(description);
    // A label is said with its control or in the dialog's text; static text is a stop.
    if entry.sem.role == SemRole::StaticText && entry.sem.flags.contains(SemFlags::TEXT_LABEL) {
        return PresClass::Layout;
    }
    match role_class(entry.sem.role) {
        RoleClass::Layout => PresClass::Layout,
        RoleClass::LayoutIfNameless if nameless => PresClass::Layout,
        RoleClass::LayoutIfNameless | RoleClass::Content => PresClass::Content,
    }
}

/// [`class_entry`] of a snapshot (the entry the snapshot carries, or its provisional one).
pub fn class_of_snapshot(snap: &NodeSnapshot, web: bool) -> PresClass {
    class_entry(&entry_of(snap), snap.struct_state(), web)
}

/// What a model role is to object navigation.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RoleClass {
    /// Layout only: never a stop, its children are spliced into the parent.
    Layout,
    /// A stop when it has a name or a description, layout otherwise (a window, a landmark).
    LayoutIfNameless,
    Content,
}

/// The navigation class of a model role. One exhaustive `match`: a new model role cannot be added
/// without deciding what object navigation does with it.
pub const fn role_class(role: SemRole) -> RoleClass {
    match role {
        SemRole::Unknown
        | SemRole::Generic
        | SemRole::Group
        | SemRole::Presentation
        | SemRole::Paragraph
        | SemRole::Caption
        | SemRole::Separator
        // Roles the object model has no stop for (inline text roles, structure the engine adds).
        | SemRole::Blockquote
        | SemRole::Figure
        | SemRole::Log
        | SemRole::Marquee
        | SemRole::Math
        | SemRole::Note
        | SemRole::RadioGroup
        | SemRole::RowGroup
        | SemRole::TabPanel
        | SemRole::Timer
        | SemRole::Comment
        | SemRole::Suggestion
        | SemRole::Details
        | SemRole::Summary
        | SemRole::Iframe => RoleClass::Layout,
        SemRole::Window
        | SemRole::Form
        | SemRole::Region
        | SemRole::Banner
        | SemRole::Complementary
        | SemRole::ContentInfo
        | SemRole::Main
        | SemRole::Navigation
        | SemRole::Search
        | SemRole::Article => RoleClass::LayoutIfNameless,
        SemRole::Alert
        | SemRole::AlertDialog
        | SemRole::Application
        | SemRole::Button
        | SemRole::Cell
        | SemRole::Checkbox
        | SemRole::ColumnHeader
        | SemRole::Combobox
        | SemRole::Definition
        | SemRole::Dialog
        | SemRole::Document
        | SemRole::DocumentWeb
        | SemRole::Feed
        | SemRole::Grid
        | SemRole::GridCell
        | SemRole::Heading
        | SemRole::Img
        | SemRole::Link
        | SemRole::List
        | SemRole::Listbox
        | SemRole::ListItem
        | SemRole::Menu
        | SemRole::Menubar
        | SemRole::MenuItem
        | SemRole::MenuItemCheckbox
        | SemRole::MenuItemRadio
        | SemRole::Meter
        | SemRole::Option
        | SemRole::ProgressBar
        | SemRole::Radio
        | SemRole::Row
        | SemRole::RowHeader
        | SemRole::Scrollbar
        | SemRole::SearchBox
        | SemRole::Slider
        | SemRole::SpinButton
        | SemRole::Status
        | SemRole::Switch
        | SemRole::Tab
        | SemRole::Table
        | SemRole::TabList
        | SemRole::Term
        | SemRole::TextBox
        | SemRole::PasswordBox
        | SemRole::Toolbar
        | SemRole::Tooltip
        | SemRole::Tree
        | SemRole::TreeGrid
        | SemRole::TreeItem
        | SemRole::Terminal
        | SemRole::Popover
        | SemRole::DescriptionList
        // Text runs: static text (the model has one role for the engine's label and static text)
        // and the inline phrase roles.
        | SemRole::StaticText
        | SemRole::Emphasis
        | SemRole::Strong
        | SemRole::Mark
        | SemRole::Code
        | SemRole::Time
        | SemRole::Insertion
        | SemRole::Deletion
        | SemRole::Subscript
        | SemRole::Superscript => RoleClass::Content,
    }
}

/// One object command (the seven of ADR-0006 5.2, minus `current`, which moves nothing).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NavMove {
    Parent,
    FirstChild,
    Next,
    Prev,
    NextInFlow,
    PrevInFlow,
}

impl NavMove {
    /// What the user hears when the move has nowhere to go (NVDA's wording).
    pub fn empty_text(self) -> &'static str {
        match self {
            NavMove::Parent => gorca_i18n::t(gorca_i18n::MsgId::NavNoParent),
            NavMove::FirstChild => gorca_i18n::t(gorca_i18n::MsgId::NavNoChild),
            NavMove::Next | NavMove::NextInFlow => gorca_i18n::t(gorca_i18n::MsgId::NavNoNext),
            NavMove::Prev | NavMove::PrevInFlow => gorca_i18n::t(gorca_i18n::MsgId::NavNoPrev),
        }
    }
}

/// The one fetch that would let a walk go on (ADR-0006 5.3, `NavOp`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NavOp {
    /// The children of `node`, with their snapshots, and `node` itself with a `Known` list.
    Children { node: NodeKey },
    /// `node` and its ancestors.
    Ancestry { node: NodeKey },
    /// `node` alone.
    Node { node: NodeKey },
}

impl NavOp {
    pub fn node(self) -> NodeKey {
        match self {
            NavOp::Children { node } | NavOp::Ancestry { node } | NavOp::Node { node } => node,
        }
    }
}

/// Why a walk stopped without an answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Stop {
    Miss(NavOp),
    /// The visit cap (OB-1) was hit; the best `Content` node seen so far, if any.
    Cap(Option<NodeKey>),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StepResult {
    Found(NodeKey),
    Empty,
    Stop(Stop),
}

/// A node plus whether it was served from stale or partial data.
#[derive(Clone, Debug)]
struct Read {
    snap: Arc<NodeSnapshot>,
}

struct Walk<'a, S: NavSource> {
    src: &'a S,
    visits: usize,
    limits: ReviewLimits,
    /// The last `Content` node seen, for the cap fallback.
    last_content: Option<NodeKey>,
    simple: bool,
    /// Degraded mode: a node the mirror cannot give is skipped instead of fetched.
    lenient: bool,
}

/// `Ok(Some)` = a node, `Ok(None)` = none, `Err(Stop)` = need data.
type R<T> = Result<T, Stop>;

impl<'a, S: NavSource> Walk<'a, S> {
    fn new(src: &'a S, simple: bool, lenient: bool, limits: ReviewLimits) -> Self {
        Self {
            src,
            visits: 0,
            limits,
            last_content: None,
            simple,
            lenient,
        }
    }

    fn tick(&mut self) -> R<()> {
        self.visits += 1;
        if self.visits > self.limits.nav_visits {
            Err(Stop::Cap(self.last_content))
        } else {
            Ok(())
        }
    }

    /// Read a node. `Ok(None)` for a dead node, which the caller skips.
    fn read(&mut self, k: NodeKey) -> R<Option<Read>> {
        self.tick()?;
        match self.src.view(k) {
            Lookup::Gone => Ok(None),
            Lookup::Miss => self.miss(NavOp::Node { node: k }),
            Lookup::Hit(v) => {
                let critical_missing =
                    !v.snap.have().contains(CRITICAL) || v.stale.intersects(CRITICAL);
                if critical_missing {
                    return self.miss(NavOp::Node { node: k });
                }
                if class_of_snapshot(&v.snap, false) == PresClass::Content {
                    self.last_content = Some(k);
                }
                Ok(Some(Read { snap: v.snap }))
            }
        }
    }

    fn miss<T>(&self, op: NavOp) -> R<Option<T>> {
        if self.lenient {
            Ok(None)
        } else {
            Err(Stop::Miss(op))
        }
    }

    /// The known children of `r`, or the miss that learns them.
    fn children(&mut self, r: &Read) -> R<Vec<NodeKey>> {
        let stale_children = match self.src.view(r.snap.key()) {
            Lookup::Hit(v) => v.stale.contains(PropMask::CHILDREN),
            Lookup::Miss | Lookup::Gone => false,
        };
        match &r.snap.children() {
            ChildList::Known(list) if !stale_children => Ok(list.to_vec()),
            ChildList::Count(0) if !stale_children => Ok(Vec::new()),
            ChildList::Known(_) | ChildList::Count(_) | ChildList::Unknown => {
                if self.lenient {
                    // A stale list is better than none: it is only a degraded step.
                    match &r.snap.children() {
                        ChildList::Known(list) => Ok(list.to_vec()),
                        ChildList::Count(_) | ChildList::Unknown => Ok(Vec::new()),
                    }
                } else {
                    Err(Stop::Miss(NavOp::Children { node: r.snap.key() }))
                }
            }
        }
    }

    fn parent(&mut self, r: &Read) -> R<Option<NodeKey>> {
        // The application root of the accessibility tree, not an ARIA `application` widget.
        if is_app_root(&r.snap) {
            // Never above the application root (ADR-0006 5.2).
            return Ok(None);
        }
        match r.snap.parent() {
            Some(p) if r.snap.have().contains(PropMask::PARENT) || self.lenient => Ok(Some(p)),
            Some(_) | None => {
                if r.snap.have().contains(PropMask::PARENT) || self.lenient {
                    Ok(None)
                } else {
                    Err(Stop::Miss(NavOp::Ancestry { node: r.snap.key() }))
                }
            }
        }
    }

    fn class_of(&self, r: &Read) -> PresClass {
        if self.simple {
            class_of_snapshot(&r.snap, false)
        } else if r.snap.struct_state().is_defunct() {
            PresClass::Unavailable
        } else {
            PresClass::Content
        }
    }

    // ---- simple review (NVDA) -------------------------------------------------------------

    /// First `Content` node in pre-order under `r`, descending through `Layout` only.
    fn first_content_under(&mut self, r: &Read, depth: usize) -> R<Option<NodeKey>> {
        if depth > self.limits.nav_depth {
            return Ok(None);
        }
        for c in self.children(r)? {
            let Some(cr) = self.read(c)? else { continue };
            match self.class_of(&cr) {
                PresClass::Content => return Ok(Some(c)),
                PresClass::Layout => {
                    if let Some(found) = self.first_content_under(&cr, depth + 1)? {
                        return Ok(Some(found));
                    }
                }
                PresClass::Unavailable => {}
            }
        }
        Ok(None)
    }

    /// Last `Content` node under `r` along the last-child spine, descending through `Layout`.
    fn last_content_under(&mut self, r: &Read, depth: usize) -> R<Option<NodeKey>> {
        if depth > self.limits.nav_depth {
            return Ok(None);
        }
        let kids = self.children(r)?;
        for c in kids.into_iter().rev() {
            let Some(cr) = self.read(c)? else { continue };
            match self.class_of(&cr) {
                PresClass::Content => return Ok(Some(c)),
                PresClass::Layout => {
                    if let Some(found) = self.last_content_under(&cr, depth + 1)? {
                        return Ok(Some(found));
                    }
                }
                PresClass::Unavailable => {}
            }
        }
        Ok(None)
    }

    fn simple_parent(&mut self, node: NodeKey) -> R<Option<NodeKey>> {
        let Some(mut cur) = self.read(node)? else {
            return Ok(None);
        };
        for _ in 0..self.limits.nav_depth * 4 {
            let Some(p) = self.parent(&cur)? else {
                return Ok(None);
            };
            let Some(pr) = self.read(p)? else {
                return Ok(None);
            };
            if !self.simple || self.class_of(&pr) == PresClass::Content {
                return Ok(Some(p));
            }
            cur = pr;
        }
        Ok(None)
    }

    fn simple_first_child(&mut self, node: NodeKey) -> R<Option<NodeKey>> {
        let Some(r) = self.read(node)? else {
            return Ok(None);
        };
        if self.simple {
            self.first_content_under(&r, 0)
        } else {
            for c in self.children(&r)? {
                if self.read(c)?.is_some() {
                    return Ok(Some(c));
                }
            }
            Ok(None)
        }
    }

    /// Siblings of `node` after (`forward`) or before it, with the parent's read.
    fn siblings(&mut self, node: NodeKey, forward: bool) -> R<Option<(Read, Vec<NodeKey>)>> {
        let Some(r) = self.read(node)? else {
            return Ok(None);
        };
        let Some(p) = self.parent(&r)? else {
            return Ok(None);
        };
        let Some(pr) = self.read(p)? else {
            return Ok(None);
        };
        let kids = self.children(&pr)?;
        let Some(i) = kids.iter().position(|k| *k == node) else {
            // The parent's list does not contain the node: stale; refresh it.
            return if self.lenient {
                Ok(None)
            } else {
                Err(Stop::Miss(NavOp::Children { node: p }))
            };
        };
        let rest: Vec<NodeKey> = if forward {
            kids[i + 1..].to_vec()
        } else {
            kids[..i].iter().rev().copied().collect()
        };
        Ok(Some((pr, rest)))
    }

    fn simple_sibling(&mut self, node: NodeKey, forward: bool) -> R<Option<NodeKey>> {
        let mut cur = node;
        for _ in 0..self.limits.nav_depth * 4 {
            let Some((pr, rest)) = self.siblings(cur, forward)? else {
                return Ok(None);
            };
            for s in rest {
                let Some(sr) = self.read(s)? else { continue };
                match self.class_of(&sr) {
                    PresClass::Content => return Ok(Some(s)),
                    PresClass::Layout => {
                        let inner = if forward {
                            self.first_content_under(&sr, 0)?
                        } else {
                            self.last_content_under(&sr, 0)?
                        };
                        if let Some(found) = inner {
                            return Ok(Some(found));
                        }
                    }
                    PresClass::Unavailable => {}
                }
            }
            // Climb through a layout parent; a content parent ends the search.
            if self.simple && self.class_of(&pr) == PresClass::Layout {
                cur = pr.snap.key();
            } else {
                return Ok(None);
            }
        }
        Ok(None)
    }

    fn deepest_last(&mut self, node: NodeKey) -> R<NodeKey> {
        let mut cur = node;
        for _ in 0..self.limits.nav_depth * 4 {
            let Some(r) = self.read(cur)? else { break };
            let kids = self.children(&r)?;
            let mut next = None;
            for c in kids.into_iter().rev() {
                let Some(cr) = self.read(c)? else { continue };
                if self.class_of(&cr) == PresClass::Content {
                    next = Some(c);
                    break;
                }
                if self.simple
                    && self.class_of(&cr) == PresClass::Layout
                    && let Some(found) = self.last_content_under(&cr, 0)?
                {
                    next = Some(found);
                    break;
                }
            }
            match next {
                Some(n) => cur = n,
                None => break,
            }
        }
        Ok(cur)
    }

    fn next_in_flow(&mut self, node: NodeKey) -> R<Option<NodeKey>> {
        if let Some(c) = self.simple_first_child(node)? {
            return Ok(Some(c));
        }
        let mut cur = node;
        for _ in 0..self.limits.nav_depth * 4 {
            if let Some(s) = self.simple_sibling(cur, true)? {
                return Ok(Some(s));
            }
            match self.simple_parent(cur)? {
                Some(p) => cur = p,
                None => return Ok(None),
            }
        }
        Ok(None)
    }

    fn prev_in_flow(&mut self, node: NodeKey) -> R<Option<NodeKey>> {
        if let Some(p) = self.simple_sibling(node, false)? {
            return Ok(Some(self.deepest_last(p)?));
        }
        self.simple_parent(node)
    }
}

/// Run one object move from `nav`. `simple` selects NVDA's simple review (the default).
pub fn step<S: NavSource>(
    src: &S,
    nav: NodeKey,
    mv: NavMove,
    simple: bool,
    limits: ReviewLimits,
) -> StepResult {
    run(&mut Walk::new(src, simple, false, limits), nav, mv)
}

/// The degraded move of OB-4: a raw move over whatever the mirror holds, never fetching.
pub fn step_degraded<S: NavSource>(
    src: &S,
    nav: NodeKey,
    mv: NavMove,
    limits: ReviewLimits,
) -> StepResult {
    run(&mut Walk::new(src, false, true, limits), nav, mv)
}

fn run<S: NavSource>(w: &mut Walk<'_, S>, nav: NodeKey, mv: NavMove) -> StepResult {
    let out = match mv {
        NavMove::Parent => w.simple_parent(nav),
        NavMove::FirstChild => w.simple_first_child(nav),
        NavMove::Next => w.simple_sibling(nav, true),
        NavMove::Prev => w.simple_sibling(nav, false),
        NavMove::NextInFlow => w.next_in_flow(nav),
        NavMove::PrevInFlow => w.prev_in_flow(nav),
    };
    match out {
        Ok(Some(k)) => StepResult::Found(k),
        Err(Stop::Cap(Some(k))) if k != nav => StepResult::Found(k),
        Ok(None) | Err(Stop::Cap(_)) => StepResult::Empty,
        Err(s @ Stop::Miss(_)) => StepResult::Stop(s),
    }
}

#[cfg(test)]
mod entry_tests {
    use gorca_model::Role;
    use gorca_model::sem::{SemText, Semantics};

    use super::*;

    fn entry(role: SemRole, name: &str) -> SemEntry {
        SemEntry {
            sem: Semantics {
                role,
                ..Semantics::default()
            },
            name: (!name.is_empty()).then(|| Arc::from(name)),
            ..SemEntry::default()
        }
    }

    fn st(f: StructFlags) -> StructState {
        StructState::known(f)
    }

    const ON: StructFlags = StructFlags::SHOWING.union(StructFlags::VISIBLE);

    #[test]
    fn the_class_is_a_function_of_the_entry_and_the_structural_state() {
        let button = entry(SemRole::Button, "OK");
        assert_eq!(class_entry(&button, st(ON), false), PresClass::Content);
        assert_eq!(
            class_entry(&button, st(StructFlags::DEFUNCT | ON), false),
            PresClass::Unavailable
        );
        assert_eq!(
            class_entry(&button, st(StructFlags::empty()), false),
            PresClass::Unavailable,
            "states were read and nothing shows"
        );
        assert_eq!(
            class_entry(&button, StructState::UNKNOWN, false),
            PresClass::Content,
            "states never fetched: present"
        );
    }

    #[test]
    fn showing_alone_counts_and_visible_alone_counts_in_a_web_document_only() {
        let button = entry(SemRole::Button, "OK");
        assert_eq!(
            class_entry(&button, st(StructFlags::SHOWING), false),
            PresClass::Content
        );
        assert_eq!(
            class_entry(&button, st(StructFlags::VISIBLE), false),
            PresClass::Unavailable
        );
        assert_eq!(
            class_entry(&button, st(StructFlags::VISIBLE), true),
            PresClass::Content
        );
    }

    #[test]
    fn a_window_or_landmark_is_a_stop_only_with_a_name_or_a_description() {
        for role in [
            SemRole::Window,
            SemRole::Main,
            SemRole::Form,
            SemRole::Region,
        ] {
            assert_eq!(
                class_entry(&entry(role, ""), st(ON), false),
                PresClass::Layout,
                "{role:?}"
            );
            assert_eq!(
                class_entry(&entry(role, "  "), st(ON), false),
                PresClass::Layout,
                "{role:?} with a blank name"
            );
            assert_eq!(
                class_entry(&entry(role, "Title"), st(ON), false),
                PresClass::Content,
                "{role:?}"
            );
            let mut described = entry(role, "");
            described.sem.text = Some(Arc::new(SemText {
                description: Some(Arc::from("tip")),
                ..SemText::default()
            }));
            assert_eq!(
                class_entry(&described, st(ON), false),
                PresClass::Content,
                "{role:?} with a description"
            );
        }
    }

    #[test]
    fn layout_roles_are_never_stops_and_controls_always_are() {
        for role in [
            SemRole::Generic,
            SemRole::Group,
            SemRole::Paragraph,
            SemRole::Separator,
            SemRole::Caption,
            SemRole::Unknown,
        ] {
            assert_eq!(
                class_entry(&entry(role, "named"), st(ON), false),
                PresClass::Layout,
                "{role:?}"
            );
        }
        for role in [
            SemRole::Button,
            SemRole::Link,
            SemRole::TextBox,
            SemRole::Slider,
        ] {
            assert_eq!(
                class_entry(&entry(role, ""), st(ON), false),
                PresClass::Content,
                "{role:?}"
            );
        }
    }

    #[test]
    fn static_text_is_a_stop_and_a_label_is_not() {
        let text = entry(SemRole::StaticText, "Hello");
        assert_eq!(class_entry(&text, st(ON), false), PresClass::Content);
        let mut label = text.clone();
        label.sem.flags |= SemFlags::TEXT_LABEL;
        assert_eq!(class_entry(&label, st(ON), false), PresClass::Layout);
        // The bit is a label's: a button that carries it by mistake is still a stop.
        let mut button = entry(SemRole::Button, "OK");
        button.sem.flags |= SemFlags::TEXT_LABEL;
        assert_eq!(class_entry(&button, st(ON), false), PresClass::Content);
    }

    #[test]
    fn every_model_role_has_a_class() {
        let mut layout = 0;
        let mut content = 0;
        for code in 0..=u16::MAX {
            let Some(role) = SemRole::from_code(code) else {
                continue;
            };
            match role_class(role) {
                RoleClass::Layout => layout += 1,
                RoleClass::LayoutIfNameless | RoleClass::Content => content += 1,
            }
        }
        assert!(
            layout > 10 && content > 50,
            "{layout} layout, {content} content"
        );
    }

    #[test]
    fn the_structural_state_reads_only_the_structural_bits() {
        use gorca_model::{
            AppId, ConnectorId, Freshness, LocalId, NodeSnapshot, PropMask, Role, StateSet,
        };
        let mut snap = NodeSnapshot::new(
            NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            Freshness::Defunct,
        )
        .with_name(Some(Arc::from("OK")))
        .with_have(PropMask::NAME | PropMask::ROLE)
        .with_source(Role::Button, StateSet::empty());
        assert!(!snap.struct_state().is_known(), "states were never fetched");
        // With the states read, the structural bits decide, and nothing spoken leaks in.
        snap.set_have(snap.have() | PropMask::STATES);
        snap.set_test_states(
            StateSet::SHOWING | StateSet::VISIBLE | StateSet::CHECKED | StateSet::DISABLED,
        );
        let s = snap.struct_state();
        assert!(s.is_known());
        assert_eq!(s.flags(), ON);
        snap.set_test_states(snap.test_states() | StateSet::DEFUNCT);
        assert!(snap.struct_state().is_defunct());
    }

    #[test]
    fn an_aria_application_widget_is_not_the_application_root() {
        use super::super::collect::testsrc::{key, snap};
        use super::super::replay::PageSource;
        let win = snap(1, Role::Window, "Doc", None, &[2]);
        // A web `role=application` widget: the author's role token says application, the
        // AT-SPI role does not, and the move to the parent must still go through it.
        let mut widget = snap(2, Role::Panel, "Board", Some(1), &[3]);
        let mut e = (*entry_of(&widget)).clone();
        e.sem.role = SemRole::Application;
        e.sem.flags |= SemFlags::ROLE_AUTHORED;
        widget.sem_mut().entry = Some(Arc::new(e));
        let leaf = snap(3, Role::Button, "Play", Some(2), &[]);
        let page = PageSource(
            [win, widget, leaf]
                .into_iter()
                .map(|s| (s.key(), Arc::new(s)))
                .collect(),
        );
        let up = step(
            &page,
            key(3),
            NavMove::Parent,
            true,
            ReviewLimits::default(),
        );
        assert_eq!(up, StepResult::Found(key(2)));
        let up = step(
            &page,
            key(2),
            NavMove::Parent,
            true,
            ReviewLimits::default(),
        );
        assert_eq!(
            up,
            StepResult::Found(key(1)),
            "not stopped as the tree root"
        );
    }

    #[test]
    fn only_a_parentless_application_node_is_the_tree_root() {
        use super::super::collect::testsrc::snap;
        let root = snap(1, Role::Application, "App", None, &[2]);
        assert!(is_app_root(&root));
        // An embedded object has the model role application and a parent inside the tree.
        let embedded = snap(2, Role::Application, "Frame", Some(1), &[]);
        assert!(!is_app_root(&embedded));
        // A snapshot that never carried its parent cannot be taken for the root.
        let mut unfetched = root.clone();
        unfetched.set_have(unfetched.have() - PropMask::PARENT);
        assert!(!is_app_root(&unfetched));
        assert!(!is_app_root(&snap(3, Role::Window, "W", None, &[])));
    }
}
