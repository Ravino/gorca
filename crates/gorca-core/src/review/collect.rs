// SPDX-License-Identifier: GPL-3.0-or-later
//! The surface walk of a model build (ADR-0006 6.1 steps 1 and 2, 6.2): read the mirror under
//! the surface root in pre-order, prune what is not showing and what is another surface, and
//! list what the mirror lacks. Pure over a [`NavSource`]; the build task fetches the misses and
//! walks again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gorca_model::sem::SemRole;
use gorca_model::{ChildList, Lookup, NodeKey, NodeSnapshot, PropMask, Rect, StructFlags};

use crate::present::entry_of;

use super::cluster::BuildNode;
use super::model::{ReviewLimits, TruncWhy};
use super::objnav::{NavSource, PresClass, class_of_snapshot};

/// What a build wants from the app, in the order the task asks for it.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Miss {
    /// Children (and the node itself with a `Known` list).
    Expand(NodeKey),
    /// One node, absent or too stale to classify.
    Node(NodeKey),
    /// One node that lacks extents.
    Bounds(NodeKey),
}

#[derive(Debug, Default)]
pub struct Collected {
    pub nodes: Vec<BuildNode>,
    pub misses: Vec<Miss>,
    /// Nodes whose visible text lines are wanted (in `nodes` order).
    pub text_nodes: Vec<NodeKey>,
    pub truncated: Option<TruncWhy>,
    /// The rectangle a web document's text lines are clipped to (its viewport), per text node.
    pub text_clips: HashMap<NodeKey, Rect>,
}

/// Where the walk is relative to a web document.
#[derive(Copy, Clone, Debug)]
enum Web {
    Outside,
    /// Inside a `DocumentWeb`, whose extents are known or not.
    Inside(Option<Rect>),
}

fn overlaps(a: Rect, b: Rect) -> bool {
    let (ax1, ay1) = (
        i64::from(a.x) + i64::from(a.w),
        i64::from(a.y) + i64::from(a.h),
    );
    let (bx1, by1) = (
        i64::from(b.x) + i64::from(b.w),
        i64::from(b.y) + i64::from(b.h),
    );
    i64::from(a.x) < bx1 && i64::from(b.x) < ax1 && i64::from(a.y) < by1 && i64::from(b.y) < ay1
}

/// Nodes the app can hang text lines on. A snapshot from a focus fetch or a child fetch does not
/// say which interfaces the node has (`NodeCaps::is_known` is false), so for those the
/// text-bearing roles are asked and a refusal is the answer; a node known to have no `Text` is not
/// asked.
pub fn wants_lines(s: &NodeSnapshot) -> bool {
    let caps = s.caps();
    if caps.is_known() && !caps.has_text() {
        return false;
    }
    match text_bearing(entry_of(s).sem.role) {
        TextBearing::Yes => true,
        // Only on an explicit claim: every filler and container has these roles.
        TextBearing::IfClaimed => caps.is_known(),
        TextBearing::No => false,
    }
}

/// Whether a model role carries text lines of its own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TextBearing {
    Yes,
    IfClaimed,
    No,
}

/// One exhaustive `match`: a new model role cannot be added without deciding whether review asks
/// for its lines. The roles of the web that the engines export as fillers, landmarks or sections
/// (`IfClaimed`) are asked on a text claim only.
fn text_bearing(role: SemRole) -> TextBearing {
    match role {
        SemRole::StaticText
        | SemRole::Caption
        | SemRole::Paragraph
        | SemRole::Heading
        | SemRole::Link
        | SemRole::TextBox
        | SemRole::SearchBox
        | SemRole::PasswordBox
        | SemRole::Document
        | SemRole::DocumentWeb
        | SemRole::Terminal
        | SemRole::ListItem
        | SemRole::Cell
        | SemRole::GridCell
        | SemRole::Option
        | SemRole::Definition
        | SemRole::Alert
        // The inline phrase roles are the engine's static text with an author's tag.
        | SemRole::Emphasis
        | SemRole::Strong
        | SemRole::Mark
        | SemRole::Code
        | SemRole::Time
        | SemRole::Insertion
        | SemRole::Deletion
        | SemRole::Subscript
        | SemRole::Superscript => TextBearing::Yes,
        SemRole::Application
        | SemRole::Window
        | SemRole::Dialog
        | SemRole::AlertDialog
        | SemRole::Img
        | SemRole::List
        | SemRole::Listbox
        | SemRole::Table
        | SemRole::Row
        | SemRole::ColumnHeader
        | SemRole::RowHeader
        | SemRole::Button
        | SemRole::Switch
        | SemRole::Checkbox
        | SemRole::Radio
        | SemRole::Combobox
        | SemRole::SpinButton
        | SemRole::Slider
        | SemRole::ProgressBar
        | SemRole::Menu
        | SemRole::Menubar
        | SemRole::MenuItem
        | SemRole::MenuItemCheckbox
        | SemRole::MenuItemRadio
        | SemRole::Popover
        | SemRole::Tab
        | SemRole::TabList
        | SemRole::Tree
        | SemRole::TreeItem
        | SemRole::Toolbar
        | SemRole::Status
        | SemRole::Separator => TextBearing::No,
        SemRole::Unknown
        | SemRole::Generic
        | SemRole::Article
        | SemRole::Banner
        | SemRole::Blockquote
        | SemRole::Complementary
        | SemRole::ContentInfo
        | SemRole::DescriptionList
        | SemRole::Details
        | SemRole::Feed
        | SemRole::Figure
        | SemRole::Form
        | SemRole::Grid
        | SemRole::Group
        | SemRole::Iframe
        | SemRole::Log
        | SemRole::Main
        | SemRole::Marquee
        | SemRole::Math
        | SemRole::Meter
        | SemRole::Navigation
        | SemRole::Note
        | SemRole::Presentation
        | SemRole::RadioGroup
        | SemRole::Region
        | SemRole::RowGroup
        | SemRole::Scrollbar
        | SemRole::Search
        | SemRole::Suggestion
        | SemRole::Summary
        | SemRole::TabPanel
        | SemRole::Term
        | SemRole::Timer
        | SemRole::Comment
        | SemRole::Tooltip
        | SemRole::TreeGrid => TextBearing::IfClaimed,
    }
}

pub fn is_surface_role(r: SemRole) -> bool {
    matches!(
        r,
        SemRole::Window
            | SemRole::Dialog
            | SemRole::AlertDialog
            | SemRole::Alert
            | SemRole::Popover
    )
}

/// Fields every node of a model must have (name, role, state).
const NEED: PropMask = PropMask::NAME.union(PropMask::ROLE);

struct Ctx<'a, S: NavSource> {
    src: &'a S,
    surface: NodeKey,
    limits: ReviewLimits,
    out: Collected,
    seen: HashSet<NodeKey>,
    node_misses: HashMap<NodeKey, Vec<NodeKey>>,
    direct: Vec<Miss>,
    /// Engine row `text_leaves_repeat_parent`: `static` children of a text node are not walked.
    skip_repeats: bool,
}

impl<S: NavSource> Ctx<'_, S> {
    /// `text_parent`: the parent carries the text of its `static` children itself.
    fn walk(
        &mut self,
        k: NodeKey,
        parent: Option<NodeKey>,
        depth: u16,
        web: Web,
        text_parent: bool,
    ) {
        if self.out.truncated.is_some() || !self.seen.insert(k) {
            return;
        }
        if self.out.nodes.len() >= self.limits.nodes_per_build {
            self.out.truncated = Some(TruncWhy::NodeCap);
            return;
        }
        let v = match self.src.view(k) {
            Lookup::Gone => return,
            Lookup::Miss => {
                match parent {
                    Some(p) => self.node_misses.entry(p).or_default().push(k),
                    None => self.direct.push(Miss::Node(k)),
                }
                return;
            }
            Lookup::Hit(v) => v,
        };
        let s = &v.snap;
        // A text run (a `static` leaf) under a text node repeats what the parent already says. A
        // `static` that holds children is an inline element the parent only points to with a
        // U+FFFC (a `<time>`, a `<span>`): its text is not in the parent and it stays.
        if text_parent
            && s.have().contains(PropMask::ROLE)
            && entry_of(s).sem.role == SemRole::StaticText
            && (matches!(&s.children(), ChildList::Count(0))
                || matches!(&s.children(), ChildList::Known(k) if k.is_empty()))
        {
            return;
        }
        if !s.have().contains(NEED) || v.stale.intersects(NEED) {
            match parent {
                Some(p) => self.node_misses.entry(p).or_default().push(k),
                None => self.direct.push(Miss::Node(k)),
            }
            return;
        }
        let role = entry_of(s).sem.role;
        let web = match (web, role) {
            (Web::Outside, SemRole::DocumentWeb) => Web::Inside(
                (s.have().contains(PropMask::BOUNDS) && !v.stale.contains(PropMask::BOUNDS))
                    .then_some(s.bounds())
                    .flatten(),
            ),
            (w, _) => w,
        };
        let in_web = !matches!(web, Web::Outside);
        if class_of_snapshot(s, in_web) == PresClass::Unavailable {
            return;
        }
        // Gecko leaves SHOWING off the nodes outside the viewport (and off most containers): such
        // a node of a web document is entered only when it overlaps the document's rectangle, or
        // a long page spends the node cap on what is scrolled away.
        if let Web::Inside(doc) = web
            && role != SemRole::DocumentWeb
            && class_of_snapshot(s, false) == PresClass::Unavailable
        {
            let known = s.have().contains(PropMask::BOUNDS) && !v.stale.contains(PropMask::BOUNDS);
            match (doc, s.bounds()) {
                (Some(d), Some(b)) if known => {
                    if !overlaps(d, b) {
                        return;
                    }
                }
                (Some(_), _) if !known => {
                    self.direct.push(Miss::Bounds(k));
                    return;
                }
                // The document's own extents are not known yet (its fetch is queued).
                _ => return,
            }
        }
        if wants_lines(s)
            && let Web::Inside(Some(d)) = web
        {
            self.out.text_clips.insert(k, d);
        }
        // Another surface (SF-1): excluded here, it gets its own model.
        if k != self.surface && is_surface_role(role) {
            return;
        }
        let lacks_bounds =
            !s.have().contains(PropMask::BOUNDS) || v.stale.contains(PropMask::BOUNDS);
        let wants_bounds = depth == 0
            || class_of_snapshot(s, in_web) == PresClass::Content
            || s.struct_state().contains(StructFlags::FOCUSABLE)
            || wants_lines(s);
        if lacks_bounds && wants_bounds {
            self.direct.push(Miss::Bounds(k));
        }
        if wants_lines(s) {
            self.out.text_nodes.push(k);
        }
        let idx = self.out.nodes.len();
        self.out.nodes.push(BuildNode {
            snap: Arc::clone(s),
            web: in_web,
            depth,
            lines: None,
        });
        let _ = idx;
        let stale_children = v.stale.contains(PropMask::CHILDREN);
        match &s.children() {
            ChildList::Known(list) if !stale_children => {
                let text_parent = self.skip_repeats && wants_lines(s);
                for c in list {
                    self.walk(*c, Some(k), depth.saturating_add(1), web, text_parent);
                }
            }
            ChildList::Count(0) if !stale_children => {}
            ChildList::Known(_) | ChildList::Count(_) | ChildList::Unknown => {
                self.direct.push(Miss::Expand(k));
            }
        }
    }
}

/// Walk `surface` in `src`. Misses are deduplicated; two or more missing children of one parent
/// become a single `Expand` of the parent.
#[cfg(test)]
pub fn collect<S: NavSource>(src: &S, surface: NodeKey, limits: ReviewLimits) -> Collected {
    collect_with(src, surface, limits, false)
}

/// [`collect`] for an engine whose text nodes repeat their text in `static` children
/// (`skip_repeats`, `EditQuirks::text_leaves_repeat_parent`).
pub fn collect_with<S: NavSource>(
    src: &S,
    surface: NodeKey,
    limits: ReviewLimits,
    skip_repeats: bool,
) -> Collected {
    let mut cx = Ctx {
        src,
        surface,
        limits,
        out: Collected::default(),
        seen: HashSet::new(),
        node_misses: HashMap::new(),
        direct: Vec::new(),
        skip_repeats,
    };
    cx.walk(surface, None, 0, Web::Outside, false);
    let mut misses: Vec<Miss> = Vec::new();
    let mut seen: HashSet<Miss> = HashSet::new();
    let mut push = |m: Miss, misses: &mut Vec<Miss>| {
        if seen.insert(m) {
            misses.push(m);
        }
    };
    // Structure first, extents last: bounds are asked for with the structure fetches.
    let mut parents: Vec<_> = cx.node_misses.iter().collect();
    parents.sort_by_key(|(p, _)| **p);
    for (p, kids) in parents {
        if kids.len() >= 2 {
            push(Miss::Expand(*p), &mut misses);
        } else {
            for k in kids {
                push(Miss::Node(*k), &mut misses);
            }
        }
    }
    for m in &cx.direct {
        if !matches!(m, Miss::Bounds(_)) {
            push(*m, &mut misses);
        }
    }
    let expanded: HashSet<NodeKey> = misses
        .iter()
        .filter_map(|m| match m {
            Miss::Expand(k) => Some(*k),
            Miss::Node(_) | Miss::Bounds(_) => None,
        })
        .collect();
    for m in &cx.direct {
        if let Miss::Bounds(k) = m
            && !expanded.contains(k)
        {
            push(*m, &mut misses);
        }
    }
    let mut out = cx.out;
    out.misses = misses;
    out
}

#[cfg(test)]
pub(crate) mod testsrc {
    //! A map-backed mirror stand-in for the review tests.

    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::Instant;

    use gorca_model::{
        AppId, ChildList, ConnectorId, Freshness, LocalId, Lookup, NodeKey, NodeSnapshot, NodeView,
        PropMask, Rect, Role, StateSet,
    };

    use super::super::objnav::NavSource;

    pub fn key(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(n),
        }
    }

    #[derive(Default)]
    pub struct MapSrc {
        pub nodes: HashMap<NodeKey, Lookup<NodeView>>,
    }

    impl NavSource for MapSrc {
        fn view(&self, k: NodeKey) -> Lookup<NodeView> {
            self.nodes.get(&k).cloned().unwrap_or(Lookup::Miss)
        }
    }

    pub fn snap(n: u64, role: Role, name: &str, parent: Option<u64>, kids: &[u64]) -> NodeSnapshot {
        NodeSnapshot::new(key(n), Freshness::Fresh { at: Instant::now() })
            .with_name((!name.is_empty()).then(|| Arc::from(name)))
            .with_parent(parent.map(key))
            .with_children(ChildList::Known(kids.iter().map(|k| key(*k)).collect()))
            .with_have(PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT)
            .with_test_source(role, StateSet::SHOWING | StateSet::VISIBLE)
    }

    impl MapSrc {
        pub fn put(&mut self, s: NodeSnapshot) {
            self.nodes.insert(
                s.key(),
                Lookup::Hit(NodeView {
                    snap: Arc::new(s),
                    stale: PropMask::empty(),
                    aged: false,
                }),
            );
        }

        pub fn put_at(&mut self, mut s: NodeSnapshot, r: Rect) {
            s.set_bounds(Some(r));
            s.set_have(s.have() | PropMask::BOUNDS);
            self.put(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testsrc::*;
    use super::*;
    use gorca_model::{Ifaces, Rect, Role, StateSet};

    fn r(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 50, h: 10 }
    }

    #[test]
    fn lines_are_asked_of_text_roles_and_of_fillers_only_on_a_claim() {
        let mut label = snap(1, Role::Label, "L", None, &[]);
        let mut panel = snap(2, Role::Panel, "P", None, &[]);
        let mut button = snap(3, Role::Button, "B", None, &[]);
        // No interface claim: text-bearing roles are asked, fillers and widgets are not.
        assert!(wants_lines(&label));
        assert!(!wants_lines(&panel));
        assert!(!wants_lines(&button));
        // A claim of Text: the filler is asked too; a widget never is.
        for s in [&mut label, &mut panel, &mut button] {
            s.set_caps(gorca_model::NodeCaps::new(Ifaces::TEXT));
        }
        assert!(wants_lines(&label) && wants_lines(&panel));
        assert!(!wants_lines(&button));
        // A claim without Text: nothing is asked.
        label.set_caps(gorca_model::NodeCaps::new(Ifaces::ACTION));
        assert!(!wants_lines(&label));
    }

    #[test]
    fn surface_roles_are_windows_dialogs_alerts_and_popovers() {
        for r in [
            SemRole::Window,
            SemRole::Dialog,
            SemRole::AlertDialog,
            SemRole::Alert,
            SemRole::Popover,
        ] {
            assert!(is_surface_role(r), "{r:?}");
        }
        for r in [SemRole::Menu, SemRole::Application, SemRole::Generic] {
            assert!(!is_surface_role(r), "{r:?}");
        }
    }

    #[test]
    fn a_complete_mirror_has_no_misses_and_prunes_hidden_subtrees() {
        let mut m = MapSrc::default();
        m.put_at(snap(1, Role::Window, "W", None, &[2, 3, 4]), r(0, 0));
        m.put_at(snap(2, Role::Button, "OK", Some(1), &[]), r(0, 0));
        let mut hidden = snap(3, Role::Panel, "", Some(1), &[5]);
        hidden.set_test_states(StateSet::empty());
        hidden.set_have(hidden.have() | PropMask::STATES);
        m.put(hidden);
        m.put(snap(5, Role::Button, "Never", Some(3), &[]));
        m.put_at(
            snap(4, Role::Dialog, "Child surface", Some(1), &[]),
            r(0, 0),
        );
        let c = collect(&m, key(1), ReviewLimits::default());
        let names: Vec<_> = c.nodes.iter().map(|n| n.snap.key().local.0).collect();
        assert_eq!(names, [1, 2]);
        assert!(c.misses.is_empty(), "{:?}", c.misses);
    }

    #[test]
    fn missing_children_collapse_into_one_expand_and_missing_extents_are_listed() {
        let mut m = MapSrc::default();
        m.put(snap(1, Role::Window, "W", None, &[2, 3, 4]));
        m.put_at(snap(4, Role::Button, "C", Some(1), &[]), r(0, 20));
        let c = collect(&m, key(1), ReviewLimits::default());
        assert!(c.misses.contains(&Miss::Expand(key(1))));
        assert!(
            !c.misses.contains(&Miss::Bounds(key(1))),
            "an expansion brings the extents with it"
        );
        // One missing child alone is a node fetch.
        let mut m2 = MapSrc::default();
        m2.put_at(snap(1, Role::Window, "W", None, &[2, 4]), r(0, 0));
        m2.put_at(snap(4, Role::Button, "C", Some(1), &[]), r(0, 20));
        let c2 = collect(&m2, key(1), ReviewLimits::default());
        assert_eq!(c2.misses, [Miss::Node(key(2))]);
    }

    /// Gecko marks only some web nodes SHOWING (links, controls), not containers or text: inside a
    /// web document VISIBLE is enough; outside one a VISIBLE-only node is still pruned.
    #[test]
    fn a_web_documents_visible_only_nodes_are_kept_and_other_toolkits_are_not() {
        let visible_only = |mut s: NodeSnapshot| {
            s.set_test_states(StateSet::VISIBLE);
            s.set_have(s.have() | PropMask::STATES);
            s
        };
        let mut m = MapSrc::default();
        m.put_at(snap(1, Role::Window, "W", None, &[2, 5]), r(0, 0));
        let page = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 100,
        };
        m.put_at(snap(2, Role::DocumentWeb, "Page", Some(1), &[3]), page);
        m.put_at(
            visible_only(snap(3, Role::Landmark, "Main", Some(2), &[4])),
            r(0, 10),
        );
        m.put_at(
            visible_only(snap(4, Role::Heading, "Title", Some(3), &[])),
            r(0, 20),
        );
        m.put_at(
            visible_only(snap(5, Role::Panel, "Native", Some(1), &[6])),
            r(0, 30),
        );
        m.put_at(snap(6, Role::Button, "Never", Some(5), &[]), r(0, 40));
        let c = collect(&m, key(1), ReviewLimits::default());
        let ids: Vec<_> = c.nodes.iter().map(|n| n.snap.key().local.0).collect();
        assert_eq!(ids, [1, 2, 3, 4]);
    }

    /// A long scrolled page: thousands of VISIBLE-only paragraphs above the viewport must not use
    /// up the node cap before the on-screen heading (review H-1).
    #[test]
    fn a_long_scrolled_page_reaches_its_on_screen_content() {
        let visible_only = |mut s: NodeSnapshot| {
            s.set_test_states(StateSet::VISIBLE);
            s.set_have(s.have() | PropMask::STATES);
            s
        };
        let page = Rect {
            x: 0,
            y: 0,
            w: 200,
            h: 100,
        };
        let above = |y: i32| Rect {
            x: 0,
            y,
            w: 50,
            h: 10,
        };
        let mut m = MapSrc::default();
        let kids: Vec<u64> = (10..5010).collect();
        let mut doc_kids = kids.clone();
        doc_kids.push(9);
        m.put_at(snap(1, Role::Window, "W", None, &[2]), page);
        m.put_at(snap(2, Role::DocumentWeb, "Page", Some(1), &doc_kids), page);
        for k in &kids {
            let y = -10_000 + i32::try_from(*k).unwrap_or(0);
            m.put_at(
                visible_only(snap(*k, Role::Paragraph, "p", Some(2), &[])),
                above(y),
            );
        }
        m.put_at(
            visible_only(snap(9, Role::Heading, "On screen", Some(2), &[])),
            above(20),
        );
        let c = collect(&m, key(1), ReviewLimits::default());
        assert!(c.truncated.is_none(), "{:?}", c.truncated);
        assert!(c.nodes.iter().any(|n| n.snap.key().local.0 == 9));
        assert_eq!(c.nodes.len(), 3, "window, document, heading");
        assert_eq!(c.text_clips.get(&key(9)), Some(&page));
    }

    #[test]
    fn the_node_cap_truncates() {
        let mut m = MapSrc::default();
        let kids: Vec<u64> = (2..40).collect();
        m.put_at(snap(1, Role::Window, "W", None, &kids), r(0, 0));
        for k in &kids {
            m.put_at(snap(*k, Role::Button, "b", Some(1), &[]), r(0, 0));
        }
        let lim = ReviewLimits {
            nodes_per_build: 10,
            ..ReviewLimits::default()
        };
        let c = collect(&m, key(1), lim);
        assert_eq!(c.nodes.len(), 10);
        assert_eq!(c.truncated, Some(TruncWhy::NodeCap));
    }

    /// A Chromium-shaped page (docs/reports/chromium-flat-review.md): every node on screen has
    /// SHOWING and VISIBLE, what is scrolled away has VISIBLE only, and a text node has `static`
    /// children that repeat its text. The engine row drops those children; off screen stays out.
    #[test]
    fn a_blink_shaped_page_skips_repeating_static_leaves_and_off_screen_nodes() {
        let visible_only = |mut s: NodeSnapshot| {
            s.set_test_states(StateSet::VISIBLE);
            s.set_have(s.have() | PropMask::STATES);
            s
        };
        let text = |mut s: NodeSnapshot| {
            s.set_caps(gorca_model::NodeCaps::new(Ifaces::TEXT));
            s
        };
        let page = Rect {
            x: 0,
            y: 143,
            w: 1280,
            h: 625,
        };
        let row = |y: i32| Rect {
            x: 152,
            y,
            w: 300,
            h: 23,
        };
        let mut m = MapSrc::default();
        m.put_at(snap(1, Role::Window, "W", None, &[2]), page);
        m.put_at(
            snap(2, Role::DocumentWeb, "Page", Some(1), &[3, 5, 7, 8]),
            page,
        );
        m.put_at(
            text(snap(3, Role::Paragraph, "", Some(2), &[4, 9])),
            row(200),
        );
        m.put_at(
            text(snap(4, Role::StaticText, "Hello", Some(3), &[])),
            row(200),
        );
        // A link: its own text, and a static leaf repeating it.
        m.put_at(text(snap(5, Role::Link, "Home", Some(2), &[6])), row(240));
        m.put_at(
            text(snap(6, Role::StaticText, "Home", Some(5), &[])),
            row(240),
        );
        // Scrolled away (below the document), and on screen without SHOWING.
        m.put_at(
            visible_only(text(snap(7, Role::Paragraph, "", Some(2), &[]))),
            row(2_000),
        );
        // An inline element (a `<time>`) is a `static` with its own run: it stays, its run goes.
        m.put_at(
            text(snap(9, Role::StaticText, "", Some(3), &[10])),
            row(200),
        );
        m.put_at(
            text(snap(10, Role::StaticText, "Today", Some(9), &[])),
            row(200),
        );
        m.put_at(
            visible_only(text(snap(8, Role::Heading, "Title", Some(2), &[]))),
            row(300),
        );
        let ids =
            |c: &Collected| -> Vec<u64> { c.nodes.iter().map(|n| n.snap.key().local.0).collect() };
        let plain = collect(&m, key(1), ReviewLimits::default());
        assert_eq!(ids(&plain), [1, 2, 3, 4, 9, 10, 5, 6, 8]);
        let row_on = collect_with(&m, key(1), ReviewLimits::default(), true);
        assert_eq!(ids(&row_on), [1, 2, 3, 9, 5, 8]);
        assert!(
            !row_on.text_nodes.contains(&key(4)),
            "no lines asked for a repeat"
        );
        assert!(row_on.misses.is_empty(), "{:?}", row_on.misses);
    }
}
