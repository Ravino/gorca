//! Mirroring the virtual browse selection into the page (NVDA copies its own buffer; gorca lets
//! the browser copy, which keeps the formatting and needs no clipboard of ours).
//!
//! The virtual selection is a range of flat-text offsets. The page selects through AT-SPI
//! `Text.SetSelection` on a *hypertext*: a container's text with one U+FFFC for every embedded
//! child, offsets in UTF-16 units. The mirror splits that text into leaves (`StaticText` nodes),
//! so a flat range becomes spans `(container, start, end)` in those hypertext offsets.
//!
//! The spans here are engine-neutral: `end` is exclusive and exact. What an engine does with an
//! end offset that names an embedded child differs (Firefox 153 ESR ends the range after the child
//! when the end offset *is* the child's character; Blink takes it as exclusive), so each span
//! also says what its end touches (`SelSpan`), and the document source adapts it
//! (`gorca-doc-source-atspi`, measured in `docs/reports/ctrl-c-passthrough.md`).
//!
//! A range whose ends sit in different containers is planned two ways: `spans`, the exact legs
//! (tail of the start branch, whole blocks between, head of the end branch), and `snap`, one
//! range on the common ancestor that rounds nested ends out to whole children. The source
//! applies `spans` where the engine keeps several ranges and `snap` where it keeps one.

use gorca_doc::web::{NodeFlags, WebDoc};
use gorca_model::{DocNodeId, Role, SelSpan};

/// How to select a flat range in the page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    /// Exact ranges in document order; one for a selection inside a container.
    pub spans: Vec<SelSpan<DocNodeId>>,
    /// The single-range alternative on the common ancestor.
    pub snap: SelSpan<DocNodeId>,
    /// The container of the first leaf: when the engine focuses it, the selection steals focus.
    pub start_container: DocNodeId,
}

fn is_leaf(d: &WebDoc, id: DocNodeId) -> bool {
    d.node(id)
        .is_some_and(|v| v.role == Role::StaticText && v.text.is_some())
        && d.child_count(id) == Some(0)
}

fn units(c: char) -> u32 {
    u32::try_from(c.len_utf16()).unwrap_or(1)
}

fn utf16_prefix(t: &str, scalars: u32) -> u32 {
    t.chars().take(scalars as usize).map(units).sum()
}

/// What `id` adds to its parent's hypertext: its text for a leaf, one embedded character else.
fn contrib(d: &WebDoc, id: DocNodeId) -> u32 {
    match d.node(id) {
        Some(v) if is_leaf(d, id) => v.text.map_or(0, |t| t.chars().map(units).sum()),
        Some(_) | None => 1,
    }
}

/// Hypertext offset of the start of `child` in its parent.
fn pos(d: &WebDoc, parent: DocNodeId, child: DocNodeId) -> u32 {
    d.children(parent)
        .take_while(|c| *c != child)
        .map(|c| contrib(d, c))
        .sum()
}

fn hyper_len(d: &WebDoc, parent: DocNodeId) -> u32 {
    d.children(parent).map(|c| contrib(d, c)).sum()
}

/// The child of `p` whose hypertext contains offset `idx`, with its start offset.
fn child_at(d: &WebDoc, p: DocNodeId, idx: u32) -> Option<(DocNodeId, u32)> {
    let mut start = 0;
    for c in d.children(p) {
        let end = start + contrib(d, c);
        if idx < end {
            return Some((c, start));
        }
        start = end;
    }
    None
}

/// A span `start..end` of `node` with what its end touches.
fn span(d: &WebDoc, node: DocNodeId, start: u32, end: u32) -> SelSpan<DocNodeId> {
    let last = end.checked_sub(1).and_then(|i| child_at(d, node, i));
    let next = child_at(d, node, end);
    let end_on_child = last.is_some_and(|(c, _)| !is_leaf(d, c));
    let child_follows = next.is_some_and(|(c, _)| !is_leaf(d, c));
    let trim_ws = last.is_some_and(|(c, cs)| {
        is_leaf(d, c)
            && d.node(c).and_then(|v| v.text).is_some_and(|t| {
                let mut at = cs;
                let target = end - 1;
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
        range: start..end,
        end_on_child,
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

/// A text leaf met by the range, with the scalar offsets of the range inside its text.
struct Hit {
    id: DocNodeId,
    from: u32,
    to: u32,
    scalars: u32,
}

fn leaves(d: &WebDoc, lo: u32, hi: u32) -> Option<(Hit, Hit)> {
    let (n0, _) = d.at_offset(lo)?;
    let mut first: Option<Hit> = None;
    let mut last: Option<Hit> = None;
    for id in d.preorder_from(n0) {
        let start = d.offset_of(id)?;
        if start >= hi {
            break;
        }
        let v = d.node(id)?;
        if v.role == Role::PasswordEntry {
            // Never mirror (and so never copy) a password field: the range stops before it.
            break;
        }
        if v.flags.contains(NodeFlags::HIDDEN) || !is_leaf(d, id) {
            continue;
        }
        let text = v.text?;
        let lead = u32::from(
            v.flags.contains(NodeFlags::BREAK_BEFORE) || v.flags.contains(NodeFlags::CELL_SEP),
        );
        let n = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        let ts = start + lead;
        if ts + n <= lo {
            continue;
        }
        let from = lo.saturating_sub(ts).min(n);
        let to = hi.saturating_sub(ts).min(n);
        if to == 0 || from >= to {
            continue;
        }
        if first.is_none() {
            first = Some(Hit {
                id,
                from,
                to,
                scalars: n,
            });
        }
        last = Some(Hit {
            id,
            from,
            to,
            scalars: n,
        });
    }
    Some((first?, last?))
}

/// Plan the page selection of the flat range `[lo, hi)`. `None` when there is nothing to select.
/// A password field ends the range before it.
pub fn plan(d: &WebDoc, lo: u32, hi: u32) -> Option<Plan> {
    if lo >= hi {
        return None;
    }
    let (f, l) = leaves(d, lo, hi)?;
    let f_text = d.node(f.id)?.text?;
    let l_text = d.node(l.id)?.text?;
    let f_par = d.parent(f.id)?;
    let l_par = d.parent(l.id)?;
    let s_in = |p: DocNodeId| pos(d, p, f.id) + utf16_prefix(f_text, f.from);
    let e_in = |p: DocNodeId| pos(d, p, l.id) + utf16_prefix(l_text, l.to);

    if f_par == l_par {
        let sp = span(d, f_par, s_in(f_par), e_in(f_par));
        return Some(Plan {
            spans: vec![sp.clone()],
            snap: sp,
            start_container: f_par,
        });
    }

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
        pos(d, c, x_e) + 1
    };
    let snap = span(d, c, snap_s, snap_e);

    // Whole blocks at both ends: the rounded-out range is exact, and one range keeps the line
    // breaks between blocks (separate ranges are joined with nothing).
    let whole_start = f_par == c || (f.from == 0 && first_leaf(d, x_s) == Some(f.id));
    let whole_end = l_par == c || (l.to == l.scalars && last_leaf(d, x_e) == Some(l.id));
    let spans = if whole_start && whole_end {
        vec![snap.clone()]
    } else {
        exact_legs(
            d,
            &f,
            &l,
            (f_text, l_text),
            (&chain_f, &chain_l),
            (c, x_s, x_e),
            (whole_start, whole_end),
        )
    };
    Some(Plan {
        spans,
        snap,
        start_container: f_par,
    })
}

fn first_leaf(d: &WebDoc, from: DocNodeId) -> Option<DocNodeId> {
    let mut cur = from;
    loop {
        if is_leaf(d, cur) {
            return Some(cur);
        }
        cur = d.first_child(cur)?;
    }
}

fn last_leaf(d: &WebDoc, from: DocNodeId) -> Option<DocNodeId> {
    let mut cur = from;
    loop {
        if is_leaf(d, cur) {
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

fn push_span(d: &WebDoc, node: DocNodeId, s: u32, e: u32, out: &mut Vec<SelSpan<DocNodeId>>) {
    if s < e {
        out.push(span(d, node, s, e));
    }
}

/// The exact legs: the tail of the start branch, the blocks between, the head of the end branch.
fn exact_legs(
    d: &WebDoc,
    f: &Hit,
    l: &Hit,
    texts: (&str, &str),
    chains: (&[DocNodeId], &[DocNodeId]),
    ends: (DocNodeId, DocNodeId, DocNodeId),
    whole: (bool, bool),
) -> Vec<SelSpan<DocNodeId>> {
    let (f_text, l_text) = texts;
    let (chain_f, chain_l) = chains;
    let (c, x_s, x_e) = ends;
    let (whole_start, whole_end) = whole;
    let f_par = chain_f[0];
    let l_par = chain_l[0];
    let mut out: Vec<SelSpan<DocNodeId>> = Vec::new();
    // Start branch: the tail of the first leaf's container, then the tail of each ancestor
    // below the common one.
    if f_par != c && !whole_start {
        let s0 = pos(d, f_par, f.id) + utf16_prefix(f_text, f.from);
        push_span(d, f_par, s0, hyper_len(d, f_par), &mut out);
        let mut child = f_par;
        for a in chain_f.iter().skip(1).take_while(|a| **a != c) {
            push_span(d, *a, pos(d, *a, child) + 1, hyper_len(d, *a), &mut out);
            child = *a;
        }
    }
    // Between: from after the start side to before the end side, in the common ancestor.
    let mid_s = if f_par == c {
        pos(d, c, f.id) + utf16_prefix(f_text, f.from)
    } else if whole_start {
        pos(d, c, x_s)
    } else {
        pos(d, c, x_s) + 1
    };
    let mid_e = if l_par == c {
        pos(d, c, l.id) + utf16_prefix(l_text, l.to)
    } else if whole_end {
        pos(d, c, x_e) + 1
    } else {
        pos(d, c, x_e)
    };
    push_span(d, c, mid_s, mid_e, &mut out);
    // End branch, innermost first here and reversed so the spans stay in document order.
    let mut tails: Vec<SelSpan<DocNodeId>> = Vec::new();
    if l_par != c && !whole_end {
        let e0 = pos(d, l_par, l.id) + utf16_prefix(l_text, l.to);
        push_span(d, l_par, 0, e0, &mut tails);
        let mut child = l_par;
        for a in chain_l.iter().skip(1).take_while(|a| **a != c) {
            push_span(d, *a, 0, pos(d, *a, child), &mut tails);
            child = *a;
        }
    }
    tails.reverse();
    out.extend(tails);
    out
}
