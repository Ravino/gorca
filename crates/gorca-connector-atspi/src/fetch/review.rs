//! Review operations of the connector (ADR-0006 15.1): window-relative extents on snapshots,
//! visible text lines, actions for `review.activate` / `review.move_focus`, and the status-bar
//! `Collection` search.
//!
//! Everything here is a **post-process or an addition** to the strategy: bounds are attached to
//! the snapshots a strategy returned (so a fill strategy that rewrites `strategy.rs` does not
//! conflict), and the text, action and collection ops are separate `BackendOp`s. All of it is
//! app-supplied data and untrusted: rectangles are range-checked, line walks are bounded and
//! always terminate, refs to other bus names are dropped.

use std::collections::HashMap;
use std::sync::Arc;

use atspi::State as Raw;
use gorca_connector_api::{ActionReq, BackendError, BackendReply, MatchRule};
use gorca_model::{
    NodeKey, NodeSnapshot, PropMask, Rect, Role, StateSet, TextLine, TextLines, TextQuery,
    text::clamp_range,
};
use gorca_rt::Deadline;
use tokio::task::JoinSet;

use super::caller::WireRule;
use super::{CallCx, Resolved, backend_error, resolve};
use crate::decode::clip;
use crate::role_map::map_role;

/// Extents are fetched for at most this many nodes of one reply (one reply is at most a
/// `ChildSnapshots` range or an `Ancestry` chain).
pub const MAX_BOUNDS_BATCH: usize = 256;
/// A coordinate or size beyond this is a lie (no screen is a million pixels wide).
pub const COORD_LIMIT: i32 = 1_000_000;
/// Hard bounds of `VisibleLines`, whatever the caller asked (ADR-0006 6.8).
pub const MAX_LINES: u16 = 200;
pub const MAX_LINE_BYTES: u32 = 64 * 1024;
/// `GetText` replies are cut here.
const MAX_GET_TEXT_BYTES: usize = 64 * 1024;
/// Collection answers at most this many nodes.
pub const MAX_MATCHES: u32 = 64;
/// Role slots of a wire match rule (`rule->roles[5]` in at-spi2-core).
const RULE_ROLE_SLOTS: usize = 5;

const MATCH_ALL: i32 = 1;
const MATCH_ANY: i32 = 2;
const MATCH_NONE: i32 = 3;
const MATCH_EMPTY: i32 = 4;

/// A rectangle from the wire, or `None` when it cannot be real: not positive, `(0,0,0,0)` or
/// beyond [`COORD_LIMIT`] (ADR-0006 6.7).
pub fn sane_rect(x: i32, y: i32, w: i32, h: i32) -> Option<Rect> {
    if w <= 0 || h <= 0 {
        return None;
    }
    if [x, y, w, h]
        .iter()
        .any(|v| v.saturating_abs() > COORD_LIMIT)
    {
        return None;
    }
    Some(Rect {
        x,
        y,
        w: u32::try_from(w).ok()?,
        h: u32::try_from(h).ok()?,
    })
}

fn is_timeout(e: &gorca_dbus::DbusError) -> bool {
    matches!(backend_error(e), BackendError::Timeout)
}

type ExtentsAnswer = (usize, Result<(i32, i32, i32, i32), gorca_dbus::DbusError>);

/// Attach `Component.GetExtents(WINDOW)` to the snapshots of one reply (at most
/// [`MAX_BOUNDS_BATCH`]), all calls pipelined under the request deadline. `have` gains `BOUNDS`
/// once the application answered at all (even with an error or a bogus rectangle), so the core
/// does not ask again; a timed-out call leaves the node without `BOUNDS`.
pub(super) async fn add_bounds(cx: &CallCx, nodes: &mut [Arc<NodeSnapshot>], dl: Deadline) {
    let mut set: JoinSet<ExtentsAnswer> = JoinSet::new();
    for (i, s) in nodes.iter().take(MAX_BOUNDS_BATCH).enumerate() {
        let Some(path) = cx.paths.get(s.key) else {
            continue;
        };
        let caller = cx.caller.clone();
        set.spawn(async move { (i, caller.extents_window(&path, dl).await) });
    }
    while let Some(done) = set.join_next().await {
        let Ok((i, r)) = done else { continue };
        let answered = match &r {
            Ok(_) => true,
            Err(e) => !is_timeout(e),
        };
        if !answered {
            continue;
        }
        let Some(slot) = nodes.get_mut(i) else {
            continue;
        };
        let node = Arc::make_mut(slot);
        node.bounds = r.ok().and_then(|(x, y, w, h)| sane_rect(x, y, w, h));
        node.have |= PropMask::BOUNDS;
    }
}

// ---- text -------------------------------------------------------------------------------------

/// The pure walk behind `VisibleLines`. The driver asks [`LineWalk::next_offset`], calls the
/// application for the line at that offset, and feeds the answer. Whatever the application says
/// (negative offsets, overlapping or non-advancing lines, a huge count), the walk ends within
/// `4 * max_lines + 16` steps.
#[derive(Debug)]
pub struct LineWalk {
    clip: Rect,
    max_lines: usize,
    max_bytes: usize,
    count: i32,
    offset: i32,
    lines: Vec<TextLine>,
    bytes: usize,
    steps: usize,
    max_steps: usize,
    truncated: bool,
    done: bool,
}

impl LineWalk {
    pub fn new(clip: Rect, max_lines: u16, max_bytes: u32, count: i32, start: i32) -> Self {
        let max_lines = usize::from(max_lines.min(MAX_LINES));
        let count = count.max(0);
        let start = if (0..count).contains(&start) {
            start
        } else {
            0
        };
        Self {
            clip,
            max_lines,
            max_bytes: usize::try_from(max_bytes.min(MAX_LINE_BYTES)).unwrap_or(0),
            count,
            offset: start,
            lines: Vec::new(),
            bytes: 0,
            steps: 0,
            max_steps: max_lines * 4 + 16,
            truncated: false,
            done: count == 0 || max_lines == 0,
        }
    }

    /// The offset whose line to ask for next, or `None` when the walk is over.
    pub fn next_offset(&self) -> Option<i32> {
        (!self.done).then_some(self.offset)
    }

    /// The application could not answer; the walk stops with what it has.
    pub fn abort(&mut self) {
        if !self.done {
            self.truncated = true;
            self.done = true;
        }
    }

    fn bottom(&self) -> i64 {
        i64::from(self.clip.y) + i64::from(self.clip.h)
    }

    /// Feed the answer for [`LineWalk::next_offset`]: the line text, its `[start, end)` offsets
    /// and its extents (`None` if missing or bogus).
    pub fn feed(&mut self, text: &str, start: i32, end: i32, rect: Option<Rect>) {
        if self.done {
            return;
        }
        self.steps += 1;
        if self.steps > self.max_steps {
            self.abort();
            return;
        }
        // No progress is the one thing that must never loop.
        if end <= self.offset {
            self.done = true;
            return;
        }
        let end_c = end.min(self.count);
        if let Some(r) = rect
            && self.clip.h > 0
        {
            if i64::from(r.y) >= self.bottom() {
                self.done = true;
                return;
            }
            if i64::from(r.y) + i64::from(r.h) <= i64::from(self.clip.y) {
                // Above the clip: skipped, not counted.
                self.offset = end_c;
                self.done = end_c >= self.count;
                return;
            }
        }
        if self.lines.len() >= self.max_lines {
            self.truncated = true;
            self.done = true;
            return;
        }
        let text = text.trim_end_matches(['\n', '\r']);
        if self.bytes + text.len() > self.max_bytes {
            self.truncated = true;
            self.done = true;
            return;
        }
        self.bytes += text.len();
        let s = start.max(0).min(end_c.saturating_sub(1));
        self.lines.push(TextLine {
            range: u32::try_from(s).unwrap_or(0)..u32::try_from(end_c).unwrap_or(0),
            text: Arc::from(text),
            rect,
        });
        self.offset = end_c;
        if end_c >= self.count {
            self.done = true;
        }
    }

    pub fn finish(self) -> TextLines {
        TextLines {
            lines: self.lines,
            truncated: self.truncated,
        }
    }
}

async fn char_count(cx: &CallCx, n: &Resolved, dl: Deadline) -> Result<i32, BackendError> {
    cx.caller
        .text_char_count(&n.path, dl)
        .await
        .map(|c| c.max(0))
        .map_err(|e| backend_error(&e))
}

/// `Text` queries. Offsets are scalar offsets (TU-1), which is what GTK, St and the fake app
/// speak.
pub(super) async fn text(
    cx: &CallCx,
    q: &TextQuery,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    match q {
        TextQuery::GetText { node, range } => {
            let n = resolve(cx, *node)?;
            let count = char_count(cx, &n, dl).await?;
            let r = clamp_range(range.clone(), u32::try_from(count).unwrap_or(0));
            let (s, e) = (
                i32::try_from(r.start).unwrap_or(0),
                i32::try_from(r.end).unwrap_or(0),
            );
            if s >= e {
                return Ok(BackendReply::Text(Arc::from("")));
            }
            let t = cx
                .caller
                .text_range(&n.path, s, e, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            Ok(BackendReply::Text(Arc::from(clip(&t, MAX_GET_TEXT_BYTES))))
        }
        TextQuery::CaretOffset { node } => {
            let n = resolve(cx, *node)?;
            let c = cx
                .caller
                .text_caret(&n.path, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            Ok(BackendReply::CaretOffset(
                u32::try_from(c.max(0)).unwrap_or(0),
            ))
        }
        TextQuery::Attributes { .. } => Err(BackendError::NotSupported),
        TextQuery::VisibleLines {
            node,
            clip,
            max_lines,
            max_bytes,
        } => {
            let n = resolve(cx, *node)?;
            visible_lines(cx, &n, *clip, *max_lines, *max_bytes, dl)
                .await
                .map(BackendReply::Lines)
        }
    }
}

async fn visible_lines(
    cx: &CallCx,
    n: &Resolved,
    clip_rect: Rect,
    max_lines: u16,
    max_bytes: u32,
    dl: Deadline,
) -> Result<TextLines, BackendError> {
    let c = &cx.caller;
    let count = char_count(cx, n, dl).await?;
    if count == 0 {
        return Ok(TextLines::default());
    }
    // One probe tells a toolkit with extents from GTK4's editables, which answer
    // `NotSupported` (F-3). Anything but a timeout selects the whole-text fallback.
    let probe = c.range_extents(&n.path, 0, 1.min(count), dl).await;
    let usable = match &probe {
        Ok((x, y, w, h)) => sane_rect(*x, *y, *w, *h).is_some(),
        Err(e) if is_timeout(e) => return Err(BackendError::Timeout),
        Err(_) => false,
    };
    if !usable {
        return whole_text(cx, n, count, max_bytes, dl).await;
    }
    let start = c
        .offset_at_point(&n.path, clip_rect.x, clip_rect.y, dl)
        .await
        .unwrap_or(0);
    let mut walk = LineWalk::new(clip_rect, max_lines, max_bytes, count, start);
    while let Some(off) = walk.next_offset() {
        let (text, s, e) = match c.line_at(&n.path, off, dl).await {
            Ok(l) => l,
            Err(e) if is_timeout(&e) && walk.lines.is_empty() => {
                return Err(BackendError::Timeout);
            }
            Err(_) => {
                walk.abort();
                break;
            }
        };
        let rect = c
            .range_extents(&n.path, s, e, dl)
            .await
            .ok()
            .and_then(|(x, y, w, h)| sane_rect(x, y, w, h));
        walk.feed(&text, s, e, rect);
    }
    Ok(walk.finish())
}

/// One line: the whole text at the node's extents (single-line editables, F-3).
async fn whole_text(
    cx: &CallCx,
    n: &Resolved,
    count: i32,
    max_bytes: u32,
    dl: Deadline,
) -> Result<TextLines, BackendError> {
    let c = &cx.caller;
    let cap_bytes = usize::try_from(max_bytes.min(MAX_LINE_BYTES)).unwrap_or(0);
    let want_end = count.min(i32::try_from(cap_bytes).unwrap_or(i32::MAX));
    let (t, ext) = tokio::join!(
        c.text_range(&n.path, 0, want_end, dl),
        c.extents_window(&n.path, dl)
    );
    let t = t.map_err(|e| backend_error(&e))?;
    let clipped = clip(&t, cap_bytes);
    let rect = ext.ok().and_then(|(x, y, w, h)| sane_rect(x, y, w, h));
    let shown = u32::try_from(clipped.chars().count()).unwrap_or(0);
    Ok(TextLines {
        lines: vec![TextLine {
            range: 0..shown,
            text: Arc::from(clipped.trim_end_matches(['\n', '\r'])),
            rect,
        }],
        truncated: shown < u32::try_from(count).unwrap_or(0),
    })
}

// ---- actions ----------------------------------------------------------------------------------

fn refused(done: bool) -> Result<BackendReply, BackendError> {
    if done {
        Ok(BackendReply::ActionDone)
    } else {
        Err(BackendError::NotSupported)
    }
}

/// `review.activate`, `review.move_focus` and caret placement. Not allowed in shadow mode.
pub(super) async fn action(
    cx: &CallCx,
    req: &ActionReq,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let c = &cx.caller;
    match req {
        ActionReq::DoDefault { node } => {
            let n = resolve(cx, *node)?;
            let actions = c
                .n_actions(&n.path, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            if actions <= 0 {
                return Err(BackendError::NotSupported);
            }
            let ok = c
                .do_action(&n.path, 0, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            refused(ok)
        }
        ActionReq::Focus { node } => {
            let n = resolve(cx, *node)?;
            let ok = c
                .grab_focus(&n.path, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            refused(ok)
        }
        ActionReq::Select { node } => {
            let n = resolve(cx, *node)?;
            let (parent, index) =
                tokio::join!(c.parent_ref(&n.path, dl), c.index_in_parent(&n.path, dl));
            let parent = parent
                .map_err(|e| backend_error(&e))?
                .and_then(|(bus, p)| cx.resolve_ref(&bus, &p))
                .ok_or(BackendError::NotSupported)?;
            let index = index.map_err(|e| backend_error(&e))?;
            if index < 0 {
                return Err(BackendError::NotSupported);
            }
            let ok = c
                .select_child(&parent.path, index, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            refused(ok)
        }
        ActionReq::SetCaret { node, offset } => {
            let n = resolve(cx, *node)?;
            let off = i32::try_from(*offset).unwrap_or(i32::MAX);
            let ok = c
                .set_caret(&n.path, off, dl)
                .await
                .map_err(|e| backend_error(&e))?;
            refused(ok)
        }
        ActionReq::ScrollTo { .. } | ActionReq::SetValue { .. } => Err(BackendError::NotSupported),
    }
}

// ---- collection -------------------------------------------------------------------------------

/// Every AT-SPI role that normalises to `role` (the reverse of `role_map`).
pub fn raw_roles(role: Role) -> Vec<i32> {
    (0u32..=160)
        .filter_map(|n| atspi::Role::try_from(n).ok().map(|r| (n, r)))
        .filter(|(_, r)| map_role(*r) == role)
        .filter_map(|(n, _)| i32::try_from(n).ok())
        .collect()
}

/// The AT-SPI state bits of the normalised states a match rule can name.
pub fn raw_state_bits(s: StateSet) -> u64 {
    let table = [
        (Raw::Focused, StateSet::FOCUSED),
        (Raw::Focusable, StateSet::FOCUSABLE),
        (Raw::Selected, StateSet::SELECTED),
        (Raw::Checked, StateSet::CHECKED),
        (Raw::Expanded, StateSet::EXPANDED),
        (Raw::Editable, StateSet::EDITABLE),
        (Raw::Showing, StateSet::SHOWING),
        (Raw::Visible, StateSet::VISIBLE),
        (Raw::Active, StateSet::ACTIVE),
        (Raw::Enabled, StateSet::ENABLED),
        (Raw::Selectable, StateSet::SELECTABLE),
    ];
    table
        .iter()
        .filter(|(_, core)| s.contains(*core))
        .fold(0u64, |acc, (raw, _)| acc | (*raw as u64))
}

/// A normalised match rule on the wire. Roles match ANY of the listed roles (at most five slots),
/// states match ALL of `states_all`; with only `states_none` the states must match NONE.
pub fn wire_rule(rule: &MatchRule) -> WireRule {
    let mut roles: Vec<i32> = rule.roles.iter().flat_map(|r| raw_roles(*r)).collect();
    roles.truncate(RULE_ROLE_SLOTS);
    let role_type = if roles.is_empty() {
        MATCH_EMPTY
    } else {
        MATCH_ANY
    };
    roles.resize(RULE_ROLE_SLOTS, 0);
    let all = raw_state_bits(rule.states_all);
    let none = raw_state_bits(rule.states_none);
    let (bits, state_type) = if all != 0 {
        (all, MATCH_ALL)
    } else if none != 0 {
        (none, MATCH_NONE)
    } else {
        (0, MATCH_EMPTY)
    };
    // The two int32 words carry the low and high halves of the 64-bit set (a bit pattern).
    let lo = u32::try_from(bits & 0xffff_ffff).unwrap_or(0);
    let hi = u32::try_from(bits >> 32).unwrap_or(0);
    let words = vec![lo.cast_signed(), hi.cast_signed()];
    (
        words,
        state_type,
        HashMap::new(),
        MATCH_EMPTY,
        roles,
        role_type,
        Vec::new(),
        MATCH_EMPTY,
        false,
    )
}

/// `Collection.GetMatches` under `rule.root`, same-bus results only.
pub(super) async fn collection(
    cx: &CallCx,
    rule: &MatchRule,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let root = resolve(cx, rule.root)?;
    let count = rule.max_results.clamp(1, MAX_MATCHES);
    let refs = cx
        .caller
        .get_matches(
            &root.path,
            wire_rule(rule),
            i32::try_from(count).unwrap_or(1),
            dl,
        )
        .await
        .map_err(|e| backend_error(&e))?;
    let keys: Vec<NodeKey> = refs
        .into_iter()
        .take(count as usize)
        .filter_map(|(bus, path)| cx.key_of_ref(&bus, &path))
        .collect();
    Ok(BackendReply::Matches(keys))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn clip_rect() -> Rect {
        Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 100,
        }
    }

    fn r(y: i32) -> Option<Rect> {
        sane_rect(0, y, 100, 16)
    }

    #[test]
    fn bogus_rectangles_are_missing() {
        assert!(sane_rect(0, 0, 0, 0).is_none());
        assert!(sane_rect(1, 1, -5, 10).is_none());
        assert!(sane_rect(1, 1, 5, 0).is_none());
        assert!(sane_rect(2_000_000, 0, 10, 10).is_none());
        assert!(sane_rect(0, 0, i32::MAX, 10).is_none());
        assert!(sane_rect(i32::MIN, 0, 10, 10).is_none());
        assert_eq!(
            sane_rect(-4, 3, 10, 20),
            Some(Rect {
                x: -4,
                y: 3,
                w: 10,
                h: 20
            })
        );
    }

    #[test]
    fn a_normal_walk_collects_lines_until_the_clip_bottom() {
        let mut w = LineWalk::new(clip_rect(), 50, 10_000, 100, 0);
        let mut off = 0;
        let mut i = 0;
        while let Some(o) = w.next_offset() {
            assert_eq!(o, off);
            w.feed("line\n", off, off + 5, r(i * 16));
            off += 5;
            i += 1;
        }
        let t = w.finish();
        // 100 px clip / 16 px lines: lines at y = 0..=96 intersect (7 lines), the 8th starts at 112.
        assert_eq!(t.lines.len(), 7);
        assert_eq!(&*t.lines[0].text, "line", "trailing newline is trimmed");
        assert_eq!(t.lines[1].range, 5..10);
        assert!(!t.truncated);
    }

    #[test]
    fn lines_above_the_clip_are_skipped_without_counting() {
        let clip = Rect {
            x: 0,
            y: 50,
            w: 400,
            h: 40,
        };
        let mut w = LineWalk::new(clip, 2, 1000, 1000, 0);
        let mut off = 0;
        let mut i = 0;
        while w.next_offset().is_some() {
            w.feed("x", off, off + 1, r(i * 16));
            off += 1;
            i += 1;
        }
        let t = w.finish();
        assert!(!t.lines.is_empty());
        assert!(
            t.lines
                .iter()
                .all(|l| l.rect.is_some_and(|r| r.y + 16 > 50))
        );
        assert!(t.lines.len() <= 2);
    }

    #[test]
    fn max_lines_and_max_bytes_truncate() {
        let mut w = LineWalk::new(clip_rect(), 3, 10_000, 1000, 0);
        let mut off = 0;
        while w.next_offset().is_some() {
            w.feed("abc", off, off + 3, None);
            off += 3;
        }
        let t = w.finish();
        assert_eq!(t.lines.len(), 3);
        assert!(t.truncated);

        let mut w = LineWalk::new(clip_rect(), 100, 8, 1000, 0);
        let mut off = 0;
        while w.next_offset().is_some() {
            w.feed("abc", off, off + 3, None);
            off += 3;
        }
        let t = w.finish();
        assert_eq!(t.lines.len(), 2, "6 bytes fit, the third line would make 9");
        assert!(t.truncated);
    }

    #[test]
    fn a_walk_never_loops_on_non_advancing_lines() {
        let mut w = LineWalk::new(clip_rect(), 200, 1 << 20, 50, 0);
        let mut n = 0;
        while w.next_offset().is_some() {
            n += 1;
            assert!(n < 1000, "must terminate");
            // The app keeps answering the same line.
            w.feed("same", 0, 0, None);
        }
        let mut w = LineWalk::new(clip_rect(), 200, 1 << 20, 50, 0);
        w.feed("a", 0, 5, None);
        assert_eq!(w.next_offset(), Some(5));
        w.feed("a", 0, 5, None); // end <= offset: stop
        assert_eq!(w.next_offset(), None);
    }

    #[test]
    fn hostile_replies_stay_bounded() {
        let hostile: &[(i32, i32)] = &[
            (-1, -1),
            (i32::MIN, i32::MAX),
            (5, 3),
            (0, i32::MAX),
            (i32::MAX, i32::MAX),
            (-100, 1),
        ];
        for count in [0, 1, 10, i32::MAX, -5] {
            for start in [i32::MIN, -1, 0, 3, i32::MAX] {
                let mut w = LineWalk::new(clip_rect(), 10, 4096, count, start);
                let mut steps = 0usize;
                while w.next_offset().is_some() {
                    let (s, e) = hostile[steps % hostile.len()];
                    w.feed(
                        "hostile\n",
                        s,
                        e,
                        sane_rect(-5, i32::try_from(steps).unwrap(), 10, 10),
                    );
                    steps += 1;
                    assert!(steps < 100, "{count} {start}");
                }
                let t = w.finish();
                assert!(t.lines.len() <= 10);
            }
        }
    }

    #[test]
    fn huge_count_clamps_ranges_to_the_count() {
        let mut w = LineWalk::new(clip_rect(), 5, 1000, 4, 0);
        w.feed("abcdefgh", 0, 100, None);
        let t = w.finish();
        assert_eq!(t.lines[0].range, 0..4);
    }

    #[test]
    fn abort_marks_truncation_once() {
        let mut w = LineWalk::new(clip_rect(), 5, 1000, 40, 0);
        w.feed("a", 0, 1, None);
        w.abort();
        assert_eq!(w.next_offset(), None);
        let t = w.finish();
        assert!(t.truncated);
        assert_eq!(t.lines.len(), 1);
    }

    #[test]
    fn status_bar_role_maps_to_its_atspi_numbers() {
        let roles = raw_roles(Role::StatusBar);
        assert_eq!(roles, vec![atspi::Role::StatusBar as i32]);
        assert!(raw_roles(Role::Window).len() >= 2, "Frame and Window");
    }

    #[test]
    fn the_wire_rule_matches_roles_any_and_states_all() {
        let rule = MatchRule {
            root: NodeKey {
                app: gorca_model::AppId {
                    connector: gorca_model::ConnectorId(1),
                    serial: 1,
                },
                local: gorca_model::LocalId(1),
            },
            roles: vec![Role::StatusBar],
            states_all: StateSet::SHOWING | StateSet::VISIBLE,
            states_none: StateSet::empty(),
            max_results: 1,
        };
        let w = wire_rule(&rule);
        let bits = (Raw::Showing as u64) | (Raw::Visible as u64);
        assert_eq!(w.0[0].cast_unsigned(), u32::try_from(bits).unwrap());
        assert_eq!(w.1, MATCH_ALL);
        assert_eq!(w.4.len(), RULE_ROLE_SLOTS);
        assert_eq!(w.4[0], atspi::Role::StatusBar as i32);
        assert_eq!(w.5, MATCH_ANY);
        assert!(w.2.is_empty() && w.6.is_empty() && !w.8);
        // No roles and no states: both criteria are empty.
        let open = wire_rule(&MatchRule {
            roles: vec![],
            states_all: StateSet::empty(),
            ..rule
        });
        assert_eq!((open.1, open.5), (MATCH_EMPTY, MATCH_EMPTY));
    }
}
