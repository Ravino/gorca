// SPDX-License-Identifier: GPL-3.0-or-later
//! Mirroring the virtual browse selection into the page (NVDA copies its own buffer; gorca lets
//! the browser copy, which keeps the formatting and needs no clipboard of ours).
//!
//! The virtual selection is a range of flat-text offsets. The *atoms* it contains are the text
//! scalars and the images: they are what a DOM selection can hold, and they form one contiguous
//! interval (block breaks, cell gaps and the markers of empty controls have no DOM counterpart).
//! The page selects through AT-SPI `Text.SetSelection` on a *hypertext*: a container's text with
//! one U+FFFC for every embedded child. The mirror splits that text into leaves (`StaticText`
//! nodes) and objects (images), so an atom interval becomes spans `(container, start, end)` in
//! hypertext offsets, and the same interval as two points for engines that select across objects.
//!
//! The spans here are engine-neutral: `end` is exclusive and exact, offsets come in UTF-16 units
//! (Gecko) and in scalars (Blink). What an engine does with an end offset that names an embedded
//! child differs, so each span also says what its end touches (`SelSpan`), and the document source
//! adapts it (`gorca-doc-source-atspi`; measurements in `docs/reports/ctrl-c-passthrough.md`,
//! model and proofs in `docs/reports/selection-mapping-math.md`).
//!
//! A range whose ends sit in different containers is planned three ways: `doc_range`, the exact
//! interval (one call on Blink), `spans`, the exact legs (tail of the start branch, whole blocks
//! between, head of the end branch, for engines that keep several ranges), and `snap`, one range
//! on the common ancestor that rounds nested ends out to whole children (engines that keep one).

use gorca_doc::web::{NodeFlags, WebDoc};
use gorca_model::sem::SemRole;
use gorca_model::{DocNodeId, DocRange, SelSpan};

/// How to select a flat range in the page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    /// Exact ranges in document order; one for a selection inside a container.
    pub spans: Vec<SelSpan<DocNodeId>>,
    /// The single-range alternative on the common ancestor.
    pub snap: SelSpan<DocNodeId>,
    /// The exact atom interval as two points.
    pub doc_range: DocRange<DocNodeId>,
    /// The container of the first atom: when the engine focuses it, the selection steals focus.
    pub start_container: DocNodeId,
    /// Flat offset where the selected part of the first atom ends: a caller that must not start
    /// in that atom's container plans again from here.
    pub first_end: u32,
    /// Gecko cannot take this exactly: a range ends right before a hypertext child it continues
    /// into (the child comes along), or several ranges are joined with no separator.
    pub gecko_lossy: bool,
}

/// The marker an image leaves in the flat text.
const OBJECT: char = '\u{FFFC}';

/// Offsets in a container's hypertext: UTF-16 units and scalars.
type Off = (u32, u32);

fn add(a: Off, b: Off) -> Off {
    (a.0 + b.0, a.1 + b.1)
}

fn is_leaf(d: &WebDoc, id: DocNodeId) -> bool {
    d.node(id)
        .is_some_and(|v| v.role.is_inline_text() && v.text.is_some())
        && d.child_count(id) == Some(0)
}

/// An embedded object with no text of its own: an image (its flat text is the U+FFFC marker).
fn is_object(d: &WebDoc, id: DocNodeId) -> bool {
    d.node(id)
        .is_some_and(|v| v.role == SemRole::Img && v.text.is_some_and(|t| t.chars().eq([OBJECT])))
        && d.child_count(id) == Some(0)
}

fn is_atom(d: &WebDoc, id: DocNodeId) -> bool {
    is_leaf(d, id) || is_object(d, id)
}

fn units(c: char) -> u32 {
    u32::try_from(c.len_utf16()).unwrap_or(1)
}

/// The offsets after the first `scalars` scalars of `t`.
fn prefix(t: &str, scalars: u32) -> Off {
    let u = t.chars().take(scalars as usize).map(units).sum();
    (
        u,
        scalars.min(u32::try_from(t.chars().count()).unwrap_or(u32::MAX)),
    )
}

/// What `id` adds to its parent's hypertext: its text for a leaf, one embedded character else.
fn contrib(d: &WebDoc, id: DocNodeId) -> Off {
    match d.node(id) {
        Some(v) if is_leaf(d, id) => v.text.map_or((0, 0), |t| {
            (
                t.chars().map(units).sum(),
                u32::try_from(t.chars().count()).unwrap_or(u32::MAX),
            )
        }),
        Some(_) | None => (1, 1),
    }
}

/// Hypertext offsets of the start of `child` in its parent.
fn pos(d: &WebDoc, parent: DocNodeId, child: DocNodeId) -> Off {
    d.children(parent)
        .take_while(|c| *c != child)
        .fold((0, 0), |a, c| add(a, contrib(d, c)))
}

fn hyper_len(d: &WebDoc, parent: DocNodeId) -> Off {
    d.children(parent)
        .fold((0, 0), |a, c| add(a, contrib(d, c)))
}

/// The child of `p` whose hypertext contains UTF-16 offset `idx`, with its start offset.
fn child_at(d: &WebDoc, p: DocNodeId, idx: u32) -> Option<(DocNodeId, u32)> {
    let mut start = 0;
    for c in d.children(p) {
        let end = start + contrib(d, c).0;
        if idx < end {
            return Some((c, start));
        }
        start = end;
    }
    None
}

/// A span `start..end` of `node` with what its end touches.
fn span(d: &WebDoc, node: DocNodeId, start: Off, end: Off) -> SelSpan<DocNodeId> {
    let last = end.0.checked_sub(1).and_then(|i| child_at(d, node, i));
    let next = child_at(d, node, end.0);
    let end_on_child = last.is_some_and(|(c, _)| !is_atom(d, c));
    let end_on_object = last.is_some_and(|(c, _)| is_object(d, c));
    let child_follows = next.is_some_and(|(c, _)| !is_atom(d, c));
    let trim_ws = last.is_some_and(|(c, cs)| {
        is_leaf(d, c)
            && d.node(c).and_then(|v| v.text).is_some_and(|t| {
                let mut at = cs;
                let target = end.0 - 1;
                t.chars()
                    .find(|ch| {
                        let here = at;
                        at += units(*ch);
                        target >= here && target < at
                    })
                    .is_some_and(char::is_whitespace)
            })
    });
    SelSpan {
        node,
        range: start.0..end.0,
        range_cp: start.1..end.1,
        end_on_child,
        end_on_object,
        child_follows,
        trim_ws,
    }
}

fn ancestors(d: &WebDoc, from: DocNodeId) -> Vec<DocNodeId> {
    let mut v = vec![from];
    let mut cur = from;
    while let Some(p) = d.parent(cur) {
        v.push(p);
        cur = p;
    }
    v
}

/// True when `id` or an ancestor is a password field: its text is never mirrored or copied.
/// Whether a document node is a secret field: its fused entry says so (the role or the source's
/// mark), or, without an entry, its role. Every sink of browse mode asks this.
pub(super) fn node_is_secret(v: &gorca_doc::web::NodeView<'_>) -> bool {
    v.sem().as_deref().map_or_else(
        || gorca_present::is_secret_role(v.role),
        gorca_present::is_secret_entry,
    )
}

fn secret(d: &WebDoc, id: DocNodeId) -> bool {
    let mut cur = Some(id);
    for _ in 0..64 {
        let Some(n) = cur else { return false };
        if d.node(n).is_some_and(|v| node_is_secret(&v)) {
            return true;
        }
        cur = d.parent(n);
    }
    false
}

/// An atom met by the range, with the scalar offsets of the range inside its text.
struct Hit {
    id: DocNodeId,
    object: bool,
    from: u32,
    to: u32,
    scalars: u32,
    /// Flat offset just after the selected part of the atom.
    end_flat: u32,
}

impl Hit {
    /// Hypertext offsets inside the atom after `k` scalars.
    fn off(&self, d: &WebDoc, k: u32) -> Off {
        if self.object {
            return (k, k);
        }
        d.node(self.id)
            .and_then(|v| v.text)
            .map_or((0, 0), |t| prefix(t, k))
    }
}

fn atoms(d: &WebDoc, lo: u32, hi: u32) -> Option<(Hit, Hit)> {
    let (n0, _) = d.at_offset(lo)?;
    let mut first: Option<Hit> = None;
    let mut last: Option<Hit> = None;
    for id in d.preorder_from(n0) {
        let start = d.offset_of(id)?;
        if start >= hi {
            break;
        }
        let v = d.node(id)?;
        let leaf = is_leaf(d, id);
        let object = !leaf && is_object(d, id);
        if v.flags.contains(NodeFlags::HIDDEN) || !(leaf || object) {
            if node_is_secret(&v) {
                // Never mirror (and so never copy) a password field: the range stops before it.
                break;
            }
            continue;
        }
        if secret(d, id) {
            break;
        }
        let lead = u32::from(
            v.flags.contains(NodeFlags::BREAK_BEFORE) || v.flags.contains(NodeFlags::CELL_SEP),
        );
        let n = if object {
            1
        } else {
            u32::try_from(v.text?.chars().count()).unwrap_or(u32::MAX)
        };
        let ts = start + lead;
        if ts + n <= lo {
            continue;
        }
        let from = lo.saturating_sub(ts).min(n);
        let to = hi.saturating_sub(ts).min(n);
        if to == 0 || from >= to {
            continue;
        }
        let mk = || Hit {
            id,
            object,
            from,
            to,
            scalars: n,
            end_flat: ts + to,
        };
        if first.is_none() {
            first = Some(mk());
        }
        last = Some(mk());
    }
    Some((first?, last?))
}

/// Plan the page selection of the flat range `[lo, hi)`. `None` when it holds no atom. A password
/// field ends the range before it.
#[cfg(test)]
pub fn plan(d: &WebDoc, lo: u32, hi: u32) -> Option<Plan> {
    plan_with(d, lo, hi, &|_, _| false)
}

/// Like [`plan`], for an engine that focuses the container a range starts in when it is focusable
/// and not focused yet (Firefox, `TextLeafRange::SetSelection`): `risky` names such containers.
/// A leg other than the first that starts in one is left out, and the leg before it ends in front
/// of the child, which the engine takes whole: the selection grows to the end of that child
/// (`gecko_lossy`) instead of moving system focus (BM-2). The first leg is the caller's to
/// refuse (`Plan::start_container`).
pub fn plan_with(
    d: &WebDoc,
    lo: u32,
    hi: u32,
    risky: &dyn Fn(&WebDoc, DocNodeId) -> bool,
) -> Option<Plan> {
    if lo >= hi {
        return None;
    }
    let (f, l) = atoms(d, lo, hi)?;
    let f_par = d.parent(f.id)?;
    let l_par = d.parent(l.id)?;
    let s_in = |p: DocNodeId| add(pos(d, p, f.id), f.off(d, f.from));
    let e_in = |p: DocNodeId| add(pos(d, p, l.id), l.off(d, l.to));
    let doc_range = DocRange {
        start: (f_par, s_in(f_par).1),
        end: (l_par, e_in(l_par).1),
    };

    let (spans, snap, gecko_lossy) = if f_par == l_par {
        let sp = span(d, f_par, s_in(f_par), e_in(f_par));
        let lossy = single_lossy(d, &sp);
        (vec![sp.clone()], sp, lossy)
    } else {
        // The common ancestor of the two containers, and the children of it on each side.
        let chain_f = ancestors(d, f_par);
        let chain_l = ancestors(d, l_par);
        let c = *chain_f.iter().find(|a| chain_l.contains(a))?;
        let x_s = child_toward(d, &chain_f, c, f.id);
        let x_e = child_toward(d, &chain_l, c, l.id);
        let snap_s = if f_par == c { s_in(c) } else { pos(d, c, x_s) };
        let snap_e = if l_par == c {
            e_in(c)
        } else {
            add(pos(d, c, x_e), (1, 1))
        };
        let snap = span(d, c, snap_s, snap_e);
        // Whole blocks at both ends: the rounded-out range is exact, and one range keeps the
        // line breaks between blocks (separate ranges are joined with nothing).
        let whole_start = f_par == c || (f.from == 0 && first_atom(d, x_s) == Some(f.id));
        let whole_end = l_par == c || (l.to == l.scalars && last_atom(d, x_e) == Some(l.id));
        if whole_start && whole_end {
            let lossy = single_lossy(d, &snap);
            (vec![snap.clone()], snap, lossy)
        } else {
            let legs = exact_legs(
                d,
                (&f, &l),
                (&chain_f, &chain_l),
                (c, x_s, x_e),
                (whole_start, whole_end),
            );
            (legs, snap, true)
        }
    };
    let mut spans = spans;
    let before = spans.len();
    let mut kept = 0;
    spans.retain(|s| {
        let keep = kept == 0 || !start_leaf_parent(d, s).is_some_and(|p| risky(d, p));
        kept += 1;
        keep
    });
    let gecko_lossy = gecko_lossy || spans.len() != before;
    // Only the end of the whole selection may drop a trailing space: where two legs meet the
    // space is real text ("See li" must not become "Seeli").
    let last_ix = spans.len().saturating_sub(1);
    for (i, s) in spans.iter_mut().enumerate() {
        if i != last_ix {
            s.trim_ws = false;
        }
    }
    Some(Plan {
        spans,
        snap,
        doc_range,
        start_container: f_par,
        first_end: f.end_flat,
        gecko_lossy,
    })
}

/// A single range is inexact on Gecko when it ends right before a hypertext child at a letter
/// (the child comes along), or ends on an image at the very end of its container.
fn single_lossy(d: &WebDoc, sp: &SelSpan<DocNodeId>) -> bool {
    (sp.child_follows && !sp.trim_ws)
        || (sp.end_on_object && sp.range.end >= hyper_len(d, sp.node).0)
}

/// The container of the leaf a span starts in: the engine focuses that one, which is `span.node`
/// when the start offset names a text leaf or an image there, and a descendant when it names a
/// hypertext child (the start resolves to the child's first atom).
fn start_leaf_parent(d: &WebDoc, sp: &SelSpan<DocNodeId>) -> Option<DocNodeId> {
    let (child, _) = child_at(d, sp.node, sp.range.start)?;
    if is_atom(d, child) {
        return Some(sp.node);
    }
    let a = first_atom(d, child)?;
    d.parent(a)
}

fn first_atom(d: &WebDoc, from: DocNodeId) -> Option<DocNodeId> {
    let mut cur = from;
    loop {
        if is_atom(d, cur) {
            return Some(cur);
        }
        cur = d.first_child(cur)?;
    }
}

fn last_atom(d: &WebDoc, from: DocNodeId) -> Option<DocNodeId> {
    let mut cur = from;
    loop {
        if is_atom(d, cur) {
            return Some(cur);
        }
        cur = d.children(cur).last()?;
    }
}

/// The child of `c` on the way down to `leaf` (the leaf itself when `c` is its parent).
fn child_toward(d: &WebDoc, chain: &[DocNodeId], c: DocNodeId, leaf: DocNodeId) -> DocNodeId {
    if d.parent(leaf) == Some(c) {
        return leaf;
    }
    chain
        .iter()
        .copied()
        .take_while(|a| *a != c)
        .last()
        .unwrap_or(leaf)
}

fn push_span(d: &WebDoc, node: DocNodeId, s: Off, e: Off, out: &mut Vec<SelSpan<DocNodeId>>) {
    if s.0 < e.0 {
        out.push(span(d, node, s, e));
    }
}

/// The exact legs: the tail of the start branch, the blocks between, the head of the end branch.
fn exact_legs(
    d: &WebDoc,
    hits: (&Hit, &Hit),
    chains: (&[DocNodeId], &[DocNodeId]),
    ends: (DocNodeId, DocNodeId, DocNodeId),
    whole: (bool, bool),
) -> Vec<SelSpan<DocNodeId>> {
    let (f, l) = hits;
    let (chain_f, chain_l) = chains;
    let (c, x_s, x_e) = ends;
    let (whole_start, whole_end) = whole;
    let f_par = chain_f[0];
    let l_par = chain_l[0];
    let one = (1, 1);
    let mut out: Vec<SelSpan<DocNodeId>> = Vec::new();
    // Start branch: the tail of the first atom's container, then the tail of each ancestor
    // below the common one.
    if f_par != c && !whole_start {
        let s0 = add(pos(d, f_par, f.id), f.off(d, f.from));
        push_span(d, f_par, s0, hyper_len(d, f_par), &mut out);
        let mut child = f_par;
        for a in chain_f.iter().skip(1).take_while(|a| **a != c) {
            push_span(
                d,
                *a,
                add(pos(d, *a, child), one),
                hyper_len(d, *a),
                &mut out,
            );
            child = *a;
        }
    }
    // Between: from after the start side to before the end side, in the common ancestor.
    let mid_s = if f_par == c {
        add(pos(d, c, f.id), f.off(d, f.from))
    } else if whole_start {
        pos(d, c, x_s)
    } else {
        add(pos(d, c, x_s), one)
    };
    let mid_e = if l_par == c {
        add(pos(d, c, l.id), l.off(d, l.to))
    } else if whole_end {
        add(pos(d, c, x_e), one)
    } else {
        pos(d, c, x_e)
    };
    push_span(d, c, mid_s, mid_e, &mut out);
    // End branch, innermost first here and reversed so the spans stay in document order.
    let mut tails: Vec<SelSpan<DocNodeId>> = Vec::new();
    if l_par != c && !whole_end {
        let e0 = add(pos(d, l_par, l.id), l.off(d, l.to));
        push_span(d, l_par, (0, 0), e0, &mut tails);
        let mut child = l_par;
        for a in chain_l.iter().skip(1).take_while(|a| **a != c) {
            push_span(d, *a, (0, 0), pos(d, *a, child), &mut tails);
            child = *a;
        }
    }
    tails.reverse();
    out.extend(tails);
    out
}

/// Test helper: the end offset Gecko takes for a span (the same rule as the source's
/// `gecko_range`; kept in step by `v6_*`).
#[cfg(test)]
pub(crate) fn tests_gecko_end(sp: &SelSpan<DocNodeId>) -> u32 {
    if (sp.end_on_child || (sp.child_follows && sp.trim_ws)) && sp.range.end > sp.range.start {
        sp.range.end - 1
    } else {
        sp.range.end
    }
}
