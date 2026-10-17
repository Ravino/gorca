// SPDX-License-Identifier: GPL-3.0-or-later
//! The surface walk of a model build (ADR-0006 6.1 steps 1 and 2, 6.2): read the mirror under
//! the surface root in pre-order, prune what is not showing and what is another surface, and
//! list what the mirror lacks. Pure over a [`NavSource`]; the build task fetches the misses and
//! walks again.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gorca_model::{ChildList, Ifaces, Lookup, NodeKey, NodeSnapshot, PropMask, Role, StateSet};

use super::cluster::BuildNode;
use super::model::{ReviewLimits, TruncWhy};
use super::objnav::{NavSource, PresClass, class};

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
}

/// Nodes the app can hang text lines on. A snapshot from a focus fetch or a child fetch does not
/// say which interfaces the node has (`Ifaces` is empty), so for those the text-bearing roles are
/// asked and a refusal is the answer; a node known to have no `Text` is not asked.
pub fn wants_lines(s: &NodeSnapshot) -> bool {
    let known = !s.interfaces.is_empty();
    if known && !s.interfaces.contains(Ifaces::TEXT) {
        return false;
    }
    match s.role {
        Role::Label
        | Role::StaticText
        | Role::Paragraph
        | Role::Heading
        | Role::Link
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::Document
        | Role::DocumentWeb
        | Role::Terminal
        | Role::ListItem
        | Role::Cell
        | Role::Alert => true,
        // Only on an explicit claim: every filler and container has these roles.
        Role::Unknown | Role::Landmark | Role::Panel => known,
        Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Image
        | Role::List
        | Role::Table
        | Role::Row
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator => false,
    }
}

fn is_surface_role(r: Role) -> bool {
    matches!(r, Role::Window | Role::Dialog | Role::Alert | Role::Popover)
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
}

impl<S: NavSource> Ctx<'_, S> {
    fn walk(&mut self, k: NodeKey, parent: Option<NodeKey>, depth: u16) {
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
        if !s.have.contains(NEED) || v.stale.intersects(NEED) {
            match parent {
                Some(p) => self.node_misses.entry(p).or_default().push(k),
                None => self.direct.push(Miss::Node(k)),
            }
            return;
        }
        if class(s) == PresClass::Unavailable {
            return;
        }
        // Another surface (SF-1): excluded here, it gets its own model.
        if k != self.surface && is_surface_role(s.role) {
            return;
        }
        let lacks_bounds = !s.have.contains(PropMask::BOUNDS) || v.stale.contains(PropMask::BOUNDS);
        let wants_bounds = depth == 0
            || class(s) == PresClass::Content
            || s.states.contains(StateSet::FOCUSABLE)
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
            depth,
            lines: None,
        });
        let _ = idx;
        let stale_children = v.stale.contains(PropMask::CHILDREN);
        match &s.children {
            ChildList::Known(list) if !stale_children => {
                for c in list {
                    self.walk(*c, Some(k), depth.saturating_add(1));
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
pub fn collect<S: NavSource>(src: &S, surface: NodeKey, limits: ReviewLimits) -> Collected {
    let mut cx = Ctx {
        src,
        surface,
        limits,
        out: Collected::default(),
        seen: HashSet::new(),
        node_misses: HashMap::new(),
        direct: Vec::new(),
    };
    cx.walk(surface, None, 0);
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
        AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeKey,
        NodeSnapshot, NodeView, PropMask, Rect, Relations, Role, StateSet,
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
        NodeSnapshot {
            key: key(n),
            role,
            states: StateSet::SHOWING | StateSet::VISIBLE,
            name: (!name.is_empty()).then(|| Arc::from(name)),
            description: None,
            value: None,
            parent: parent.map(key),
            children: ChildList::Known(kids.iter().map(|k| key(*k)).collect()),
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: None,
            have: PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        }
    }

    impl MapSrc {
        pub fn put(&mut self, s: NodeSnapshot) {
            self.nodes.insert(
                s.key,
                Lookup::Hit(NodeView {
                    snap: Arc::new(s),
                    stale: PropMask::empty(),
                    aged: false,
                }),
            );
        }

        pub fn put_at(&mut self, mut s: NodeSnapshot, r: Rect) {
            s.bounds = Some(r);
            s.have |= PropMask::BOUNDS;
            self.put(s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testsrc::*;
    use super::*;
    use gorca_model::Rect;

    fn r(x: i32, y: i32) -> Rect {
        Rect { x, y, w: 50, h: 10 }
    }

    #[test]
    fn a_complete_mirror_has_no_misses_and_prunes_hidden_subtrees() {
        let mut m = MapSrc::default();
        m.put_at(snap(1, Role::Window, "W", None, &[2, 3, 4]), r(0, 0));
        m.put_at(snap(2, Role::Button, "OK", Some(1), &[]), r(0, 0));
        let mut hidden = snap(3, Role::Panel, "", Some(1), &[5]);
        hidden.states = StateSet::empty();
        hidden.have |= PropMask::STATES;
        m.put(hidden);
        m.put(snap(5, Role::Button, "Never", Some(3), &[]));
        m.put_at(
            snap(4, Role::Dialog, "Child surface", Some(1), &[]),
            r(0, 0),
        );
        let c = collect(&m, key(1), ReviewLimits::default());
        let names: Vec<_> = c.nodes.iter().map(|n| n.snap.key.local.0).collect();
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
}
