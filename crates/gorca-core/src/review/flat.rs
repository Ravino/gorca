// SPDX-License-Identifier: GPL-3.0-or-later
//! Moves of the flat-review position over a [`LineModel`] (ADR-0006 6.9, 4.2). Pure index
//! arithmetic over immutable data: no IPC, no locks, which is what makes R1 hold.

use gorca_i18n::{MsgId as M, t};
use gorca_model::NodeKey;

use super::cursor::{FlatPos, ZoneAnchor, hash_line};
use super::model::{LineModel, LineSource, Zone, ZoneKind};
use super::segment::{TextUnits, Unit, slice};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FlatMove {
    PrevLine,
    CurLine,
    NextLine,
    PrevWord,
    CurWord,
    NextWord,
    PrevChar,
    CurChar,
    NextChar,
    Top,
    Bottom,
    StartOfLine,
    EndOfLine,
}

/// The result of a move: the new position, what to say, and whether it hit an end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlatOut {
    pub pos: FlatPos,
    pub said: String,
    pub boundary: bool,
}

pub(crate) fn zone_at(model: &LineModel, line: u32, zone: u16) -> Option<(u32, &Zone)> {
    let l = model.lines.get(line as usize)?;
    let g = l.zones.start.checked_add(u32::from(zone))?;
    if g >= l.zones.end {
        return None;
    }
    Some((g, model.zones.get(g as usize)?))
}

/// A position inside `model` (clamped to what exists).
pub fn pos_at(model: &LineModel, line: u32, zone: u16, offset: u32) -> Option<FlatPos> {
    let (_, z) = zone_at(model, line, zone)?;
    let l = model.lines.get(line as usize)?;
    let len = u32::try_from(z.text.chars().count()).unwrap_or(u32::MAX);
    Some(FlatPos {
        model: model.id,
        line,
        zone,
        offset: offset.min(len.saturating_sub(1)),
        anchor: ZoneAnchor {
            node: z.node,
            src_start: z.src_start(),
            y_mid: z.rect.map_or_else(
                || l.top.saturating_add(l.bottom.saturating_sub(l.top) / 2),
                |r| r.y.saturating_add_unsigned(r.h / 2),
            ),
            line_hash: hash_line(&l.text),
        },
    })
}

fn pos_of_global(model: &LineModel, g: u32, offset: u32) -> Option<FlatPos> {
    let (line, zone) = model.locate(g)?;
    pos_at(model, line, zone, offset)
}

/// The global index of the zone a position is in.
fn global(model: &LineModel, pos: &FlatPos) -> Option<u32> {
    zone_at(model, pos.line, pos.zone).map(|(g, _)| g)
}

/// The text of `line`, "blank" when it has nothing to read.
pub fn line_text(model: &LineModel, line: u32) -> String {
    match model.lines.get(line as usize) {
        Some(l) if !l.text.trim().is_empty() => l.text.to_string(),
        Some(_) | None => t(M::WordBlank).to_owned(),
    }
}

/// First zone of the nav node (RC-1), position at its start.
pub fn anchor_at_node(model: &LineModel, node: NodeKey) -> Option<FlatPos> {
    let first = *model.by_node.get(&node)?.first()?;
    pos_of_global(model, first, 0)
}

/// Re-anchor a position of an older model (RA-1).
pub fn reanchor(model: &LineModel, a: ZoneAnchor) -> Option<FlatPos> {
    if model.source == LineSource::Terminal {
        // No old model to compare with: the content first (ADR-0018), else the same row.
        return match carry_terminal(None, model, &a, 0, None) {
            Some(Carried::At(p)) => Some(p),
            Some(Carried::Gone | Carried::Replaced) | None => nearest_line(model, a.y_mid),
        };
    }
    let zs = model.by_node.get(&a.node)?;
    let pick = zs
        .iter()
        .copied()
        .find(|g| {
            model.zones.get(*g as usize).is_some_and(|z| match &z.kind {
                ZoneKind::Text { src } => src.contains(&a.src_start),
                ZoneKind::Object | ZoneKind::Name => true,
            })
        })
        .or_else(|| zs.first().copied())?;
    pos_of_global(model, pick, 0)
}

/// Fallback of RA-1: the line whose middle is closest to `y_mid`.
pub fn nearest_line(model: &LineModel, y_mid: i32) -> Option<FlatPos> {
    let best = model
        .lines
        .iter()
        .enumerate()
        .min_by_key(|(_, l)| {
            let mid = i64::from(l.top) + (i64::from(l.bottom) - i64::from(l.top)) / 2;
            (mid - i64::from(y_mid)).abs()
        })?
        .0;
    pos_at(model, u32::try_from(best).ok()?, 0, 0)
}

/// Where a terminal position went when the screen was rebuilt (ADR-0018).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Carried {
    /// On the same content (or on the row its content was rewritten on), same offset.
    At(FlatPos),
    /// The line scrolled off the screen: nothing on the new screen is it any more.
    Gone,
    /// The new screen shares no line with the old one (a flood or a clear replaced it).
    Replaced,
}

/// Most rows the shift estimate looks at; a bigger screen is compared on its first rows.
const SHIFT_ROWS: usize = 512;

/// The row of a terminal anchor in the model it was made for.
fn anchor_row(a: &ZoneAnchor) -> usize {
    usize::try_from(a.y_mid.max(0) / super::terminal::ROW_H).unwrap_or(0)
}

/// The signed row shift (new row = old row + d) that makes most non-blank rows of `old` equal to
/// rows of `new`, with the number of rows it explains. Ties go to the smallest |d|, then to the
/// scroll direction output makes (up, d < 0).
/// Rows of `o` that equal the row `d` further down in `n` (blank rows never count).
fn shift_score(o: &[u64], n: &[u64], blank: u64, d: i64) -> usize {
    o.iter()
        .enumerate()
        .filter(|(i, h)| {
            **h != blank
                && i64::try_from(*i)
                    .ok()
                    .and_then(|i| usize::try_from(i + d).ok())
                    .and_then(|j| n.get(j))
                    == Some(*h)
        })
        .count()
}

/// The hashes of the rows of `m`, the caret row blanked: a prompt stays where it is while output
/// scrolls above it, so it would vote for "no scroll".
fn masked_hashes(m: &LineModel, blank: u64) -> Vec<u64> {
    let mut v: Vec<u64> = m.lines.iter().map(|l| hash_line(&l.text)).collect();
    if let Some(f) = m.focus_line.and_then(|f| v.get_mut(f as usize)) {
        *f = blank;
    }
    v
}

fn best_shift(old: &[u64], new: &[u64], blank: u64) -> (i64, usize, bool) {
    let (o, n) = (
        &old[..old.len().min(SHIFT_ROWS)],
        &new[..new.len().min(SHIFT_ROWS)],
    );
    let mut best = (0i64, 0usize);
    let mut ties = 0usize;
    let span = i64::try_from(o.len().max(n.len())).unwrap_or(0);
    for k in 0..=span {
        let both = [-k, k];
        for &d in if k == 0 { &both[1..] } else { &both[..] } {
            let score = shift_score(o, n, blank, d);
            if score > best.1 {
                best = (d, score);
                ties = 0;
            } else if score == best.1 && score > 0 {
                ties += 1;
            }
        }
    }
    (best.0, best.1, ties > 0)
}

/// Carry a terminal position over to a rebuilt screen (ADR-0018). `scroll` is the number of rows
/// the screen scrolled up since `anchor` was made, when the terminal's own edit events said so
/// (exact: VTE reports the rows that left the top); the line is then at its shifted row, or
/// gone. Without it the scroll is estimated from the rows the two screens share (`old` is the
/// screen the position was made on, when still known). The cursor never moves on an ambiguous
/// match: when several shifts explain the screens equally, or the content occurs on several
/// rows, the line is `Gone` (held, and read from its own text) unless it occurs exactly once. A
/// line rewritten in place (progress bar, prompt) keeps its shifted row. Without `old` the row
/// is the best guess and nothing is ever `Gone` by estimate. `None` when the new screen is empty.
pub fn carry_terminal(
    old: Option<&LineModel>,
    new: &LineModel,
    a: &ZoneAnchor,
    offset: u32,
    scroll: Option<u32>,
) -> Option<Carried> {
    if new.lines.is_empty() {
        return None;
    }
    let row = anchor_row(a);
    let new_h: Vec<u64> = new.lines.iter().map(|l| hash_line(&l.text)).collect();
    let blank = hash_line("");
    // A delete at the top of the screen is not always a scroll (a redraw of the top rows, tmux,
    // Ctrl+L): the count is used only when the content agrees with it.
    let scroll = scroll.filter(|q| {
        let d = -i64::from(*q);
        old.is_some_and(|o| {
            usize::try_from(i64::try_from(row).unwrap_or(0) + d)
                .ok()
                .and_then(|t| new_h.get(t))
                == Some(&a.line_hash)
                || shift_score(
                    &masked_hashes(o, blank),
                    &masked_hashes(new, blank),
                    blank,
                    d,
                ) > 0
        })
    });
    let exact = scroll.is_some();
    let (d, score, ambiguous) = match scroll {
        Some(q) => (-i64::from(q), usize::MAX, false),
        None => old.map_or((0, 0, false), |o| {
            best_shift(&masked_hashes(o, blank), &masked_hashes(new, blank), blank)
        }),
    };
    let target = i64::try_from(row).unwrap_or(0) + d;
    let at = |j: usize| {
        u32::try_from(j)
            .ok()
            .and_then(|l| pos_at(new, l, 0, offset))
            .map(Carried::At)
    };
    let shifted = usize::try_from(target).ok().filter(|t| *t < new_h.len());
    if exact {
        // The terminal said how far it scrolled: same row shifted, whatever it holds now.
        return shifted.map_or(Some(Carried::Gone), at);
    }
    let occ: Vec<usize> = new_h
        .iter()
        .enumerate()
        .filter(|(_, h)| **h == a.line_hash)
        .map(|(j, _)| j)
        .collect();
    if ambiguous && old.is_some() {
        return match occ.as_slice() {
            [only] => at(*only),
            _ => Some(Carried::Gone),
        };
    }
    if let Some(t) = shifted
        && new_h[t] == a.line_hash
    {
        return at(t);
    }
    let want = target.clamp(0, i64::try_from(new_h.len() - 1).unwrap_or(0));
    match occ.as_slice() {
        [] => {}
        [only] => return at(*only),
        _ if old.is_some() => return Some(Carried::Gone),
        many => {
            let j = many
                .iter()
                .copied()
                .min_by_key(|j| (i64::try_from(*j).unwrap_or(0) - want).abs())?;
            return at(j);
        }
    }
    match (old.is_some(), shifted) {
        // Continuity but the row is off the screen: scrolled away.
        (true, None) if score > 0 => Some(Carried::Gone),
        // Nothing in common at all with a known screen: a flood or a clear replaced it.
        (true, _) if score == 0 => Some(Carried::Replaced),
        // Rewritten in place at its shifted row.
        (_, Some(t)) => at(t),
        (_, None) => at(usize::try_from(want).unwrap_or(0)),
    }
}

fn boundary(pos: &FlatPos, text: &'static str) -> FlatOut {
    FlatOut {
        pos: *pos,
        said: text.to_owned(),
        boundary: true,
    }
}

/// The unit of `unit` at or after `from` in zone `g`, going forward; `None` past the last zone.
fn first_unit_from(
    model: &LineModel,
    units: &dyn TextUnits,
    mut g: u32,
    unit: Unit,
) -> Option<(u32, std::ops::Range<u32>)> {
    while let Some(z) = model.zones.get(g as usize) {
        if let Some(r) = units.units(&z.text, unit).into_iter().next() {
            return Some((g, r));
        }
        g += 1;
    }
    None
}

fn last_unit_before(
    model: &LineModel,
    units: &dyn TextUnits,
    mut g: u32,
    unit: Unit,
) -> Option<(u32, std::ops::Range<u32>)> {
    loop {
        let z = model.zones.get(g as usize)?;
        if let Some(r) = units.units(&z.text, unit).into_iter().next_back() {
            return Some((g, r));
        }
        g = g.checked_sub(1)?;
    }
}

fn unit_move(
    model: &LineModel,
    pos: &FlatPos,
    units: &dyn TextUnits,
    unit: Unit,
    fwd: bool,
) -> FlatOut {
    let Some(g) = global(model, pos) else {
        return boundary(
            pos,
            if fwd {
                t(M::ReviewBottom)
            } else {
                t(M::ReviewTop)
            },
        );
    };
    let here = model.zones.get(g as usize);
    let in_zone = here.and_then(|z| {
        units.next(
            &z.text,
            pos.offset,
            unit,
            if fwd {
                super::segment::Dir::Fwd
            } else {
                super::segment::Dir::Back
            },
        )
    });
    let target = match in_zone {
        Some(r) => Some((g, r)),
        None if fwd => first_unit_from(model, units, g + 1, unit),
        None => g
            .checked_sub(1)
            .and_then(|p| last_unit_before(model, units, p, unit)),
    };
    let Some((tg, r)) = target else {
        return boundary(
            pos,
            if fwd {
                t(M::ReviewBottom)
            } else {
                t(M::ReviewTop)
            },
        );
    };
    let (Some(z), Some(p)) = (
        model.zones.get(tg as usize),
        pos_of_global(model, tg, r.start),
    ) else {
        return boundary(
            pos,
            if fwd {
                t(M::ReviewBottom)
            } else {
                t(M::ReviewTop)
            },
        );
    };
    let said = spoken(&z.text, &r, unit);
    FlatOut {
        pos: p,
        said,
        boundary: false,
    }
}

/// What is said for the unit `r` of `text`: a word is said without the spaces it carries.
fn spoken(text: &str, r: &std::ops::Range<u32>, unit: Unit) -> String {
    let s = slice(text, r);
    match unit {
        Unit::Word => s.trim_end().to_owned(),
        Unit::Char => s,
    }
}

fn current_unit(model: &LineModel, pos: &FlatPos, units: &dyn TextUnits, unit: Unit) -> FlatOut {
    let said = global(model, pos)
        .and_then(|g| model.zones.get(g as usize))
        .and_then(|z| {
            units
                .at(&z.text, pos.offset, unit)
                .map(|r| spoken(&z.text, &r, unit))
        })
        .unwrap_or_default();
    FlatOut {
        pos: *pos,
        said,
        boundary: false,
    }
}

fn to_line(model: &LineModel, pos: &FlatPos, line: Option<u32>, edge: &'static str) -> FlatOut {
    match line.and_then(|l| pos_at(model, l, 0, 0)) {
        Some(p) => FlatOut {
            said: line_text(model, p.line),
            pos: p,
            boundary: false,
        },
        None => boundary(pos, edge),
    }
}

/// Apply one move to `pos` (which belongs to `model`).
pub fn flat_step(model: &LineModel, pos: &FlatPos, mv: FlatMove, units: &dyn TextUnits) -> FlatOut {
    let last = u32::try_from(model.lines.len().saturating_sub(1)).unwrap_or(0);
    match mv {
        FlatMove::PrevLine => to_line(model, pos, pos.line.checked_sub(1), t(M::ReviewTop)),
        FlatMove::NextLine => to_line(
            model,
            pos,
            (pos.line < last).then_some(pos.line + 1),
            t(M::ReviewBottom),
        ),
        FlatMove::Top => to_line(
            model,
            pos,
            (!model.is_empty()).then_some(0),
            t(M::ReviewTop),
        ),
        FlatMove::Bottom => to_line(
            model,
            pos,
            (!model.is_empty()).then_some(last),
            t(M::ReviewBottom),
        ),
        FlatMove::CurLine => FlatOut {
            said: line_text(model, pos.line),
            pos: *pos,
            boundary: false,
        },
        FlatMove::NextWord => unit_move(model, pos, units, Unit::Word, true),
        FlatMove::PrevWord => unit_move(model, pos, units, Unit::Word, false),
        FlatMove::NextChar => unit_move(model, pos, units, Unit::Char, true),
        FlatMove::PrevChar => unit_move(model, pos, units, Unit::Char, false),
        FlatMove::CurWord => current_unit(model, pos, units, Unit::Word),
        FlatMove::CurChar => current_unit(model, pos, units, Unit::Char),
        FlatMove::StartOfLine => match pos_at(model, pos.line, 0, 0) {
            Some(p) => current_unit(model, &p, units, Unit::Char),
            None => boundary(pos, t(M::ReviewTop)),
        },
        FlatMove::EndOfLine => {
            let n = model
                .lines
                .get(pos.line as usize)
                .map_or(0, |l| l.zones.end.saturating_sub(l.zones.start));
            let z = u16::try_from(n.saturating_sub(1)).unwrap_or(0);
            let len = zone_at(model, pos.line, z).map_or(0, |(_, zn)| {
                u32::try_from(zn.text.chars().count()).unwrap_or(0)
            });
            match pos_at(model, pos.line, z, len.saturating_sub(1)) {
                Some(p) => current_unit(model, &p, units, Unit::Char),
                None => boundary(pos, t(M::ReviewBottom)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::cluster::{BuildInput, BuildNode, build_line_model};
    use super::super::collect::testsrc::{key, snap};
    use super::super::model::ReviewLimits;
    use super::super::segment::SimpleUnits;
    use super::*;
    use gorca_model::{PropMask, Rect, Role, TextLine};
    use std::sync::Arc;
    use std::time::Instant;

    fn model() -> LineModel {
        // Two lines: "Save as" (label text, two words) and "OK" button beside "Cancel" button.
        let mut win = snap(1, Role::Window, "W", None, &[2, 3, 4]);
        win.set_bounds(Some(Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
        }));
        win.set_have(win.have() | PropMask::BOUNDS);
        let mut label = snap(2, Role::Label, "Save as", Some(1), &[]);
        label.set_bounds(Some(Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 20,
        }));
        label.set_caps(gorca_model::NodeCaps::new(gorca_model::Ifaces::TEXT));
        let mut ok = snap(3, Role::Button, "OK", Some(1), &[]);
        ok.set_bounds(Some(Rect {
            x: 0,
            y: 40,
            w: 40,
            h: 20,
        }));
        let mut cancel = snap(4, Role::Button, "Cancel", Some(1), &[]);
        cancel.set_bounds(Some(Rect {
            x: 60,
            y: 41,
            w: 60,
            h: 20,
        }));
        let node = |s, depth, lines| BuildNode {
            snap: Arc::new(s),
            web: false,
            depth,
            lines,
        };
        let lines: Arc<[TextLine]> = Arc::from(vec![TextLine {
            range: 0..7,
            text: Arc::from("Save as"),
            rect: Some(Rect {
                x: 0,
                y: 0,
                w: 80,
                h: 20,
            }),
        }]);
        build_line_model(
            &BuildInput {
                app: key(1).app,
                surface: key(1),
                nodes: vec![
                    node(win, 0, None),
                    node(label, 1, Some(lines)),
                    node(ok, 1, None),
                    node(cancel, 1, None),
                ],
                built_rev: 1,
                built_at: Instant::now(),
                truncated: None,
            },
            &ReviewLimits::default(),
        )
    }

    #[test]
    fn lines_read_in_visual_order_and_word_char_moves_cross_zones() {
        let m = model();
        assert_eq!(m.lines.len(), 2);
        assert_eq!(&*m.lines[0].text, "Save as");
        assert_eq!(&*m.lines[1].text, "OK button Cancel button");
        let u = SimpleUnits;
        let p = pos_at(&m, 0, 0, 0).unwrap();
        let o = flat_step(&m, &p, FlatMove::NextWord, &u);
        assert_eq!(o.said, "as");
        let o = flat_step(&m, &o.pos, FlatMove::NextWord, &u);
        assert_eq!((o.said.as_str(), o.pos.line, o.pos.zone), ("OK", 1, 0));
        let o = flat_step(&m, &o.pos, FlatMove::PrevWord, &u);
        assert_eq!(o.said, "as");
        let o = flat_step(&m, &o.pos, FlatMove::NextChar, &u);
        assert_eq!(o.said, "s");
        let o = flat_step(&m, &o.pos, FlatMove::NextChar, &u);
        assert_eq!(o.said, "O");
    }

    #[test]
    fn the_ends_say_top_and_bottom_and_stay_put() {
        let m = model();
        let u = SimpleUnits;
        let p = pos_at(&m, 0, 0, 0).unwrap();
        let o = flat_step(&m, &p, FlatMove::PrevLine, &u);
        assert!(o.boundary && o.said == "Top" && o.pos == p);
        let o = flat_step(&m, &p, FlatMove::PrevChar, &u);
        assert!(o.boundary && o.said == "Top");
        let o = flat_step(&m, &p, FlatMove::Bottom, &u);
        assert_eq!(o.said, "OK button Cancel button");
        let o = flat_step(&m, &o.pos, FlatMove::NextLine, &u);
        assert!(o.boundary && o.said == "Bottom");
        let o = flat_step(&m, &o.pos, FlatMove::EndOfLine, &u);
        assert_eq!(o.said, "n");
        let o = flat_step(&m, &o.pos, FlatMove::StartOfLine, &u);
        assert_eq!(o.said, "O");
    }

    #[test]
    fn reanchoring_finds_the_same_node_or_the_nearest_line() {
        let m = model();
        let p = pos_at(&m, 1, 1, 0).unwrap();
        let again = reanchor(&m, p.anchor).unwrap();
        assert_eq!((again.line, again.zone), (1, 1));
        assert!(
            reanchor(
                &m,
                ZoneAnchor {
                    node: key(99),
                    src_start: 0,
                    y_mid: 0,
                    line_hash: 0
                }
            )
            .is_none()
        );
        assert_eq!(nearest_line(&m, 45).unwrap().line, 1);
        assert_eq!(anchor_at_node(&m, key(3)).unwrap().line, 1);
    }
    // ---- terminal positions follow their content (ADR-0018) ---------------------------------

    fn screen(text: &str) -> LineModel {
        use gorca_ext_api::terminal::TermSnapshot;
        let n = key(2);
        super::super::terminal::terminal_model(
            n.app,
            n,
            &TermSnapshot {
                text: text.to_owned(),
                caret: 0,
                generation: 1,
            },
            &ReviewLimits::default(),
            Instant::now(),
        )
    }

    fn carried(old: &LineModel, new: &LineModel, line: u32, offset: u32) -> Option<Carried> {
        let p = pos_at(old, line, 0, offset)?;
        carry_terminal(Some(old), new, &p.anchor, p.offset, None)
    }

    fn at(c: Option<Carried>) -> (u32, u32) {
        match c {
            Some(Carried::At(p)) => (p.line, p.offset),
            other => panic!("not carried: {other:?}"),
        }
    }

    #[test]
    fn a_scrolled_line_is_followed_with_its_offset() {
        let old = screen("a1\nalpha bravo\nb2\nc3");
        let new = screen("alpha bravo\nb2\nc3\nd4");
        assert_eq!(at(carried(&old, &new, 1, 6)), (0, 6));
        // Output that scrolls by two.
        let new = screen("b2\nc3\nd4\ne5");
        assert_eq!(carried(&old, &new, 1, 6), Some(Carried::Gone));
    }

    #[test]
    fn a_line_rewritten_in_place_keeps_its_row_and_clamps_the_offset() {
        let old = screen("x1\nprogress 10%\nx3");
        let new = screen("x1\nprogress 100% done\nx3");
        assert_eq!(at(carried(&old, &new, 1, 5)), (1, 5));
        let new = screen("x1\nok\nx3");
        assert_eq!(
            at(carried(&old, &new, 1, 9)),
            (1, 1),
            "clamped to the new length"
        );
    }

    #[test]
    fn repeated_lines_follow_the_scroll_not_the_nearest_equal_line() {
        let old = screen("y\ny\ny\ny\nanchor\ny\ntail one");
        let new = screen("y\ny\nanchor\ny\ntail one\nnew two\nnew three");
        // "y" at row 5 is the one after "anchor": two rows up, now row 3.
        assert_eq!(at(carried(&old, &new, 5, 0)), (3, 0));
        assert_eq!(at(carried(&old, &new, 4, 2)), (2, 2));
    }

    #[test]
    fn a_screen_with_nothing_in_common_loses_the_line_but_a_missing_old_screen_never_does() {
        let old = screen("one\ntwo\nthree");
        let new = screen("four\nfive\nsix");
        assert_eq!(carried(&old, &new, 1, 0), Some(Carried::Replaced));
        let p = pos_at(&old, 1, 0, 0).unwrap();
        assert_eq!(
            at(carry_terminal(None, &new, &p.anchor, 0, None)),
            (1, 0),
            "no old screen: the row"
        );
        assert!(carry_terminal(Some(&old), &screen(""), &p.anchor, 0, None).is_none());
    }

    #[test]
    fn the_shift_estimate_is_bounded_on_a_huge_screen() {
        let text: String = (0..3000).fold(String::new(), |a, i| a + &format!("line {i}\n"));
        let old = screen(&text);
        let shifted: String = (1..3001).fold(String::new(), |a, i| a + &format!("line {i}\n"));
        let new = screen(&shifted);
        let t = Instant::now();
        let _ = carried(&old, &new, 700, 0);
        assert!(t.elapsed() < std::time::Duration::from_millis(500));
    }
    #[test]
    fn an_exact_scroll_from_the_terminal_beats_the_content() {
        // A flood of one line: the content cannot say where the cursor went, the events can.
        let old = screen("ok\nok\nok\nok\nok");
        let new = screen("ok\nok\nok\nok\nok");
        let p = pos_at(&old, 3, 0, 1).unwrap();
        let c = carry_terminal(Some(&old), &new, &p.anchor, p.offset, Some(2));
        assert_eq!(at(c), (1, 1));
        // Distinct rows: a count the content backs is used, and can put the line off the top.
        let old = screen("a\nb\nc\nd\n$ ");
        let new = screen("z\nc\nd\ne\nf");
        let p = pos_at(&old, 3, 0, 0).unwrap();
        assert_eq!(
            at(carry_terminal(Some(&old), &new, &p.anchor, 0, Some(1))),
            (2, 0)
        );
        let p = pos_at(&old, 0, 0, 0).unwrap();
        assert_eq!(
            carry_terminal(Some(&old), &new, &p.anchor, 0, Some(1)),
            Some(Carried::Gone),
            "scrolled off the top"
        );
    }

    #[test]
    fn a_scroll_count_the_content_contradicts_is_not_trusted() {
        // A redraw of the top rows deletes at offset 0 too: "ccc" has not moved.
        let old = screen("aaa\nbbb\nccc\nddd\n$ ");
        let new = screen("xxx\nyyy\nccc\nddd\n$ ");
        let p = pos_at(&old, 2, 0, 1).unwrap();
        let c = carry_terminal(Some(&old), &new, &p.anchor, p.offset, Some(2));
        assert_eq!(at(c), (2, 1));
    }

    #[test]
    fn a_screen_sharing_nothing_is_replaced_not_scrolled() {
        let old = screen("one\ntwo\nthree");
        let new = screen("four\nfive\nsix");
        let p = pos_at(&old, 1, 0, 0).unwrap();
        assert_eq!(
            carry_terminal(Some(&old), &new, &p.anchor, 0, None),
            Some(Carried::Replaced)
        );
    }
}
