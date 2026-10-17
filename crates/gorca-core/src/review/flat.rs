//! Moves of the flat-review position over a [`LineModel`] (ADR-0006 6.9, 4.2). Pure index
//! arithmetic over immutable data: no IPC, no locks, which is what makes R1 hold.

use gorca_model::NodeKey;

use super::cursor::{FlatPos, ZoneAnchor};
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

fn zone_at(model: &LineModel, line: u32, zone: u16) -> Option<(u32, &Zone)> {
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
        Some(_) | None => "blank".to_owned(),
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
        // A screen is a grid: the same row is the same place, whatever the text scrolled to.
        return nearest_line(model, a.y_mid);
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
        return boundary(pos, if fwd { "Bottom" } else { "Top" });
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
        return boundary(pos, if fwd { "Bottom" } else { "Top" });
    };
    let (Some(z), Some(p)) = (
        model.zones.get(tg as usize),
        pos_of_global(model, tg, r.start),
    ) else {
        return boundary(pos, if fwd { "Bottom" } else { "Top" });
    };
    let said = slice(&z.text, &r);
    FlatOut {
        pos: p,
        said,
        boundary: false,
    }
}

fn current_unit(model: &LineModel, pos: &FlatPos, units: &dyn TextUnits, unit: Unit) -> FlatOut {
    let said = global(model, pos)
        .and_then(|g| model.zones.get(g as usize))
        .and_then(|z| {
            units
                .at(&z.text, pos.offset, unit)
                .map(|r| slice(&z.text, &r))
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
        FlatMove::PrevLine => to_line(model, pos, pos.line.checked_sub(1), "Top"),
        FlatMove::NextLine => to_line(
            model,
            pos,
            (pos.line < last).then_some(pos.line + 1),
            "Bottom",
        ),
        FlatMove::Top => to_line(model, pos, (!model.is_empty()).then_some(0), "Top"),
        FlatMove::Bottom => to_line(model, pos, (!model.is_empty()).then_some(last), "Bottom"),
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
            None => boundary(pos, "Top"),
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
                None => boundary(pos, "Bottom"),
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
        win.bounds = Some(Rect {
            x: 0,
            y: 0,
            w: 400,
            h: 300,
        });
        win.have |= PropMask::BOUNDS;
        let mut label = snap(2, Role::Label, "Save as", Some(1), &[]);
        label.bounds = Some(Rect {
            x: 0,
            y: 0,
            w: 80,
            h: 20,
        });
        label.interfaces = gorca_model::Ifaces::TEXT;
        let mut ok = snap(3, Role::Button, "OK", Some(1), &[]);
        ok.bounds = Some(Rect {
            x: 0,
            y: 40,
            w: 40,
            h: 20,
        });
        let mut cancel = snap(4, Role::Button, "Cancel", Some(1), &[]);
        cancel.bounds = Some(Rect {
            x: 60,
            y: 41,
            w: 60,
            h: 20,
        });
        let node = |s, depth, lines| BuildNode {
            snap: Arc::new(s),
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
                    y_mid: 0
                }
            )
            .is_none()
        );
        assert_eq!(nearest_line(&m, 45).unwrap().line, 1);
        assert_eq!(anchor_at_node(&m, key(3)).unwrap().line, 1);
    }
}
