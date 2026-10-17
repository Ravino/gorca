//! Flat review over a terminal's screen (ADR-0006 LS-1, milestone M6).
//!
//! The terminal extension holds the visible screen (ADR-0001 T-2); a review key reads a line
//! model built from that snapshot, with no IPC. One zone per screen row, rows in order, the
//! row's text as the zone's text. The model is rebuilt when the screen text changes (a caret-only
//! generation keeps it), and the cursor follows its line by content (ADR-0018, `flat::carry_terminal`),
//! never by visible row: a screen that scrolls under a streaming program must not move the cursor.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use gorca_ext_api::terminal::Rows;
use gorca_ext_api::terminal::TermSnapshot;
use gorca_model::{AppId, NodeKey, Rect};
use smallvec::SmallVec;

use super::cursor::ReviewPos;
use super::model::{
    Completeness, Line, LineModel, LineSource, ReviewLimits, TruncWhy, Zone, ZoneKind,
    next_model_id,
};
use crate::reader::{Reader, ReaderEffects};

/// Height of one synthetic row (only the ordering matters).
pub(super) const ROW_H: i32 = 16;

/// FNV-1a of the whole screen text.
fn hash_text(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The one-line model of a terminal line that scrolled off the screen (`built_rev` 0: it is
/// never the screen's current model).
pub(super) fn held_model(app: AppId, node: NodeKey, text: &str, now: Instant) -> LineModel {
    let snap = TermSnapshot {
        text: text.to_owned(),
        caret: 0,
        generation: 0,
    };
    terminal_model(app, node, &snap, &ReviewLimits::default(), now)
}

/// Build the line model of `snap`, the screen of terminal `node`.
pub fn terminal_model(
    app: AppId,
    node: NodeKey,
    snap: &TermSnapshot,
    limits: &ReviewLimits,
    now: Instant,
) -> LineModel {
    let screen = Rows::new(&snap.text);
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
        let text_hash = hash_text(&snap.text);
        if let Some(m) = self
            .review
            .cur
            .as_ref()
            .filter(|m| m.source == LineSource::Terminal && m.surface == nav)
        {
            // The same screen generation, or a new one with the same text (a caret-only change,
            // an event that changed nothing): the model stands. The caret row matters only to a
            // cursor that is not anchored yet.
            let same_text =
                m.built_rev == snap.generation || self.review.term_text == Some((m.id, text_hash));
            let anchored = matches!(
                self.cursor().map(|c| c.pos),
                Some(ReviewPos::Screen(Some(_)))
            );
            let caret_row = Rows::new(&snap.text).locate(snap.caret).0;
            let focus_ok = anchored
                || m.focus_line
                    .is_none_or(|l| l as usize == caret_row.min(m.lines.len().saturating_sub(1)));
            if same_text && focus_ok {
                return Some(Arc::clone(m));
            }
        }
        let m = Arc::new(terminal_model(
            nav.app,
            nav,
            &snap,
            &self.review.limits,
            now,
        ));
        self.review.term_text = Some((m.id, text_hash));
        self.install_model(&m, now, fx);
        Some(m)
    }

    /// The screen read that was answered after a review key spoke from the held screen: the
    /// model is rebuilt now, so that a line that turns out to be different is corrected (RV-1)
    /// instead of waiting for the next key.
    pub(super) fn term_review_refresh(&mut self, fx: &mut ReaderEffects) {
        let Some(m) = self
            .review
            .cur
            .as_ref()
            .filter(|m| m.source == LineSource::Terminal)
        else {
            return;
        };
        if self.review.rr_check.is_some() {
            let (nav, now) = (m.surface, self.now);
            self.term_review_model(nav, now, fx);
        }
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
