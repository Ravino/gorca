//! The native text window (ADR-0007 section 6): a bounded copy of the text around the caret, kept
//! in step with the application by splicing its `text-changed` events, plus a small cache of the
//! lines the toolkit reported. Character, word and paragraph units are answered from it with no
//! IPC (TW-3); a unit it cannot answer says what it needs ([`Need`]).
//!
//! All offsets are Unicode scalar offsets. The window is a `Vec<char>`: a window is at most
//! [`MAX_WINDOW_CHARS`] long and every operation on it is a bounded slice operation.

use std::ops::Range;
use std::sync::Arc;

use gorca_model::TextWindowFill;
use gorca_text::space::{CharsSpace, Lookup, Need, TextSpace, Unit};

/// A window never holds more than this many scalars (64 KiB of text at most 4 bytes each is
/// 16 K scalars; this is generous for Latin and exact for the worst case).
pub const MAX_WINDOW_CHARS: usize = 16 * 1024;
/// Toolkit lines remembered per window (TW-3).
pub const MAX_LINES: usize = 16;

/// One toolkit line, as `GetStringAtOffset(LINE)` reported it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LineSpan {
    pub range: Range<u32>,
    pub text: Arc<str>,
}

/// The text around the caret. Contents are secret for a password field; the `Debug` output
/// never shows them.
#[derive(Clone)]
pub struct TextWindow {
    /// Scalar offset of `chars[0]`.
    base: u32,
    chars: Vec<char>,
    /// Length of the whole text in scalars, once known.
    len: Option<u32>,
    lines: Vec<LineSpan>,
    /// Bumped by every splice, refill and invalidation: a fetch reply is applied only if the
    /// revision it was issued under is still current.
    rev: u64,
}

impl std::fmt::Debug for TextWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextWindow")
            .field("base", &self.base)
            .field("chars", &self.chars.len())
            .field("len", &self.len)
            .field("lines", &self.lines.len())
            .field("rev", &self.rev)
            .finish()
    }
}

impl Default for TextWindow {
    fn default() -> Self {
        Self::new()
    }
}

fn to_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl TextWindow {
    pub const fn new() -> Self {
        Self {
            base: 0,
            chars: Vec::new(),
            len: None,
            lines: Vec::new(),
            rev: 0,
        }
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    pub fn len(&self) -> Option<u32> {
        self.len
    }

    pub fn base(&self) -> u32 {
        self.base
    }

    /// Scalars held.
    #[cfg(test)]
    pub fn held(&self) -> usize {
        self.chars.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.chars.is_empty()
    }

    pub fn end(&self) -> u32 {
        self.base.saturating_add(to_u32(self.chars.len()))
    }

    /// Does the window hold the whole text?
    pub fn whole(&self) -> bool {
        self.base == 0 && self.len == Some(to_u32(self.chars.len()))
    }

    /// The scalars held, for reading units straight out of the window.
    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    /// Does the window hold `r`?
    pub fn covers(&self, r: &Range<u32>) -> bool {
        r.start >= self.base && r.end <= self.end()
    }

    /// Forget everything but the revision (the text is refilled lazily).
    pub fn invalidate(&mut self) {
        self.zero();
        self.chars.clear();
        self.lines.clear();
        self.len = None;
        self.rev += 1;
    }

    /// Overwrite the held text, so a password does not linger in freed memory (PW-4).
    pub fn zero(&mut self) {
        self.chars.fill('\0');
        std::hint::black_box(&self.chars);
        for l in &mut self.lines {
            l.text = Arc::from("");
        }
    }

    /// Replace the window by a fetched fill. The text is clipped to [`MAX_WINDOW_CHARS`].
    pub fn fill(&mut self, f: &TextWindowFill) {
        self.zero();
        self.chars.clear();
        self.chars.extend(f.text.chars().take(MAX_WINDOW_CHARS));
        self.base = f.base;
        self.len = Some(f.len);
        self.lines.clear();
        self.rev += 1;
    }

    /// Apply an edit of the application: `removed` scalars at `at` replaced by `ins`.
    pub fn splice(&mut self, at: u32, removed: u32, ins: &str) {
        let n_ins = to_u32(ins.chars().count());
        if let Some(l) = self.len {
            self.len = Some(l.saturating_sub(removed).saturating_add(n_ins));
        }
        self.lines.clear();
        self.rev += 1;
        let (wb, we) = (self.base, self.end());
        if at > we {
            // Behind the window: only the length changed.
            return;
        }
        let edit_end = at.saturating_add(removed);
        if edit_end < wb {
            // In front of the window: it moves.
            self.base = wb.saturating_sub(removed).saturating_add(n_ins);
            return;
        }
        let rs = at.max(wb) - wb;
        let re = edit_end.min(we) - wb;
        let (rs, re) = (rs as usize, (re as usize).max(rs as usize));
        let mut new: Vec<char> = Vec::with_capacity(self.chars.len() + ins.len());
        if at < wb {
            // The edit starts before the window and reaches into it: what is left of the old
            // window follows the inserted text, so the window now starts at the edit.
            new.extend(ins.chars());
            new.extend_from_slice(self.chars.get(re..).unwrap_or(&[]));
            self.base = at;
        } else {
            new.extend_from_slice(self.chars.get(..rs).unwrap_or(&[]));
            new.extend(ins.chars());
            new.extend_from_slice(self.chars.get(re..).unwrap_or(&[]));
        }
        self.zero();
        self.chars = new;
        if self.chars.len() > MAX_WINDOW_CHARS {
            self.chars.truncate(MAX_WINDOW_CHARS);
        }
    }

    /// Read `r` out of the window; `None` when it does not hold all of it.
    pub fn read(&self, r: Range<u32>) -> Option<String> {
        if !self.covers(&r) || r.start > r.end {
            return None;
        }
        let a = (r.start - self.base) as usize;
        let b = (r.end - self.base) as usize;
        Some(self.chars.get(a..b)?.iter().collect())
    }

    /// Remember a toolkit line.
    pub fn add_line(&mut self, range: Range<u32>, text: Arc<str>) {
        if range.is_empty() && text.is_empty() {
            return;
        }
        if self.lines.len() >= MAX_LINES {
            self.lines.remove(0);
        }
        self.lines.retain(|l| l.range != range);
        self.lines.push(LineSpan { range, text });
    }

    pub fn line_at(&self, at: u32) -> Option<&LineSpan> {
        self.lines
            .iter()
            .find(|l| l.range.start <= at && at < l.range.end)
    }

    /// Is there no line break between the offsets, as far as the window knows? `false` when it
    /// cannot tell.
    pub fn same_line(&self, a: u32, b: u32) -> bool {
        let r = a.min(b)..a.max(b);
        self.read(r).is_some_and(|s| !s.contains('\n'))
    }
}

/// The window as a [`TextSpace`].
#[derive(Debug)]
pub struct NativeText<'a> {
    win: &'a TextWindow,
    /// A single-line entry: its only line is the whole text.
    single_line: bool,
}

impl<'a> NativeText<'a> {
    pub fn new(win: &'a TextWindow, single_line: bool) -> Self {
        Self { win, single_line }
    }

    /// Is `r` (relative to the window start) clear of the window's edges, unless the edge is
    /// the end of the text? A unit that touches an edge may continue beyond it.
    fn interior(&self, r: &Range<usize>) -> bool {
        let w = self.win;
        let at_text_start = w.base == 0;
        let at_text_end = w.len == Some(w.end());
        (r.start > 0 || at_text_start) && (r.end < w.chars.len() || at_text_end)
    }
}

impl TextSpace for NativeText<'_> {
    fn len(&self) -> Option<u32> {
        self.win.len
    }

    fn unit_at(&self, at: u32, unit: Unit) -> Lookup<Range<u32>> {
        let w = self.win;
        if w.len.is_none() {
            return Lookup::Need(Need::Length);
        }
        let len = w.len.unwrap_or(0);
        let at = at.min(len);
        match unit {
            Unit::Line => {
                if self.single_line && w.whole() {
                    return Lookup::Ready(0..len);
                }
                if let Some(l) = w.line_at(at) {
                    return Lookup::Ready(l.range.clone());
                }
                return Lookup::Need(Need::Line { at });
            }
            Unit::Char | Unit::Word | Unit::Sentence | Unit::Paragraph => {}
        }
        if at < w.base || at > w.end() {
            return Lookup::Need(Need::Window { around: at });
        }
        let rel = (at - w.base) as usize;
        // `max_line` 0: hard lines only; native `Line` never reaches CharsSpace.
        let cs = CharsSpace::new(&w.chars, 0);
        let Lookup::Ready(r) = cs.unit_at(to_u32(rel), unit) else {
            return Lookup::Need(Need::Window { around: at });
        };
        let rr = r.start as usize..r.end as usize;
        if at == len {
            return Lookup::Ready(len..len);
        }
        if !self.interior(&rr) {
            return Lookup::Need(Need::Window { around: at });
        }
        Lookup::Ready(w.base + r.start..w.base + r.end)
    }

    fn read(&self, r: Range<u32>, out: &mut String) -> Lookup<()> {
        match self.win.read(r.clone()) {
            Some(s) => {
                out.push_str(&s);
                Lookup::Ready(())
            }
            None => Lookup::Need(Need::Window { around: r.start }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fill(base: u32, text: &str, len: u32) -> TextWindow {
        let mut w = TextWindow::new();
        w.fill(&TextWindowFill {
            base,
            text: Arc::from(text),
            len,
            caret: None,
        });
        w
    }

    fn unit(w: &TextWindow, at: u32, u: Unit, single: bool) -> Option<String> {
        let sp = NativeText::new(w, single);
        let r = sp.unit_at(at, u).ready()?;
        let mut s = String::new();
        sp.read(r, &mut s).ready()?;
        Some(s)
    }

    #[test]
    fn chars_and_words_come_from_the_window_with_no_fetch() {
        let w = fill(0, "hello big world", 15);
        assert_eq!(unit(&w, 1, Unit::Char, true).as_deref(), Some("e"));
        assert_eq!(unit(&w, 6, Unit::Word, true).as_deref(), Some("big "));
        assert_eq!(unit(&w, 15, Unit::Char, true).as_deref(), Some(""));
    }

    #[test]
    fn a_unit_touching_the_window_edge_asks_for_more() {
        // Window [10, 20) of a 100-scalar text.
        let w = fill(10, "abcdefghij", 100);
        let sp = NativeText::new(&w, false);
        assert_eq!(
            sp.unit_at(5, Unit::Char),
            Lookup::Need(Need::Window { around: 5 })
        );
        assert_eq!(
            sp.unit_at(10, Unit::Char),
            Lookup::Need(Need::Window { around: 10 })
        );
        assert_eq!(
            sp.unit_at(19, Unit::Char),
            Lookup::Need(Need::Window { around: 19 })
        );
        assert_eq!(sp.unit_at(15, Unit::Char), Lookup::Ready(15..16));
        // A word cut by the edge may be longer.
        assert!(matches!(
            sp.unit_at(12, Unit::Word),
            Lookup::Need(Need::Window { .. })
        ));
    }

    #[test]
    fn an_unknown_length_asks_for_it() {
        let w = TextWindow::new();
        let sp = NativeText::new(&w, true);
        assert_eq!(sp.unit_at(0, Unit::Char), Lookup::Need(Need::Length));
    }

    #[test]
    fn single_line_entries_are_one_line() {
        let w = fill(0, "one line", 8);
        assert_eq!(unit(&w, 3, Unit::Line, true).as_deref(), Some("one line"));
        // A multi-line text needs the toolkit's line.
        let sp = NativeText::new(&w, false);
        assert_eq!(
            sp.unit_at(3, Unit::Line),
            Lookup::Need(Need::Line { at: 3 })
        );
    }

    #[test]
    fn lines_are_cached_until_an_edit() {
        let mut w = fill(0, "ab\ncd\n", 6);
        w.add_line(3..6, Arc::from("cd\n"));
        let sp = NativeText::new(&w, false);
        assert_eq!(sp.unit_at(4, Unit::Line), Lookup::Ready(3..6));
        w.splice(0, 0, "x");
        let sp = NativeText::new(&w, false);
        assert_eq!(
            sp.unit_at(4, Unit::Line),
            Lookup::Need(Need::Line { at: 4 })
        );
    }

    #[test]
    fn splices_track_the_text() {
        let mut w = fill(0, "hello world", 11);
        w.splice(5, 0, ",");
        assert_eq!(w.read(0..12).as_deref(), Some("hello, world"));
        assert_eq!(w.len(), Some(12));
        w.splice(0, 7, "");
        assert_eq!(w.read(0..5).as_deref(), Some("world"));
        assert_eq!(w.len(), Some(5));
        w.splice(5, 0, "!");
        assert_eq!(w.read(0..6).as_deref(), Some("world!"));
    }

    #[test]
    fn splices_before_and_after_a_partial_window() {
        // Window [10, 15) of a 30-scalar text.
        let mut w = fill(10, "abcde", 30);
        w.splice(25, 0, "zz");
        assert_eq!((w.base(), w.held(), w.len()), (10, 5, Some(32)));
        w.splice(0, 3, "");
        assert_eq!((w.base(), w.len()), (7, Some(29)));
        assert_eq!(w.read(7..12).as_deref(), Some("abcde"));
        // An edit that reaches into the window from the left.
        w.splice(5, 4, "XY");
        assert_eq!(w.base(), 5);
        assert_eq!(w.read(5..10).as_deref(), Some("XYcde"));
        // A delete across the right edge keeps the part before it.
        let mut w = fill(10, "abcde", 30);
        w.splice(12, 20, "");
        assert_eq!(w.read(10..12).as_deref(), Some("ab"));
        assert_eq!(w.len(), Some(10));
    }

    #[test]
    fn invalidating_drops_the_text_and_bumps_the_revision() {
        let mut w = fill(0, "abc", 3);
        let r = w.rev();
        w.invalidate();
        assert!(w.is_empty() && w.len().is_none());
        assert!(w.rev() > r);
    }

    #[test]
    fn debug_never_prints_the_text() {
        let w = fill(0, "hunter2", 7);
        let d = format!("{w:?}");
        assert!(!d.contains("hunter2"), "{d}");
        assert!(d.contains("chars: 7"));
    }

    #[test]
    fn the_window_is_bounded() {
        let big: String = "a".repeat(MAX_WINDOW_CHARS + 500);
        let w = fill(0, &big, 70_000);
        assert_eq!(w.held(), MAX_WINDOW_CHARS);
    }

    #[test]
    fn same_line_needs_the_window() {
        let w = fill(0, "ab\ncd", 5);
        assert!(w.same_line(0, 2));
        assert!(!w.same_line(1, 4));
        assert!(!w.same_line(1, 50));
    }
}
