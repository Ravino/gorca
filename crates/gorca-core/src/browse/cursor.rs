//! Virtual-cursor moves over the flat text (browse-mode design section 8.1). Pure functions of
//! a published `WebDoc`: no IPC, no allocation beyond the text of the unit being spoken.
//!
//! A cursor is a flat-text offset. Moves go through `gorca_text::TextSpace` (the one segmentation
//! implementation): a *line* is a hard line split at `max_line` scalars, a *word* includes its
//! trailing spaces, a *character* is a grapheme. Blank lines (FT-5) are skipped by line moves
//! and count as one `blank` for character moves.

use std::ops::Range;

use gorca_doc::web::{WebDoc, WebSpace};
use gorca_model::DocNodeId;
use gorca_text::{Dir, Lookup, Step, TextSpace, Unit};

use crate::WebCursor;

/// Steps a blank-skipping move may take before it gives up (a page of nested empty blocks).
const MAX_SKIP: usize = 4096;
/// NVDA `linesPerPage` (`configSpec.py:218`).
pub const LINES_PER_PAGE: u32 = 25;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Edge {
    Top,
    Bottom,
}

/// The result of a move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Mv {
    /// The cursor is now at `off`; `speak` is the range to present.
    Landed {
        off: u32,
        speak: Range<u32>,
        /// A character move onto an empty line: say "blank".
        blank: bool,
    },
    /// Nothing further in that direction.
    Edge(Edge),
}

fn ready<T>(l: Lookup<T>) -> Option<T> {
    l.ready()
}

fn text_of(doc: &WebDoc, r: &Range<u32>) -> String {
    doc.text_in(r.start, r.end)
}

/// Does a line contain nothing a user would hear (only line breaks and white space)?
pub fn is_blank(doc: &WebDoc, r: &Range<u32>) -> bool {
    if r.start >= r.end {
        return true;
    }
    // A line is short; the marker scalar is not white space.
    text_of(doc, r)
        .chars()
        .all(|c| c.is_whitespace() && c != '\u{FFFC}')
}

/// The flat offset of a cursor, if its node is still in the document.
pub fn offset_of(doc: &WebDoc, c: WebCursor) -> Option<u32> {
    let base = doc.offset_of(c.node)?;
    let own = doc.own_len(c.node).unwrap_or(0);
    Some(base + c.local.min(own.saturating_sub(1)))
}

/// The cursor at a flat offset (clamped to the last scalar).
pub fn cursor_at(doc: &WebDoc, off: u32) -> Option<WebCursor> {
    let len = doc.text_len();
    if len == 0 {
        return doc.root().map(|node| WebCursor { node, local: 0 });
    }
    let (node, local) = doc.at_offset(off.min(len - 1))?;
    Some(WebCursor { node, local })
}

/// The position of the first content of a node: after its own line break and after the blank
/// breaks of the blocks that open inside it (a list starts with the list item's break), so a
/// container hit lands on its first line of text (FT-4 stacks one break per nested block).
pub fn content_start(doc: &WebDoc, node: DocNodeId) -> Option<u32> {
    let v = doc.node(node)?;
    let base = doc.offset_of(node)?;
    let br = u32::from(
        v.flags.contains(gorca_doc::web::NodeFlags::BREAK_BEFORE)
            && !v.flags.contains(gorca_doc::web::NodeFlags::HIDDEN),
    );
    let start = base + br;
    let Some((_, end)) = doc.subtree_text_range(node) else {
        return Some(start);
    };
    let limit = end.min(start.saturating_add(BLANK_SKIP));
    if start >= limit {
        return Some(start);
    }
    let lead = doc.text_in(start, limit);
    let blanks = lead.chars().take_while(|c| *c == '\n').count();
    let blanks = u32::try_from(blanks).unwrap_or(0);
    // All breaks and nothing behind them within the window: stay on the node itself.
    if start + blanks >= limit && blanks > 0 {
        return Some(start);
    }
    Some(start + blanks)
}

/// Leading blank breaks skipped at most this far (nested blocks open a few levels deep).
const BLANK_SKIP: u32 = 64;

/// The speakable part of a line: without its terminating line break.
fn trim_break(doc: &WebDoc, r: Range<u32>) -> Range<u32> {
    if r.end > r.start && doc.text_in(r.end - 1, r.end) == "\n" {
        r.start..r.end - 1
    } else {
        r
    }
}

fn line_land(doc: &WebDoc, r: Range<u32>) -> Mv {
    Mv::Landed {
        off: r.start,
        speak: trim_break(doc, r),
        blank: false,
    }
}

/// Next or previous non-blank line from `off`.
pub fn line(doc: &WebDoc, max_line: u16, off: u32, dir: Dir) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    let edge = match dir {
        Dir::Next => Edge::Bottom,
        Dir::Prev => Edge::Top,
    };
    let mut at = off;
    for _ in 0..MAX_SKIP {
        match ready(sp.step(at, Unit::Line, dir)) {
            Some(Step::Moved(r)) => {
                if !is_blank(doc, &r) {
                    return line_land(doc, r);
                }
                at = r.start;
            }
            Some(Step::Edge(_)) | None => return Mv::Edge(edge),
        }
    }
    Mv::Edge(edge)
}

/// The first non-blank line at or after `off` (document start).
pub fn first_line(doc: &WebDoc, max_line: u16) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    match ready(sp.unit_at(0, Unit::Line)) {
        Some(r) if !is_blank(doc, &r) => line_land(doc, r),
        Some(_) | None => line(doc, max_line, 0, Dir::Next),
    }
}

/// The last non-blank line of the document.
pub fn last_line(doc: &WebDoc, max_line: u16) -> Mv {
    let len = doc.text_len();
    let sp = WebSpace::new(doc, max_line);
    match ready(sp.unit_at(len.saturating_sub(1), Unit::Line)) {
        Some(r) if !is_blank(doc, &r) => line_land(doc, r),
        Some(r) => line(doc, max_line, r.start, Dir::Prev),
        None => Mv::Edge(Edge::Bottom),
    }
}

/// `n` lines in `dir`, stopping at the edge; an edge before the first line is an `Edge`.
pub fn page(doc: &WebDoc, max_line: u16, off: u32, dir: Dir, n: u32) -> Mv {
    let mut at = off;
    let mut last: Option<Mv> = None;
    for _ in 0..n {
        match line(doc, max_line, at, dir) {
            Mv::Landed {
                off: o,
                speak,
                blank,
            } => {
                at = o;
                last = Some(Mv::Landed {
                    off: o,
                    speak,
                    blank,
                });
            }
            Mv::Edge(e) => {
                return last.unwrap_or(Mv::Edge(e));
            }
        }
    }
    last.unwrap_or(Mv::Edge(match dir {
        Dir::Next => Edge::Bottom,
        Dir::Prev => Edge::Top,
    }))
}

fn char_is(doc: &WebDoc, p: u32, c: char) -> bool {
    doc.text_in(p, p + 1).starts_with(c)
}

/// Is the `\n` at `p` a whole empty line (blank line), as opposed to the end of a non-empty one?
fn empty_line_at(doc: &WebDoc, p: u32) -> bool {
    if p == 0 {
        return true;
    }
    char_is(doc, p - 1, '\n')
}

/// One character in `dir`. A line break that ends a non-empty line is stepped over; one that is
/// a whole empty line is a `blank` (NVDA).
pub fn character(doc: &WebDoc, max_line: u16, off: u32, dir: Dir) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    let mut at = off;
    for _ in 0..3 {
        let step = match ready(sp.step(at, Unit::Char, dir)) {
            Some(Step::Moved(r)) => r,
            Some(Step::Edge(_)) | None => {
                return Mv::Edge(match dir {
                    Dir::Next => Edge::Bottom,
                    Dir::Prev => Edge::Top,
                });
            }
        };
        if !char_is(doc, step.start, '\n') {
            return Mv::Landed {
                off: step.start,
                speak: step,
                blank: false,
            };
        }
        if empty_line_at(doc, step.start) {
            return Mv::Landed {
                off: step.start,
                speak: step,
                blank: true,
            };
        }
        at = step.start;
    }
    Mv::Edge(match dir {
        Dir::Next => Edge::Bottom,
        Dir::Prev => Edge::Top,
    })
}

/// The next or previous word (white space and line breaks are not words).
pub fn word(doc: &WebDoc, max_line: u16, off: u32, dir: Dir) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    let mut at = off;
    if dir == Dir::Prev
        && let Some(cur) = ready(sp.unit_at(off, Unit::Word))
        && cur.start < off
        && !is_blank(doc, &cur)
    {
        return Mv::Landed {
            off: cur.start,
            speak: cur,
            blank: false,
        };
    }
    for _ in 0..MAX_SKIP {
        let r = match ready(sp.step(at, Unit::Word, dir)) {
            Some(Step::Moved(r)) => r,
            Some(Step::Edge(_)) | None => {
                return Mv::Edge(match dir {
                    Dir::Next => Edge::Bottom,
                    Dir::Prev => Edge::Top,
                });
            }
        };
        if !is_blank(doc, &r) {
            return Mv::Landed {
                off: r.start,
                speak: r,
                blank: false,
            };
        }
        at = r.start;
    }
    Mv::Edge(Edge::Bottom)
}

/// Home: the start of the soft line, speaking the character there.
pub fn line_start(doc: &WebDoc, max_line: u16, off: u32) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    let Some(r) = ready(sp.unit_at(off, Unit::Line)) else {
        return Mv::Edge(Edge::Top);
    };
    char_land(doc, r.start)
}

/// End: the last character of the soft line.
pub fn line_end(doc: &WebDoc, max_line: u16, off: u32) -> Mv {
    let sp = WebSpace::new(doc, max_line);
    let Some(r) = ready(sp.unit_at(off, Unit::Line)) else {
        return Mv::Edge(Edge::Bottom);
    };
    let r = trim_break(doc, r);
    let at = if r.end > r.start { r.end - 1 } else { r.start };
    char_land(doc, at)
}

fn char_land(doc: &WebDoc, at: u32) -> Mv {
    let r = at..(at + 1).min(doc.text_len().max(at));
    let blank = r.start >= r.end || char_is(doc, at, '\n');
    Mv::Landed {
        off: at,
        speak: r,
        blank,
    }
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;
    use std::sync::Arc;

    use gorca_doc::web::{DocOp, NodeFlags, NodeSpec, SubtreeSpec};
    use gorca_model::Role;

    use super::*;

    /// A document whose flat text is exactly `text` (one run per `\n`-separated line, each
    /// after the first with a break before it).
    pub(crate) fn doc_of(text: &str) -> WebDoc {
        let mut d = WebDoc::new(3);
        let kids: Vec<SubtreeSpec> = text
            .split('\n')
            .enumerate()
            .map(|(i, line)| {
                let mut n = NodeSpec::new(Role::StaticText).text(line);
                if i > 0 {
                    n = n.flags(NodeFlags::BREAK_BEFORE);
                }
                SubtreeSpec::leaf(n)
            })
            .collect();
        let spec = SubtreeSpec::with_children(NodeSpec::new(Role::DocumentWeb), kids);
        assert!(
            d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(spec),
            })
            .is_ok()
        );
        assert_eq!(d.text_in(0, d.text_len()), text);
        d
    }

    fn spoken(doc: &WebDoc, m: &Mv) -> Option<(u32, String, bool)> {
        match m {
            Mv::Landed { off, speak, blank } => {
                Some((*off, doc.text_in(speak.start, speak.end), *blank))
            }
            Mv::Edge(_) => None,
        }
    }

    #[test]
    fn lines_skip_blank_lines_and_report_edges() {
        let d = doc_of("one\n\n\ntwo\n \nthree");
        let m = line(&d, 100, 0, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((6, "two".into(), false)));
        let m = line(&d, 100, 6, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((12, "three".into(), false)));
        assert_eq!(line(&d, 100, 12, Dir::Next), Mv::Edge(Edge::Bottom));
        let m = line(&d, 100, 12, Dir::Prev);
        assert_eq!(spoken(&d, &m), Some((6, "two".into(), false)));
        let m = line(&d, 100, 6, Dir::Prev);
        assert_eq!(spoken(&d, &m), Some((0, "one".into(), false)));
        assert_eq!(line(&d, 100, 0, Dir::Prev), Mv::Edge(Edge::Top));
    }

    #[test]
    fn top_bottom_and_page_moves() {
        let d = doc_of("\n\nfirst\nmiddle\nlast\n\n");
        assert_eq!(
            spoken(&d, &first_line(&d, 100)).map(|x| x.1),
            Some("first".into())
        );
        assert_eq!(
            spoken(&d, &last_line(&d, 100)).map(|x| x.1),
            Some("last".into())
        );
        let mut text = String::new();
        for i in 0..60 {
            let _ = writeln!(text, "l{i}");
        }
        let d = doc_of(text.trim_end());
        let m = page(&d, 100, 0, Dir::Next, LINES_PER_PAGE);
        assert_eq!(spoken(&d, &m).map(|x| x.1), Some("l25".into()));
        let m = page(&d, 100, d.text_len() - 2, Dir::Next, LINES_PER_PAGE);
        assert_eq!(m, Mv::Edge(Edge::Bottom));
        let mid = d.line_start(30).unwrap();
        let m = page(&d, 100, mid, Dir::Prev, LINES_PER_PAGE);
        assert_eq!(spoken(&d, &m).map(|x| x.1), Some("l5".into()));
    }

    #[test]
    fn characters_step_over_line_ends_and_say_blank_for_empty_lines() {
        let d = doc_of("ab\n\ncd");
        // "ab\n\ncd": a0 b1 \n2 \n3 c4 d5
        let m = character(&d, 100, 1, Dir::Next);
        assert_eq!(
            spoken(&d, &m),
            Some((3, "\n".into(), true)),
            "empty line is a blank"
        );
        let m = character(&d, 100, 3, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((4, "c".into(), false)));
        let m = character(&d, 100, 4, Dir::Prev);
        assert_eq!(spoken(&d, &m), Some((3, "\n".into(), true)));
        let m = character(&d, 100, 3, Dir::Prev);
        assert_eq!(
            spoken(&d, &m),
            Some((1, "b".into(), false)),
            "the line end is skipped"
        );
        assert_eq!(character(&d, 100, 5, Dir::Next), Mv::Edge(Edge::Bottom));
        assert_eq!(character(&d, 100, 0, Dir::Prev), Mv::Edge(Edge::Top));
        let m = character(&d, 100, 0, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((1, "b".into(), false)));
    }

    #[test]
    fn words_cross_lines_and_prev_word_goes_to_the_start_of_the_current_one() {
        let d = doc_of("alpha beta\ngamma");
        let m = word(&d, 100, 0, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((6, "beta".into(), false)));
        let m = word(&d, 100, 6, Dir::Next);
        assert_eq!(spoken(&d, &m), Some((11, "gamma".into(), false)));
        let m = word(&d, 100, 8, Dir::Prev);
        assert_eq!(
            spoken(&d, &m),
            Some((6, "beta".into(), false)),
            "start of this word"
        );
        let m = word(&d, 100, 6, Dir::Prev);
        assert_eq!(spoken(&d, &m), Some((0, "alpha ".into(), false)));
        assert_eq!(word(&d, 100, 11, Dir::Next), Mv::Edge(Edge::Bottom));
    }

    #[test]
    fn home_and_end_speak_the_character_at_the_line_edge() {
        let d = doc_of("hello world\nsecond");
        let m = line_end(&d, 100, 3);
        assert_eq!(spoken(&d, &m), Some((10, "d".into(), false)));
        let m = line_start(&d, 100, 8);
        assert_eq!(spoken(&d, &m), Some((0, "h".into(), false)));
        let m = line_end(&d, 100, 14);
        assert_eq!(spoken(&d, &m), Some((17, "d".into(), false)));
    }

    #[test]
    fn soft_lines_split_a_long_hard_line() {
        let d = doc_of("alpha beta gamma delta");
        let m = line(&d, 12, 0, Dir::Next);
        assert_eq!(spoken(&d, &m).map(|x| x.0), Some(11));
        let m = line_end(&d, 12, 0);
        assert_eq!(spoken(&d, &m).map(|x| x.0), Some(10));
    }

    #[test]
    fn cursor_offsets_round_trip_through_nodes() {
        let d = doc_of("ab\ncd");
        let c = cursor_at(&d, 4).unwrap();
        assert_eq!(offset_of(&d, c), Some(4));
        let beyond = cursor_at(&d, 99).unwrap();
        assert_eq!(offset_of(&d, beyond), Some(d.text_len() - 1));
    }
}
