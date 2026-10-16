//! Flat review over a terminal's screen (ADR-0006 LS-1, milestone M6).
//!
//! The terminal extension holds the visible screen (ADR-0001 T-2); a review key reads a line
//! model built from that snapshot, with no IPC. One zone per screen row, rows in order, the
//! row's text as the zone's text. The model is rebuilt when the snapshot's generation changes,
//! and the cursor is re-anchored by row (a screen is a grid, so "the same row" is the same place
//! after a scroll), never by source offset.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ext_vte::Screen;
use gorca_ext_api::terminal::TermSnapshot;
use gorca_model::{AppId, NodeKey, Rect};
use smallvec::SmallVec;

use super::model::{
    Completeness, Line, LineModel, LineSource, ReviewLimits, TruncWhy, Zone, ZoneKind,
    next_model_id,
};
use crate::reader::{Reader, ReaderEffects};

/// Height of one synthetic row (only the ordering matters).
const ROW_H: i32 = 16;

/// Build the line model of `snap`, the screen of terminal `node`.
pub fn terminal_model(
    app: AppId,
    node: NodeKey,
    snap: &TermSnapshot,
    limits: &ReviewLimits,
    now: Instant,
) -> LineModel {
    let screen = Screen::new(&snap.text);
    let (caret_row, _) = screen.locate(snap.caret);
    let mut rows = screen.line_count();
    let mut completeness = Completeness::Complete;
    if rows > limits.zones {
        rows = limits.zones;
        completeness = Completeness::Truncated(TruncWhy::ZoneCap);
    }
    let mut lines: Vec<Line> = Vec::with_capacity(rows);
    let mut zones: Vec<Zone> = Vec::with_capacity(rows);
    let mut start: u32 = 0;
    let mut bytes = 0usize;
    for i in 0..rows {
        let text = screen.line(i).trim_end();
        bytes += text.len();
        if bytes > limits.text_bytes {
            completeness = Completeness::Truncated(TruncWhy::TextCap);
            break;
        }
        let n = u32::try_from(text.chars().count()).unwrap_or(u32::MAX);
        let top = i32::try_from(i)
            .unwrap_or(i32::MAX / ROW_H)
            .saturating_mul(ROW_H);
        let z = u32::try_from(zones.len()).unwrap_or(u32::MAX);
        zones.push(Zone {
            node,
            kind: ZoneKind::Text {
                src: start..start.saturating_add(n),
            },
            text: Arc::from(text),
            rect: None,
            name: None,
            label_like: false,
        });
        lines.push(Line {
            zones: z..z + 1,
            top,
            bottom: top.saturating_add(ROW_H),
            text: Arc::from(text),
        });
        // The row's newline.
        start = start.saturating_add(n).saturating_add(1);
    }
    let focus_line =
        (!lines.is_empty()).then(|| u32::try_from(caret_row.min(lines.len() - 1)).unwrap_or(0));
    let mut by_node: HashMap<NodeKey, SmallVec<[u32; 2]>> = HashMap::new();
    by_node.insert(node, (0..u32::try_from(zones.len()).unwrap_or(0)).collect());
    LineModel {
        id: next_model_id(),
        app,
        surface: node,
        source: LineSource::Terminal,
        lines: lines.into_boxed_slice(),
        zones: zones.into_boxed_slice(),
        by_node,
        checks: Box::new([]),
        texts: HashMap::new(),
        // The generation of the screen the model was built from: a terminal model is current
        // while the extension reports the same one.
        built_rev: snap.generation,
        built_at: now,
        completeness,
        clip: Rect {
            x: 0,
            y: 0,
            w: 1,
            h: 1,
        },
        focus_line,
    }
}

impl Reader {
    /// The model of the tracked terminal's screen, when `nav` is that terminal (LS-1): the
    /// current one, or a fresh one installed as the review model. `None` for every other
    /// object, which goes through the geometry path.
    pub(super) fn term_review_model(
        &mut self,
        nav: NodeKey,
        now: Instant,
        fx: &mut ReaderEffects,
    ) -> Option<Arc<LineModel>> {
        let snap = self.term_snapshot(nav)?;
        if let Some(m) = self
            .review
            .cur
            .as_ref()
            .filter(|m| m.source == LineSource::Terminal && m.surface == nav)
            && m.built_rev == snap.generation
        {
            return Some(Arc::clone(m));
        }
        let m = Arc::new(terminal_model(
            nav.app,
            nav,
            &snap,
            &self.review.limits,
            now,
        ));
        self.install_model(&m, now, fx);
        Some(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{ConnectorId, LocalId};

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(3),
        }
    }

    fn snap(text: &str, caret: u32, generation: u64) -> TermSnapshot {
        TermSnapshot {
            text: text.to_owned(),
            caret,
            generation,
        }
    }

    #[test]
    fn rows_become_lines_in_order_with_the_caret_row_marked() {
        let n = node();
        let m = terminal_model(
            n.app,
            n,
            &snap("one\n\nthree\nme$ x", 14, 7),
            &ReviewLimits::default(),
            Instant::now(),
        );
        assert_eq!(m.source, LineSource::Terminal);
        assert_eq!(m.lines.len(), 4);
        assert_eq!(&*m.lines[2].text, "three");
        assert_eq!(&*m.lines[1].text, "", "blank rows are kept");
        assert_eq!(m.focus_line, Some(3));
        assert_eq!(m.built_rev, 7);
        assert!(m.lines.windows(2).all(|w| w[0].bottom <= w[1].top));
        assert_eq!(m.by_node[&n].len(), 4);
        assert!(m.checks.is_empty(), "nothing to revalidate");
    }

    #[test]
    fn source_ranges_follow_the_screen_offsets() {
        let n = node();
        let m = terminal_model(
            n.app,
            n,
            &snap("ab\ncde", 0, 1),
            &ReviewLimits::default(),
            Instant::now(),
        );
        assert!(matches!(&m.zones[0].kind, ZoneKind::Text { src } if *src == (0..2)));
        assert!(matches!(&m.zones[1].kind, ZoneKind::Text { src } if *src == (3..6)));
    }

    #[test]
    fn a_huge_screen_is_cut_at_the_limits() {
        let n = node();
        let text = "row\n".repeat(5000);
        let m = terminal_model(
            n.app,
            n,
            &snap(&text, 0, 1),
            &ReviewLimits::default(),
            Instant::now(),
        );
        assert_eq!(m.lines.len(), ReviewLimits::default().zones);
        assert_eq!(m.completeness, Completeness::Truncated(TruncWhy::ZoneCap));
    }

    #[test]
    fn an_empty_screen_has_no_lines_and_no_focus_line() {
        let n = node();
        let m = terminal_model(
            n.app,
            n,
            &snap("", 0, 1),
            &ReviewLimits::default(),
            Instant::now(),
        );
        assert!(m.is_empty());
        assert_eq!(m.focus_line, None);
    }
}
