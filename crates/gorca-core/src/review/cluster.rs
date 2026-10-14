//! `build_line_model`: zones from snapshots and text lines, clustered into lines
//! (ADR-0006 6.3, 6.4, 6.7). **Pure**: owned input, no I/O, no clock. It runs on the `cpu` pool.
//!
//! The input is hostile (an application chose every rectangle and string): rectangles are
//! sanitised, work is bounded by [`ReviewLimits`], and the output holds every zone exactly once.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use gorca_model::{AppId, Ifaces, NodeKey, NodeSnapshot, Rect, Role, StateSet, TextLine, Value};
use smallvec::SmallVec;

use super::model::{
    Completeness, Line, LineModel, LineSource, NodeCheck, ReviewLimits, TruncWhy, Zone, ZoneKind,
    next_model_id,
};
use super::objnav::{PresClass, class};
use super::validity::snapshot_hash;
use crate::present::flat_zone_text;

/// U+FFFC OBJECT REPLACEMENT CHARACTER: an embedded object inside a text.
const EMBEDDED: char = '\u{fffc}';
/// Coordinates beyond this are bogus (ADR-0006 6.7).
const COORD_MAX: i64 = 1_000_000;

/// One node of the surface walk, in pre-order.
#[derive(Clone, Debug)]
pub struct BuildNode {
    pub snap: Arc<NodeSnapshot>,
    /// Depth below the surface root (0 for the root).
    pub depth: u16,
    /// Visible lines of the node's text, if it was asked for them.
    pub lines: Option<Arc<[TextLine]>>,
}

#[derive(Clone, Debug)]
pub struct BuildInput {
    pub app: AppId,
    pub surface: NodeKey,
    pub nodes: Vec<BuildNode>,
    pub built_rev: u64,
    pub built_at: Instant,
    /// The collector stopped at a bound.
    pub truncated: Option<TruncWhy>,
}

/// A rectangle an application may have invented: kept only when it can be a screen area.
pub fn sane_rect(r: Rect) -> Option<Rect> {
    let ok = |v: i64| (-COORD_MAX..=COORD_MAX).contains(&v);
    let (x, y, w, h) = (
        i64::from(r.x),
        i64::from(r.y),
        i64::from(r.w),
        i64::from(r.h),
    );
    (w > 0 && h > 0 && w <= COORD_MAX && h <= COORD_MAX && ok(x) && ok(y) && ok(x + w) && ok(y + h))
        .then_some(r)
}

fn is_entry(role: Role) -> bool {
    matches!(role, Role::TextEntry | Role::PasswordEntry)
}

/// Controls whose name is spoken as one zone together with their role.
fn is_control(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::MenuItem
            | Role::Tab
            | Role::Link
            | Role::TreeItem
    )
}

fn label_role(role: Role) -> bool {
    matches!(role, Role::Label | Role::StaticText)
}

fn norm(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn focusable(s: &NodeSnapshot) -> bool {
    s.states.contains(StateSet::FOCUSABLE)
}

fn actionable(s: &NodeSnapshot) -> bool {
    s.interfaces.contains(Ifaces::ACTION)
}

/// The object zone of a node, if it has anything to say or can be acted on.
fn object_zone(n: &BuildNode, surface: Option<Rect>) -> Option<Zone> {
    let s = &*n.snap;
    let mut text = if is_entry(s.role) && s.value.is_none() && n.lines.is_some() {
        // A single-line entry: the label, the role and what is typed.
        let typed = n
            .lines
            .as_deref()
            .map(|l| {
                l.iter()
                    .map(|x| x.text.as_ref())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .unwrap_or_default();
        let mut shadow = (*n.snap).clone();
        shadow.value = (!typed.is_empty()).then(|| Value::Text(Arc::from(typed)));
        flat_zone_text(&shadow)
    } else {
        flat_zone_text(s)
    };
    if text.is_empty() {
        if !(focusable(s) || actionable(s)) {
            return None;
        }
        let w = s.role.spoken();
        if w.is_empty() { "object" } else { w }.clone_into(&mut text);
    }
    Some(Zone {
        node: s.key,
        kind: ZoneKind::Object,
        text: Arc::from(text),
        rect: placed(s.bounds, surface),
        name: s.name.clone().filter(|n| !n.is_empty()),
        label_like: label_role(s.role),
    })
}

/// Sanitise a rectangle and drop one that lies entirely outside the surface.
fn placed(r: Option<Rect>, surface: Option<Rect>) -> Option<Rect> {
    let r = sane_rect(r?)?;
    if let Some(sf) = surface {
        let (rx0, ry0) = (i64::from(r.x), i64::from(r.y));
        let (rx1, ry1) = (rx0 + i64::from(r.w), ry0 + i64::from(r.h));
        let (sx0, sy0) = (i64::from(sf.x), i64::from(sf.y));
        let (sx1, sy1) = (sx0 + i64::from(sf.w), sy0 + i64::from(sf.h));
        if rx1 <= sx0 || rx0 >= sx1 || ry1 <= sy0 || ry0 >= sy1 {
            return None;
        }
    }
    Some(r)
}

/// Text zones of a node: one per visible line, split at embedded-object characters.
fn text_zones(n: &BuildNode, surface: Option<Rect>) -> Vec<Zone> {
    let s = &*n.snap;
    let Some(lines) = n.lines.as_deref() else {
        return Vec::new();
    };
    let node_rect = placed(s.bounds, surface);
    let mut out = Vec::new();
    for line in lines {
        let rect = line
            .rect
            .and_then(|r| placed(Some(r), surface))
            .or(node_rect);
        let mut at = line.range.start;
        for piece in line.text.split(EMBEDDED) {
            let len = u32::try_from(piece.chars().count()).unwrap_or(u32::MAX);
            let start = at;
            at = at.saturating_add(len).saturating_add(1);
            let t = piece.trim();
            if t.is_empty() {
                continue;
            }
            out.push(Zone {
                node: s.key,
                kind: ZoneKind::Text {
                    src: start..start.saturating_add(len),
                },
                text: Arc::from(t),
                rect,
                name: None,
                label_like: label_role(s.role),
            });
        }
    }
    out
}

/// Zones of the subtree rooted at node `i`, children already folded (rules 1 to 4, absorb).
fn finalize(n: &BuildNode, kids: Vec<Zone>, surface: Option<Rect>) -> Vec<Zone> {
    let s = &*n.snap;
    let has_lines = n.lines.as_ref().is_some_and(|l| !l.is_empty());
    let single_line_entry = is_entry(s.role) && !s.states.contains(StateSet::MULTILINE);
    if has_lines && !single_line_entry {
        let mut own = text_zones(n, surface);
        if !own.is_empty() {
            own.extend(kids);
            return own;
        }
    }
    if has_lines {
        // A single-line entry with text: label, role and the typed text in one zone.
        if let Some(z) = object_zone(n, surface) {
            return vec![z];
        }
    }
    if kids.is_empty() {
        // A label or paragraph without a text interface still says its name.
        let named_text = matches!(s.role, Role::Label | Role::StaticText | Role::Paragraph)
            && s.name.as_deref().is_some_and(|n| !n.trim().is_empty());
        let eligible =
            class(s) == PresClass::Content || focusable(s) || actionable(s) || named_text;
        return if eligible {
            object_zone(n, surface).into_iter().collect()
        } else {
            Vec::new()
        };
    }
    let name = s.name.as_deref().map(norm).filter(|x| !x.is_empty());
    let joined = norm(
        &kids
            .iter()
            .map(|z| z.text.as_ref())
            .collect::<Vec<_>>()
            .join(" "),
    );
    // A control whose name is just its label's text is one zone: "OK button", not "OK".
    if is_control(s.role)
        && let Some(nm) = &name
        && *nm == joined
        && let Some(mut z) = object_zone(n, surface)
    {
        if z.rect.is_none() {
            z.rect = union_rect(kids.iter().filter_map(|k| k.rect));
        }
        return vec![z];
    }
    // A named container says its name first, unless the name is made of its content (rule 3).
    if let Some(nm) = &name
        && !s.role.takes_name_from_content()
        // A window's title is `info.window_title`, not a line of its content.
        && !matches!(s.role, Role::Window | Role::Application)
        && class(s) == PresClass::Content
        && *nm != joined
        && let Some(z) = object_zone(n, surface)
    {
        let mut z = z;
        z.kind = ZoneKind::Name;
        let mut out = Vec::with_capacity(kids.len() + 1);
        out.push(z);
        out.extend(kids);
        return out;
    }
    kids
}

fn union_rect(it: impl Iterator<Item = Rect>) -> Option<Rect> {
    let mut acc: Option<(i64, i64, i64, i64)> = None;
    for r in it {
        let (x0, y0) = (i64::from(r.x), i64::from(r.y));
        let (x1, y1) = (x0 + i64::from(r.w), y0 + i64::from(r.h));
        acc = Some(match acc {
            None => (x0, y0, x1, y1),
            Some((a, b, c, d)) => (a.min(x0), b.min(y0), c.max(x1), d.max(y1)),
        });
    }
    let (x0, y0, x1, y1) = acc?;
    sane_rect(Rect {
        x: i32::try_from(x0).ok()?,
        y: i32::try_from(y0).ok()?,
        w: u32::try_from(x1 - x0).ok()?,
        h: u32::try_from(y1 - y0).ok()?,
    })
}

/// Fold the pre-order list into zones, still in pre-order.
fn collect_zones(input: &BuildInput, surface_rect: Option<Rect>) -> Vec<Zone> {
    let nodes = &input.nodes;
    let n = nodes.len();
    let mut parent: Vec<Option<usize>> = vec![None; n];
    let mut stack: Vec<usize> = Vec::new();
    let mut skip = vec![false; n];
    for (i, node) in nodes.iter().enumerate() {
        while let Some(&t) = stack.last() {
            if nodes[t].depth >= node.depth {
                stack.pop();
            } else {
                break;
            }
        }
        parent[i] = stack.last().copied();
        skip[i] = class(&node.snap) == PresClass::Unavailable || parent[i].is_some_and(|p| skip[p]);
        stack.push(i);
    }
    let mut pending: Vec<Vec<(usize, Vec<Zone>)>> = vec![Vec::new(); n];
    let mut top: Vec<(usize, Vec<Zone>)> = Vec::new();
    for i in (0..n).rev() {
        let zones = if skip[i] {
            Vec::new()
        } else {
            let mut kids_lists = std::mem::take(&mut pending[i]);
            kids_lists.sort_by_key(|(c, _)| *c);
            let kids: Vec<Zone> = kids_lists.into_iter().flat_map(|(_, z)| z).collect();
            finalize(&nodes[i], kids, surface_rect)
        };
        match parent[i] {
            Some(p) => pending[p].push((i, zones)),
            None => top.push((i, zones)),
        }
    }
    top.sort_by_key(|(i, _)| *i);
    top.into_iter().flat_map(|(_, z)| z).collect()
}

/// Do two zones belong on one line? (CL-1.) The anchor is the first zone of the line.
fn same_line(a: Rect, z: Rect) -> bool {
    let (at, ab) = (i64::from(a.y), i64::from(a.y) + i64::from(a.h));
    let (zt, zb) = (i64::from(z.y), i64::from(z.y) + i64::from(z.h));
    let overlaps = zt < ab && at < zb;
    let (am, zm) = (at + i64::from(a.h) / 2, zt + i64::from(z.h) / 2);
    let tol = 5.max(i64::from(a.h.min(z.h)) / 4);
    overlaps && (am - zm).abs() <= tol
}

fn y_mid(r: Rect) -> i64 {
    i64::from(r.y) + i64::from(r.h) / 2
}

/// Order zone indices into lines (indices into `zones`), GX-1 and CL-1 to CL-3 applied.
fn cluster(zones: &[Zone], fallback: bool) -> Vec<Vec<usize>> {
    if fallback {
        return (0..zones.len()).map(|i| vec![i]).collect();
    }
    // Monotone envelope per node: the text zones of one node never move upward (CL-3).
    let mut env: Vec<Option<i64>> = vec![None; zones.len()];
    let mut last_y: HashMap<NodeKey, i64> = HashMap::new();
    for (i, z) in zones.iter().enumerate() {
        if let Some(r) = z.rect {
            let mut y = y_mid(r);
            if matches!(z.kind, ZoneKind::Text { .. }) {
                if let Some(prev) = last_y.get(&z.node) {
                    y = y.max(*prev);
                }
                last_y.insert(z.node, y);
            }
            env[i] = Some(y);
        }
    }
    let mut order: Vec<usize> = (0..zones.len()).filter(|i| env[*i].is_some()).collect();
    order.sort_by_key(|&i| {
        let x = zones[i].rect.map_or(0, |r| i64::from(r.x));
        (env[i].unwrap_or(0), x, i)
    });
    let mut lines: Vec<Vec<usize>> = Vec::new();
    let mut anchor: Option<Rect> = None;
    for i in order {
        let Some(r) = zones[i].rect else { continue };
        match (anchor, lines.last_mut()) {
            (Some(a), Some(cur)) if same_line(a, r) => cur.push(i),
            _ => {
                anchor = Some(r);
                lines.push(vec![i]);
            }
        }
    }
    // Within a line: left to right (CL-2, LTR), ties in tree order; one node's text zones keep
    // their source order (CL-3).
    for line in &mut lines {
        line.sort_by_key(|&i| (zones[i].rect.map_or(0, |r| i64::from(r.x)), i));
        let mut slots: HashMap<NodeKey, Vec<usize>> = HashMap::new();
        for (pos, &i) in line.iter().enumerate() {
            if matches!(zones[i].kind, ZoneKind::Text { .. }) {
                slots.entry(zones[i].node).or_default().push(pos);
            }
        }
        for positions in slots.values() {
            if positions.len() < 2 {
                continue;
            }
            let mut ids: Vec<usize> = positions.iter().map(|&p| line[p]).collect();
            ids.sort_by_key(|&i| (zones[i].src_start(), i));
            for (p, id) in positions.iter().zip(ids) {
                line[*p] = id;
            }
        }
    }
    // Zones without extents: their own line right after the line of the nearest preceding
    // zone (tree order) that has one, or at the top (GX-1).
    let mut line_of = vec![usize::MAX; zones.len()];
    for (l, line) in lines.iter().enumerate() {
        for &i in line {
            line_of[i] = l;
        }
    }
    let mut after: Vec<Vec<usize>> = vec![Vec::new(); lines.len() + 1];
    let mut prev_line: Option<usize> = None;
    for (i, z) in zones.iter().enumerate() {
        if z.rect.is_some() && env[i].is_some() {
            prev_line = Some(line_of[i]);
        } else {
            // Slot 0 = before the first line; slot l + 1 = right after line l.
            after[prev_line.map_or(0, |l| l + 1)].push(i);
        }
    }
    let mut out: Vec<Vec<usize>> = Vec::with_capacity(zones.len());
    for i in &after[0] {
        out.push(vec![*i]);
    }
    for (l, line) in lines.into_iter().enumerate() {
        out.push(line);
        for i in &after[l + 1] {
            out.push(vec![*i]);
        }
    }
    out
}

/// Drop a label's text when a control on the same line carries the same name (rule 4).
fn elide_labels(zones: &[Zone], lines: &mut [Vec<usize>]) {
    for line in lines.iter_mut() {
        if line.len() < 2 {
            continue;
        }
        let names: SmallVec<[&str; 4]> = line
            .iter()
            .filter(|&&i| zones[i].kind == ZoneKind::Object && !zones[i].label_like)
            .filter_map(|&i| zones[i].name.as_deref())
            .collect();
        if names.is_empty() {
            continue;
        }
        line.retain(|&i| {
            let z = &zones[i];
            !(z.label_like && names.iter().any(|n| *n == z.text.as_ref()))
        });
    }
}

/// Build the immutable line model of one surface (ADR-0006 6.1, step 4).
pub fn build_line_model(input: &BuildInput, limits: &ReviewLimits) -> LineModel {
    let surface_rect = input
        .nodes
        .first()
        .and_then(|n| n.snap.bounds)
        .and_then(sane_rect);
    let mut zones = collect_zones(input, surface_rect);
    let mut completeness = input
        .truncated
        .map_or(Completeness::Complete, Completeness::Truncated);
    if zones.len() > limits.zones {
        zones.truncate(limits.zones);
        completeness = Completeness::Truncated(TruncWhy::ZoneCap);
    }
    let mut bytes = 0usize;
    if let Some(cut) = zones.iter().position(|z| {
        bytes += z.text.len();
        bytes > limits.text_bytes
    }) {
        zones.truncate(cut);
        completeness = Completeness::Truncated(TruncWhy::TextCap);
    }
    let missing = zones.iter().filter(|z| z.rect.is_none()).count();
    let fallback = !zones.is_empty() && missing * 2 > zones.len();
    if fallback {
        completeness = Completeness::GeometryFallback;
    }
    let mut groups = cluster(&zones, fallback);
    elide_labels(&zones, &mut groups);
    groups.retain(|g| !g.is_empty());

    // Final arrays: zones in reading order, lines as ranges over them.
    let mut slots: Vec<Option<Zone>> = zones.into_iter().map(Some).collect();
    let mut out_zones: Vec<Zone> = Vec::with_capacity(slots.len());
    let mut out_lines: Vec<Line> = Vec::with_capacity(groups.len());
    for g in groups {
        let start = u32::try_from(out_zones.len()).unwrap_or(u32::MAX);
        let mut texts: Vec<Arc<str>> = Vec::with_capacity(g.len());
        let (mut top, mut bottom) = (i32::MAX, i32::MIN);
        for i in g {
            let Some(z) = slots.get_mut(i).and_then(Option::take) else {
                continue;
            };
            if let Some(r) = z.rect {
                top = top.min(r.y);
                bottom = bottom.max(r.y.saturating_add_unsigned(r.h));
            }
            texts.push(Arc::clone(&z.text));
            out_zones.push(z);
        }
        let end = u32::try_from(out_zones.len()).unwrap_or(u32::MAX);
        if end == start {
            continue;
        }
        let text: Arc<str> = if texts.len() == 1 {
            Arc::clone(&texts[0])
        } else {
            Arc::from(
                texts
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" "),
            )
        };
        let (top, bottom) = if top > bottom { (0, 0) } else { (top, bottom) };
        out_lines.push(Line {
            zones: Range { start, end },
            top,
            bottom,
            text,
        });
    }

    let mut by_node: HashMap<NodeKey, SmallVec<[u32; 2]>> = HashMap::new();
    for (i, z) in out_zones.iter().enumerate() {
        by_node
            .entry(z.node)
            .or_default()
            .push(u32::try_from(i).unwrap_or(u32::MAX));
    }
    let by_key: HashMap<NodeKey, &BuildNode> =
        input.nodes.iter().map(|n| (n.snap.key, n)).collect();
    let mut checks: Vec<NodeCheck> = by_node
        .keys()
        .filter_map(|k| {
            by_key.get(k).map(|n| NodeCheck {
                node: *k,
                hash: snapshot_hash(&n.snap),
            })
        })
        .collect();
    checks.sort_by_key(|c| c.node);
    let texts: HashMap<NodeKey, Arc<[TextLine]>> = input
        .nodes
        .iter()
        .filter_map(|n| n.lines.as_ref().map(|l| (n.snap.key, Arc::clone(l))))
        .collect();
    LineModel {
        id: next_model_id(),
        app: input.app,
        surface: input.surface,
        source: LineSource::Geometry,
        lines: out_lines.into_boxed_slice(),
        zones: out_zones.into_boxed_slice(),
        by_node,
        checks: checks.into_boxed_slice(),
        texts,
        built_rev: input.built_rev,
        built_at: input.built_at,
        completeness,
    }
}

#[cfg(test)]
mod tests {
    use super::super::collect::testsrc::{key, snap};
    use super::*;
    use gorca_model::PropMask;

    fn bn(mut s: NodeSnapshot, depth: u16, r: Option<Rect>) -> BuildNode {
        s.bounds = r;
        if r.is_some() {
            s.have |= PropMask::BOUNDS;
        }
        BuildNode {
            snap: Arc::new(s),
            depth,
            lines: None,
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn r(x: i32, y: i32, w: u32, h: u32) -> Option<Rect> {
        Some(Rect { x, y, w, h })
    }

    fn input(nodes: Vec<BuildNode>) -> BuildInput {
        BuildInput {
            app: key(1).app,
            surface: key(1),
            nodes,
            built_rev: 7,
            built_at: Instant::now(),
            truncated: None,
        }
    }

    fn btn(n: u64, name: &str) -> NodeSnapshot {
        let mut s = snap(n, Role::Button, name, Some(1), &[]);
        s.states |= StateSet::FOCUSABLE;
        s
    }

    fn texts(m: &LineModel) -> Vec<String> {
        m.lines.iter().map(|l| l.text.to_string()).collect()
    }

    fn root(kids: &[u64]) -> BuildNode {
        bn(snap(1, Role::Window, "W", None, kids), 0, r(0, 0, 800, 600))
    }

    #[test]
    fn a_three_column_form_reads_row_by_row_left_to_right() {
        let mut nodes = vec![root(&[2, 3, 4, 5, 6, 7])];
        // Tree order is column by column; the screen is row by row.
        let cols = [
            (2, "A1", 0, 0),
            (3, "A2", 0, 40),
            (4, "B1", 200, 0),
            (5, "B2", 200, 40),
            (6, "C1", 400, 0),
            (7, "C2", 400, 40),
        ];
        for (n, name, x, y) in cols {
            nodes.push(bn(btn(n, name), 1, r(x, y, 100, 30)));
        }
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert_eq!(
            texts(&m),
            [
                "A1 button B1 button C1 button",
                "A2 button B2 button C2 button"
            ]
        );
        assert_eq!(m.completeness, Completeness::Complete);
        assert_eq!(m.built_rev, 7);
    }

    #[test]
    fn a_staggered_layout_does_not_drift_into_one_line() {
        // Each zone sits 4 px lower than the previous one: pairwise they overlap, but the line
        // is decided against its first zone, so the row breaks apart (CL-1).
        let mut nodes = vec![root(&(2..12).collect::<Vec<_>>())];
        for i in 0..10u64 {
            let y = i32::try_from(i).unwrap_or(0) * 4;
            let x = i32::try_from(i).unwrap_or(0) * 60;
            nodes.push(bn(btn(2 + i, &format!("b{i}")), 1, r(x, y, 50, 20)));
        }
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert!(m.lines.len() >= 2, "{:?}", texts(&m));
        assert!(m.lines.len() <= 10);
        let mut all: Vec<_> = m.zones.iter().map(|z| z.node.local.0).collect();
        all.sort_unstable();
        assert_eq!(all, (2..12).collect::<Vec<_>>(), "nothing lost");
    }

    #[test]
    fn a_wrapped_label_keeps_its_source_order_even_with_inconsistent_extents() {
        let mut label = snap(2, Role::Label, "", Some(1), &[]);
        label.interfaces = Ifaces::TEXT;
        let mut n = bn(label, 1, r(0, 0, 300, 90));
        let line = |a: u32, b: u32, t: &str, y: i32| TextLine {
            range: a..b,
            text: Arc::from(t),
            rect: r(0, y, 300, 20),
        };
        // The toolkit reports the third line above the second.
        n.lines = Some(Arc::from(vec![
            line(0, 5, "one", 0),
            line(5, 10, "three", 50),
            line(10, 15, "two", 25),
        ]));
        let m = build_line_model(&input(vec![root(&[2]), n]), &ReviewLimits::default());
        let t = texts(&m);
        let pos = |s: &str| t.iter().position(|l| l.contains(s));
        assert!(pos("one") <= pos("three"), "{t:?}");
        assert!(
            pos("three") <= pos("two"),
            "source order wins over bogus extents: {t:?}"
        );
    }

    #[test]
    fn embedded_object_characters_split_a_text_line_into_zones() {
        let mut label = snap(2, Role::Label, "", Some(1), &[]);
        label.interfaces = Ifaces::TEXT;
        let mut n = bn(label, 1, r(0, 0, 300, 20));
        n.lines = Some(Arc::from(vec![TextLine {
            range: 0..9,
            text: Arc::from("see \u{fffc} here"),
            rect: r(0, 0, 300, 20),
        }]));
        let m = build_line_model(&input(vec![root(&[2]), n]), &ReviewLimits::default());
        assert_eq!(m.zones.len(), 2);
        assert_eq!(texts(&m), ["see here"]);
    }

    #[test]
    fn a_zone_without_extents_gets_its_own_line_after_its_predecessor() {
        let nodes = vec![
            root(&[2, 3, 4]),
            bn(btn(2, "top"), 1, r(0, 0, 50, 20)),
            bn(btn(3, "blind"), 1, None),
            bn(btn(4, "bottom"), 1, r(0, 100, 50, 20)),
        ];
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert_eq!(texts(&m), ["top button", "blind button", "bottom button"]);
        assert_eq!(m.completeness, Completeness::Complete);
        // A zone with no extents before any zone with extents goes to the top.
        let nodes = vec![
            root(&[2, 3, 4]),
            bn(btn(2, "blind"), 1, None),
            bn(btn(3, "a"), 1, r(0, 0, 50, 20)),
            bn(btn(4, "b"), 1, r(0, 50, 50, 20)),
        ];
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert_eq!(texts(&m), ["blind button", "a button", "b button"]);
    }

    #[test]
    fn more_than_half_without_extents_falls_back_to_tree_order() {
        let nodes = vec![
            bn(snap(1, Role::Window, "W", None, &[2, 3, 4]), 0, None),
            bn(btn(2, "a"), 1, None),
            bn(btn(3, "b"), 1, None),
            bn(btn(4, "c"), 1, r(0, 0, 50, 20)),
        ];
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert_eq!(m.completeness, Completeness::GeometryFallback);
        assert_eq!(texts(&m), ["a button", "b button", "c button"]);
    }

    #[test]
    fn bogus_extents_are_treated_as_missing() {
        for bad in [
            r(0, 0, 0, 20),
            r(0, 0, 50, 0),
            r(i32::MAX, 0, 50, 20),
            r(0, i32::MIN, 50, 20),
            r(0, 0, u32::MAX, 20),
            r(5_000_000, 5_000_000, 10, 10),
        ] {
            assert!(bad.and_then(sane_rect).is_none(), "{bad:?}");
        }
        assert!(r(-5, -5, 10, 10).and_then(sane_rect).is_some());
        // Entirely outside the surface: missing too.
        let nodes = vec![root(&[2]), bn(btn(2, "far"), 1, r(5_000, 5_000, 10, 10))];
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert!(m.zones[0].rect.is_none());
    }

    #[test]
    fn a_control_absorbs_its_label_and_a_container_names_itself_first() {
        let mut lbl = snap(3, Role::Label, "OK", Some(2), &[]);
        lbl.interfaces = Ifaces::TEXT;
        let mut label_node = bn(lbl, 2, r(4, 4, 30, 20));
        label_node.lines = Some(Arc::from(vec![TextLine {
            range: 0..2,
            text: Arc::from("OK"),
            rect: r(4, 4, 30, 20),
        }]));
        let mut b = btn(2, "OK");
        b.children = gorca_model::ChildList::Known([key(3)].into_iter().collect());
        let list = snap(4, Role::List, "Folders", Some(1), &[5]);
        let item = bn(
            snap(5, Role::ListItem, "Inbox", Some(4), &[]),
            2,
            r(0, 100, 100, 20),
        );
        let nodes = vec![
            root(&[2, 4]),
            bn(b, 1, r(0, 0, 40, 30)),
            label_node,
            bn(list, 1, r(0, 90, 100, 5)),
            item,
        ];
        let m = build_line_model(&input(nodes), &ReviewLimits::default());
        assert_eq!(texts(&m), ["OK button", "Folders list", "Inbox"]);
    }

    #[test]
    fn an_entry_says_its_name_its_role_and_what_is_typed() {
        let mut e = snap(2, Role::TextEntry, "Search", Some(1), &[]);
        e.interfaces = Ifaces::TEXT;
        e.states |= StateSet::FOCUSABLE | StateSet::EDITABLE;
        let mut n = bn(e, 1, r(0, 0, 200, 20));
        n.lines = Some(Arc::from(vec![TextLine {
            range: 0..5,
            text: Arc::from("hello"),
            rect: r(0, 0, 200, 20),
        }]));
        let m = build_line_model(&input(vec![root(&[2]), n]), &ReviewLimits::default());
        assert_eq!(texts(&m), ["Search edit hello"]);
        assert_eq!(
            m.zones.len(),
            1,
            "one zone per single-line entry (GTK4 editables)"
        );
    }

    #[test]
    fn caps_truncate_and_say_so() {
        let mut nodes = vec![root(&(2..60).collect::<Vec<_>>())];
        for i in 0..58u64 {
            nodes.push(bn(
                btn(2 + i, "x"),
                1,
                r(0, i32::try_from(i).unwrap_or(0) * 30, 50, 20),
            ));
        }
        let lim = ReviewLimits {
            zones: 10,
            ..ReviewLimits::default()
        };
        let m = build_line_model(&input(nodes.clone()), &lim);
        assert_eq!(m.zones.len(), 10);
        assert_eq!(m.completeness, Completeness::Truncated(TruncWhy::ZoneCap));
        let lim = ReviewLimits {
            text_bytes: 25,
            ..ReviewLimits::default()
        };
        let m = build_line_model(&input(nodes), &lim);
        assert_eq!(m.completeness, Completeness::Truncated(TruncWhy::TextCap));
        assert!(m.zones.len() < 5);
    }

    /// V-6: hostile trees never break the invariants (no zone lost or doubled, bounded work),
    /// whatever the rectangles say.
    #[test]
    fn fuzz_hostile_extents_keep_every_zone_exactly_once() {
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let small = |v: u64, m: u64| u32::try_from(v % m).unwrap_or(0);
        for round in 0..200 {
            let n = 1 + usize::try_from(next() % 60).unwrap_or(1);
            let kids: Vec<u64> = (2..2 + u64::try_from(n).unwrap_or(0)).collect();
            let mut nodes = vec![root(&kids)];
            for i in 0..n {
                let rect = match next() % 6 {
                    0 => None,
                    1 => r(i32::MIN, i32::MAX, u32::MAX, 0),
                    2 => r(0, 0, 0, 0),
                    3 => r(
                        i32::try_from(small(next(), 100_000)).unwrap_or(0) - 50_000,
                        i32::try_from(small(next(), 100_000)).unwrap_or(0) - 50_000,
                        small(next(), 500),
                        small(next(), 500),
                    ),
                    _ => r(
                        i32::try_from(small(next(), 800)).unwrap_or(0),
                        i32::try_from(small(next(), 600)).unwrap_or(0),
                        1 + small(next(), 200),
                        1 + small(next(), 50),
                    ),
                };
                let mut s = btn(2 + u64::try_from(i).unwrap_or(0), &format!("n{i}"));
                if next() % 5 == 0 {
                    s.name = None;
                }
                nodes.push(bn(s, 1, rect));
            }
            let lim = ReviewLimits::default();
            let m = build_line_model(&input(nodes), &lim);
            assert_eq!(m.zones.len(), n, "round {round}");
            let mut seen = std::collections::HashSet::new();
            let mut total = 0;
            for (li, l) in m.lines.iter().enumerate() {
                assert!(l.zones.start < l.zones.end, "empty line {li}");
                for z in &m.zones[l.zones.start as usize..l.zones.end as usize] {
                    assert!(seen.insert(z.node), "round {round}: duplicate zone");
                    total += 1;
                }
            }
            assert_eq!(total, n);
            assert!(m.zones.len() <= lim.zones);
        }
    }

    #[test]
    fn the_model_builds_in_budget() {
        // R3: build_line_model CPU for 2048 zones. The strict 2 ms gate is the release bound.
        let n = 2_000u64;
        let kids: Vec<u64> = (2..2 + n).collect();
        let mut nodes = vec![root(&kids)];
        for i in 0..n {
            let (row, col) = (
                i32::try_from(i / 8).unwrap_or(0),
                i32::try_from(i % 8).unwrap_or(0),
            );
            nodes.push(bn(
                btn(2 + i, &format!("Button {i}")),
                1,
                r(col * 90, row * 28, 80, 24),
            ));
        }
        let inp = input(nodes);
        let t = Instant::now();
        let m = build_line_model(&inp, &ReviewLimits::default());
        let took = t.elapsed();
        assert_eq!(m.zones.len(), 2_000);
        let bound = if cfg!(debug_assertions) {
            std::time::Duration::from_millis(400)
        } else {
            std::time::Duration::from_millis(5)
        };
        assert!(took < bound, "{took:?}");
    }
}
