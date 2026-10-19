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

use gorca_model::{ChildList, Lookup, NodeKey, NodeSnapshot, NodeView, PropMask, Role, StateSet};

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

/// `fn class(v: &NodeView) -> PresClass` (ADR-0006 5.1). Roles that the model does not have as
/// their own (filler, scroll pane, split pane, section, root pane, ...) arrive as `Unknown` or
/// `Panel` from the connector and are layout.
pub fn class(snap: &NodeSnapshot) -> PresClass {
    class_in(snap, false)
}

/// [`class`] for a node inside a web document (`web`): Gecko sets `SHOWING` on some of its nodes
/// only (links and controls, not containers, landmarks or text), so `VISIBLE` alone counts there;
/// what is off screen is clipped by its extents.
pub fn class_in(snap: &NodeSnapshot, web: bool) -> PresClass {
    let showing = !snap.have.contains(PropMask::STATES)
        || (snap.states.contains(StateSet::SHOWING) && snap.states.contains(StateSet::VISIBLE))
        // Some toolkits set only one of the pair.
        || snap.states.contains(StateSet::SHOWING)
        || (web && snap.states.contains(StateSet::VISIBLE));
    if snap.states.contains(StateSet::DEFUNCT) || !showing {
        return PresClass::Unavailable;
    }
    let nameless = blank(snap.name.as_ref()) && blank(snap.description.as_ref());
    match snap.role {
        Role::Unknown | Role::Panel | Role::Paragraph | Role::Label | Role::Separator => {
            PresClass::Layout
        }
        Role::Window | Role::Landmark if nameless => PresClass::Layout,
        Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Document
        | Role::DocumentWeb
        | Role::Heading
        | Role::Link
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
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
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => PresClass::Content,
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
                    !v.snap.have.contains(CRITICAL) || v.stale.intersects(CRITICAL);
                if critical_missing {
                    return self.miss(NavOp::Node { node: k });
                }
                if class(&v.snap) == PresClass::Content {
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
        let stale_children = match self.src.view(r.snap.key) {
            Lookup::Hit(v) => v.stale.contains(PropMask::CHILDREN),
            Lookup::Miss | Lookup::Gone => false,
        };
        match &r.snap.children {
            ChildList::Known(list) if !stale_children => Ok(list.to_vec()),
            ChildList::Count(0) if !stale_children => Ok(Vec::new()),
            ChildList::Known(_) | ChildList::Count(_) | ChildList::Unknown => {
                if self.lenient {
                    // A stale list is better than none: it is only a degraded step.
                    match &r.snap.children {
                        ChildList::Known(list) => Ok(list.to_vec()),
                        ChildList::Count(_) | ChildList::Unknown => Ok(Vec::new()),
                    }
                } else {
                    Err(Stop::Miss(NavOp::Children { node: r.snap.key }))
                }
            }
        }
    }

    fn parent(&mut self, r: &Read) -> R<Option<NodeKey>> {
        if r.snap.role == Role::Application {
            // Never above the application root (ADR-0006 5.2).
            return Ok(None);
        }
        match r.snap.parent {
            Some(p) if r.snap.have.contains(PropMask::PARENT) || self.lenient => Ok(Some(p)),
            Some(_) | None => {
                if r.snap.have.contains(PropMask::PARENT) || self.lenient {
                    Ok(None)
                } else {
                    Err(Stop::Miss(NavOp::Ancestry { node: r.snap.key }))
                }
            }
        }
    }

    fn class_of(&self, r: &Read) -> PresClass {
        if self.simple {
            class(&r.snap)
        } else if r.snap.states.contains(StateSet::DEFUNCT) {
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
                cur = pr.snap.key;
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
