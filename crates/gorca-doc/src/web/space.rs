//! `WebSpace`: the document flat text as a [`gorca_text::TextSpace`] (ADR-0007 section 3, browse
//! design FT-7, FT-8). Every lookup is `Ready`: the mirror is resident, so no step needs IPC.
//!
//! A *paragraph* is a hard line (block boundary or `\n`) including its terminating `\n`; a *line*
//! is a paragraph split into soft lines of at most `max_line` scalars (NVDA `maxLineLength`).
//! Units tile the text, as `gorca_text::CharsSpace` does for a resident slice, and every rule is
//! delegated to it, so browse mode, native fields and review segment text identically.
//!
//! A hard line longer than [`LONG_LINE`] scalars is not read whole: soft lines are wrapped inside
//! a window of at most two checkpoints ([`CHECKPOINT`] scalars apart, FT-7), which bounds the
//! cost of a step at the price of an approximate wrap at the window edges.

use std::ops::Range;

use gorca_text::{CharsSpace, Lookup, TextSpace, Unit, seg};

use super::doc::WebDoc;

/// Hard lines longer than this are wrapped from checkpoints (FT-7).
pub const LONG_LINE: u32 = 64 * 1024;
/// Distance between wrap checkpoints inside a long hard line.
pub const CHECKPOINT: u32 = 4 * 1024;
/// Scalars looked at on each side of a position when finding a grapheme cluster.
const CHAR_WINDOW: u32 = 48;

/// Borrowed view of a [`WebDoc`] as a text space.
#[derive(Copy, Clone, Debug)]
pub struct WebSpace<'a> {
    doc: &'a WebDoc,
    max_line: u16,
}

impl<'a> WebSpace<'a> {
    /// `max_line` is `browse.max_line_length` (default 100); 0 disables soft wrapping.
    pub fn new(doc: &'a WebDoc, max_line: u16) -> Self {
        Self { doc, max_line }
    }

    pub fn doc(&self) -> &'a WebDoc {
        self.doc
    }

    /// The paragraph (hard line including its `\n`) containing `at`, `at < len`.
    fn paragraph(&self, at: u32) -> Range<u32> {
        let total = self.doc.text_len();
        let (ls, le) = self.doc.line_range(at);
        let end = if le < total { le + 1 } else { total };
        ls..end
    }

    /// The window of a paragraph that is read to answer a question about `at`.
    fn window(para: &Range<u32>, at: u32) -> Range<u32> {
        if para.end - para.start <= LONG_LINE {
            return para.clone();
        }
        let k = (at - para.start) / CHECKPOINT;
        let ws = para.start + k * CHECKPOINT;
        ws..para.end.min(ws + 2 * CHECKPOINT)
    }

    fn chars_in(&self, r: &Range<u32>) -> Vec<char> {
        let mut s = String::with_capacity((r.end - r.start) as usize);
        self.doc.read_text(r.start, r.end, &mut s);
        s.chars().collect()
    }

    /// True when the paragraph containing `at` is long enough to be wrapped from checkpoints.
    pub fn is_checkpointed(&self, at: u32) -> bool {
        let total = self.doc.text_len();
        if at >= total {
            return false;
        }
        let p = self.paragraph(at);
        p.end - p.start > LONG_LINE
    }
}

impl TextSpace for WebSpace<'_> {
    fn len(&self) -> Option<u32> {
        Some(self.doc.text_len())
    }

    fn unit_at(&self, at: u32, unit: Unit) -> Lookup<Range<u32>> {
        let total = self.doc.text_len();
        if total == 0 || at >= total {
            return Lookup::Ready(total..total);
        }
        if unit == Unit::Char {
            let lo = at.saturating_sub(CHAR_WINDOW);
            let hi = total.min(at + CHAR_WINDOW + 1);
            let chars = self.chars_in(&(lo..hi));
            let r = seg::char_range(&chars, (at - lo) as usize);
            return Lookup::Ready(shift(&r, lo));
        }
        let para = self.paragraph(at);
        if unit == Unit::Paragraph {
            return Lookup::Ready(para);
        }
        let win = Self::window(&para, at);
        // A line that fits needs no text at all.
        if unit == Unit::Line
            && (self.max_line == 0 || win.end - win.start <= u32::from(self.max_line))
        {
            return Lookup::Ready(win);
        }
        let chars = self.chars_in(&win);
        let space = CharsSpace::new(&chars, self.max_line);
        let r = space.unit_at(at - win.start, unit).ready().unwrap_or(0..0);
        Lookup::Ready(r.start + win.start..r.end + win.start)
    }

    fn read(&self, r: Range<u32>, out: &mut String) -> Lookup<()> {
        self.doc.read_text(r.start, r.end, out);
        Lookup::Ready(())
    }
}

fn shift(r: &Range<usize>, by: u32) -> Range<u32> {
    let f = |v: usize| u32::try_from(v).unwrap_or(u32::MAX).saturating_add(by);
    f(r.start)..f(r.end)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::Role;
    use gorca_text::{Dir, Step};

    use super::super::cats::{NodeFlags, NodeSpec, SubtreeSpec};
    use super::super::ops::DocOp;
    use super::*;

    fn doc_of(parts: &[(&str, bool)]) -> WebDoc {
        let mut d = WebDoc::new(1);
        let kids: Vec<SubtreeSpec> = parts
            .iter()
            .map(|(t, block)| {
                let mut n = NodeSpec::new(Role::StaticText).text(t);
                if *block {
                    n = n.flags(NodeFlags::BREAK_BEFORE);
                }
                SubtreeSpec::leaf(n)
            })
            .collect();
        let spec = SubtreeSpec::with_children(NodeSpec::new(Role::Document), kids);
        assert!(
            d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(spec),
            })
            .is_ok()
        );
        d
    }

    fn text_of(sp: &WebSpace<'_>, r: Range<u32>) -> String {
        let mut s = String::new();
        let _ = sp.read(r, &mut s);
        s
    }

    #[test]
    fn lines_follow_block_breaks_and_include_the_terminator() {
        let d = doc_of(&[("one", false), ("two", true), ("three four", true)]);
        let sp = WebSpace::new(&d, 0);
        assert_eq!(d.text_in(0, d.text_len()), "one\ntwo\nthree four");
        let l0 = sp.unit_at(1, Unit::Line).ready().unwrap_or(0..0);
        assert_eq!(text_of(&sp, l0), "one\n");
        let l1 = sp.unit_at(5, Unit::Line).ready().unwrap_or(0..0);
        assert_eq!(text_of(&sp, l1), "two\n");
        let l2 = sp.unit_at(12, Unit::Line).ready().unwrap_or(0..0);
        assert_eq!(text_of(&sp, l2), "three four");
        let w = sp.unit_at(8, Unit::Word).ready().unwrap_or(0..0);
        assert_eq!(text_of(&sp, w), "three ");
        let c = sp.unit_at(3, Unit::Char).ready().unwrap_or(0..0);
        assert_eq!(text_of(&sp, c), "\n");
    }

    #[test]
    fn soft_lines_split_long_hard_lines_at_word_boundaries() {
        let long = "alpha beta gamma delta epsilon zeta eta theta iota kappa";
        let d = doc_of(&[(long, false)]);
        let sp = WebSpace::new(&d, 20);
        let mut at = 0;
        let mut lines = Vec::new();
        loop {
            let r = sp.unit_at(at, Unit::Line).ready().unwrap_or(0..0);
            lines.push(text_of(&sp, r.clone()));
            if r.end >= d.text_len() {
                break;
            }
            at = r.end;
        }
        assert!(lines.len() >= 3, "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= 20));
        assert_eq!(lines.concat(), long);
    }

    #[test]
    fn step_matches_chars_space_on_the_same_text() {
        let d = doc_of(&[("ab cd", false), ("ef", true), ("g", true)]);
        let flat: Vec<char> = d.text_in(0, d.text_len()).chars().collect();
        let web = WebSpace::new(&d, 4);
        let reference = CharsSpace::new(&flat, 4);
        for at in 0..d.text_len() {
            for unit in [Unit::Char, Unit::Word, Unit::Line, Unit::Paragraph] {
                assert_eq!(
                    web.unit_at(at, unit),
                    reference.unit_at(at, unit),
                    "{unit:?} at {at}"
                );
                for dir in [Dir::Next, Dir::Prev] {
                    let (a, b) = (web.step(at, unit, dir), reference.step(at, unit, dir));
                    // The web text has no trailing newline, so only compare moves that exist.
                    if let (Lookup::Ready(Step::Moved(x)), Lookup::Ready(Step::Moved(y))) = (&a, &b)
                    {
                        assert_eq!(x, y, "{unit:?} {dir:?} at {at}");
                    }
                }
            }
        }
    }

    #[test]
    fn out_of_range_clamps_and_empty_doc_is_empty() {
        let d = doc_of(&[("x", false)]);
        let sp = WebSpace::new(&d, 0);
        assert_eq!(sp.unit_at(99, Unit::Word), Lookup::Ready(1..1));
        let e = WebDoc::new(0);
        let es = WebSpace::new(&e, 100);
        assert_eq!(es.unit_at(3, Unit::Line), Lookup::Ready(0..0));
        assert_eq!(es.len(), Some(0));
    }

    #[test]
    fn long_hard_lines_are_wrapped_from_checkpoints() {
        let word = "abcdefghi ";
        let long: String = word.repeat((LONG_LINE as usize / word.len()) + 400);
        let d = doc_of(&[(&long, false)]);
        let sp = WebSpace::new(&d, 100);
        assert!(sp.is_checkpointed(10));
        let at = LONG_LINE / 2 + 7;
        let r = sp.unit_at(at, Unit::Line).ready().unwrap_or(0..0);
        assert!(r.start <= at && at < r.end);
        assert!(r.end - r.start <= 100);
    }
}
