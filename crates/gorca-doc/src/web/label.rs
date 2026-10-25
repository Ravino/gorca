// SPDX-License-Identifier: GPL-3.0-or-later
//! Label inference for a nameless form control: the text next to it that a sighted user reads as
//! its label (M22a C3b).
//!
//! A port of the rules of Orca's `label_inference.py` (`refs/orca/src/orca/label_inference.py`,
//! `_infer_from_text_left/right/above/below`, `infer`) and of the gate in
//! `scripts/web/script_utilities.py` (`_should_infer_label_for`), split in two pure halves so that
//! the cost stays bounded:
//!
//! * [`plan`] reads the mirror only (flat text and lines, no I/O): the text of the same line to
//!   the left and to the right of the control, and of the line above and below it, each with the
//!   one node whose position decides whether the text is close enough to be a label;
//! * [`choose`] applies Orca's geometry (distance limits, in the order left, right, above, below,
//!   then the wide left fallback) to the rectangles of the control and of those nodes. Getting
//!   the rectangles is one batched source request with a deadline; this module never asks.
//!
//! Not ported: Orca's table-cell branch (`_infer_from_table`; cell neighbours need grid
//! coordinates and layout tables are rare on forms that matter).
//!
//! The text is the page's own, so it is untrusted: it is only ever spoken, never interpreted.

use gorca_model::sem::SemRole;
use gorca_model::{DocNodeId, Rect};

use super::cats::{Cat, NodeFlags};
use super::doc::WebDoc;

/// Orca's proximity limits, in pixels (`label_inference.py` defaults).
const NEAR_LEFT: i64 = 75;
const NEAR_RIGHT: i64 = 25;
const NEAR_VERTICAL: i64 = 20;
/// Boxes of adjacent lines may overlap by a pixel or two (Chromium rounds line boxes outward);
/// Orca's `0 <= distance` allows for it here.
const SLACK: i64 = 3;
/// "Desperate times call for desperate measures": the left rule again, wider.
const FAR_LEFT: i64 = 200;

/// Most nodes read on one side of the control (a line of a long paragraph is not a label).
const SIDE_NODES: usize = 24;
/// Longest label kept, in characters.
const LABEL_CHARS: usize = 160;
/// Ancestors looked at when asking whether a text node belongs to a widget or a heading.
const CONTEXT_HOPS: usize = 6;

/// Where the label text sits relative to the control.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

/// One piece of text that may label the control, and the node whose rectangle places it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Cand {
    pub side: Side,
    pub text: String,
    /// Left: the text node nearest the control; right: the nearest on that side; above and
    /// below: the one text node of that line.
    pub probe: DocNodeId,
}

/// What [`plan`] found for one control.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Plan {
    pub ctl: DocNodeId,
    pub role: SemRole,
    pub cands: Vec<Cand>,
}

impl Plan {
    /// The nodes whose rectangles [`choose`] needs: the control first, then each candidate's probe.
    pub fn probes(&self) -> Vec<DocNodeId> {
        let mut v = vec![self.ctl];
        v.extend(self.cands.iter().map(|c| c.probe));
        v
    }
}

/// Roles Orca infers a label for (`_should_infer_label_for`): check box, combo box, entry,
/// list box, password entry, radio button. A text control reached through a nameless
/// `contenteditable` is a text entry by then (the presenter turns it into one).
pub fn infers_for(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::Checkbox
            | SemRole::Combobox
            | SemRole::TextBox
            | SemRole::SearchBox
            | SemRole::PasswordBox
            | SemRole::Radio
            | SemRole::List
            | SemRole::Listbox
    )
}

fn prefers_right(role: SemRole) -> bool {
    matches!(role, SemRole::Checkbox | SemRole::Radio)
}

fn prevents_right(role: SemRole) -> bool {
    matches!(role, SemRole::Combobox | SemRole::List | SemRole::Listbox)
}

fn allows_below(role: SemRole) -> bool {
    matches!(
        role,
        SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox
    )
}

/// A widget, or something that is never a label (Orca `_cannot_label`): a control, a heading, a
/// list item, on the node or a few ancestors up.
fn cannot_label(d: &WebDoc, id: DocNodeId) -> bool {
    let widget = Cat::FORM_FIELD
        | Cat::EDIT
        | Cat::BUTTON
        | Cat::COMBOBOX
        | Cat::CHECKBOX
        | Cat::RADIO
        | Cat::SLIDER_SPIN
        | Cat::LISTBOX;
    let mut cur = Some(id);
    for _ in 0..CONTEXT_HOPS {
        let Some(n) = cur else { return false };
        let Some(v) = d.node(n) else { return false };
        if v.cats.intersects(widget)
            || v.cats.intersects(Cat::HEADING)
            || matches!(
                v.role,
                SemRole::ListItem | SemRole::Option | SemRole::Heading
            )
        {
            return true;
        }
        cur = d.parent(n);
    }
    false
}

/// Text nodes that start inside `[a, b)` of the flat text, in document order, each with the
/// part of its text inside the range. Hidden nodes and line breaks are skipped; so are nodes
/// without text unless they are widgets or headings (those end the label text on their side).
fn items(d: &WebDoc, a: u32, b: u32) -> Vec<(DocNodeId, String)> {
    let mut out = Vec::new();
    if a >= b {
        return out;
    }
    let Some((first, _)) = d.at_offset(a) else {
        return out;
    };
    for id in d.preorder_from(first) {
        if out.len() >= SIDE_NODES {
            break;
        }
        let Some(off) = d.offset_of(id) else { break };
        if off >= b {
            break;
        }
        let Some(v) = d.node(id) else { continue };
        if v.flags.contains(NodeFlags::HIDDEN) {
            continue;
        }
        let own = d.own_len(id).unwrap_or(0);
        let lo = a.max(off);
        let hi = b.min(off.saturating_add(own));
        if lo >= hi {
            continue;
        }
        let t = d.text_in(lo, hi);
        let t = t.trim_matches(|c: char| c == '\n' || c == '\u{FFFC}');
        // A widget with no text of its own still ends the text on its side: it is kept, empty.
        if !t.trim().is_empty() || cannot_label(d, id) {
            out.push((id, t.to_owned()));
        }
    }
    out
}

/// Where the text node `id` sits in its parent's text, as scalar offsets, assuming the Firefox
/// shape (the parent holds the text of its runs with one object marker per real child):
/// `(parent, start, end)`. `None` for a node without a parent.
pub fn hyper_range(d: &WebDoc, id: DocNodeId) -> Option<(DocNodeId, u32, u32)> {
    let parent = d.parent(id)?;
    let mut at = 0u32;
    for c in d.children(parent) {
        let v = d.node(c)?;
        let is_run = v.role.is_inline_text() && d.child_count(c) == Some(0);
        let len = if is_run {
            u32::try_from(v.text.map_or(0, |t| t.chars().count())).ok()?
        } else {
            1
        };
        if c == id {
            return Some((parent, at, at.saturating_add(len)));
        }
        at = at.saturating_add(len);
    }
    None
}

/// Collapse white space, drop object markers and icon glyphs, cap the length.
fn clean(s: &str) -> Option<String> {
    let kept: String = s
        .chars()
        .filter(|c| {
            !gorca_model::text::is_icon_glyph(*c) && !gorca_model::text::is_object_marker(*c)
        })
        .collect();
    let one = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let one: String = one.chars().take(LABEL_CHARS).collect();
    (!one.is_empty()).then_some(one)
}

/// What the mirror knows about the text around the control `ctl` (its role is `role`). `None`
/// when the control is not in the document or has no position in the text.
pub fn plan(d: &WebDoc, ctl: DocNodeId, role: SemRole) -> Option<Plan> {
    let (s, e) = d.subtree_text_range(ctl)?;
    // The control's own contribution starts after the line breaks its flags add.
    let lead = d
        .text_in(s, e)
        .chars()
        .take_while(|c| *c == '\n' || *c == ' ')
        .count();
    let body = s.saturating_add(u32::try_from(lead).ok()?).min(e);
    let (ls, le) = d.line_range(body);
    let mut cands = Vec::new();

    // Left: the text after the last widget before the control on the same line.
    let before = items(d, ls, s.max(ls));
    let start = before
        .iter()
        .rposition(|(id, _)| cannot_label(d, *id))
        .map_or(0, |i| i + 1);
    let left = &before[start..];
    if let Some((probe, _)) = left.last() {
        let text = left.iter().map(|(_, t)| t.as_str()).collect::<String>();
        if let Some(text) = clean(&text) {
            cands.push(Cand {
                side: Side::Left,
                text,
                probe: *probe,
            });
        }
    }

    // Right: the text up to the first widget after the control (check boxes and radio buttons
    // take all of it up to the widget; others only a close first node, checked by geometry).
    if !prevents_right(role) {
        let after = items(d, e.min(le), le);
        let end = after
            .iter()
            .position(|(id, _)| cannot_label(d, *id))
            .unwrap_or(after.len());
        let right = &after[..end];
        if let Some((probe, _)) = right.first()
            && (end == after.len() || prefers_right(role))
        {
            let text = right.iter().map(|(_, t)| t.as_str()).collect::<String>();
            if let Some(text) = clean(&text) {
                cands.push(Cand {
                    side: Side::Right,
                    text,
                    probe: *probe,
                });
            }
        }
    }

    // Above and below: the control starts its line, the neighbouring line is one text node.
    let first_on_line = items(d, ls, s.max(ls)).is_empty();
    if first_on_line
        && ls > 0
        && let Some(c) = one_line(d, ls.saturating_sub(1), Side::Above)
    {
        cands.push(c);
    }
    if first_on_line && allows_below(role) {
        let (_, le2) = d.line_range(body);
        if le2 < d.text_len()
            && let Some(c) = one_line(d, le2.saturating_add(1), Side::Below)
        {
            cands.push(c);
        }
    }

    (!cands.is_empty()).then_some(Plan { ctl, role, cands })
}

/// The line containing `offset` as a candidate when it is one text node of plain text.
fn one_line(d: &WebDoc, offset: u32, side: Side) -> Option<Cand> {
    let (a, b) = d.line_range(offset);
    let its = items(d, a, b);
    let [(probe, text)] = its.as_slice() else {
        return None;
    };
    if cannot_label(d, *probe) {
        return None;
    }
    Some(Cand {
        side,
        text: clean(text)?,
        probe: *probe,
    })
}

fn right_edge(r: Rect) -> i64 {
    i64::from(r.x) + i64::from(r.w)
}

fn bottom_edge(r: Rect) -> i64 {
    i64::from(r.y) + i64::from(r.h)
}

/// The two rectangles share some height: the text is on the control's line.
fn same_row(a: Rect, b: Rect) -> bool {
    i64::from(a.y) < bottom_edge(b) && i64::from(b.y) < bottom_edge(a)
}

fn usable(r: Rect) -> bool {
    r.w > 0 && r.h > 0
}

/// Orca's geometry over the plan (`infer`): the first side that is close enough wins, in the order left
/// (75 px), right (25 px, any distance for check boxes and radio buttons, which prefer it),
/// above (20 px, not to the right of the control), below (20 px, entries only), then the left
/// text again within 200 px. `rects` is parallel to [`Plan::probes`]; a missing rectangle
/// disqualifies the candidate (the control's own makes the whole plan fail).
pub fn choose(plan: &Plan, rects: &[Option<Rect>]) -> Option<String> {
    let ctl = rects.first().copied().flatten().filter(|r| usable(*r))?;
    let rect_of = |i: usize| rects.get(i + 1).copied().flatten().filter(|r| usable(*r));
    let find = |side: Side| {
        plan.cands
            .iter()
            .enumerate()
            .find(|(_, c)| c.side == side)
            .and_then(|(i, c)| rect_of(i).map(|r| (c, r)))
    };
    let left = |limit: i64| {
        find(Side::Left).and_then(|(c, r)| {
            let dist = i64::from(ctl.x) - right_edge(r);
            (r.x <= ctl.x && (-SLACK..=limit).contains(&dist) && same_row(ctl, r))
                .then(|| c.text.clone())
        })
    };
    let right = || {
        find(Side::Right).and_then(|(c, r)| {
            let dist = i64::from(r.x) - right_edge(ctl);
            ((dist <= NEAR_RIGHT || prefers_right(plan.role)) && same_row(ctl, r))
                .then(|| c.text.clone())
        })
    };

    let mut found = left(NEAR_LEFT);
    if found.is_none() || prefers_right(plan.role) {
        // A right-hand label overrides the left one for check boxes and radio buttons.
        if let Some(r) = right() {
            found = Some(r);
        }
    }
    if found.is_none()
        && let Some((c, r)) = find(Side::Above)
    {
        let dist = i64::from(ctl.y) - bottom_edge(r);
        if (-SLACK..=NEAR_VERTICAL).contains(&dist) && r.x <= ctl.x {
            found = Some(c.text.clone());
        }
    }
    if found.is_none()
        && allows_below(plan.role)
        && let Some((c, r)) = find(Side::Below)
    {
        let dist = i64::from(r.y) - bottom_edge(ctl);
        if (-SLACK..=NEAR_VERTICAL).contains(&dist) {
            found = Some(c.text.clone());
        }
    }
    if found.is_none() {
        found = left(FAR_LEFT);
    }
    found
}

#[cfg(test)]
mod tests;
