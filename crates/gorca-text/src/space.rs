// SPDX-License-Identifier: GPL-3.0-or-later
//! `TextSpace`: the one trait through which native text, browse-mode flat text and review read
//! units (ADR-0007 section 3, TL-1 to TL-4), plus [`CharsSpace`], the implementation over a
//! resident `&[char]` that the document mirror and the native window both delegate to.

use std::ops::Range;

use gorca_model::WordLevel;

use crate::seg;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Unit {
    Char,
    Word,
    Line,
    Sentence,
    Paragraph,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Dir {
    Prev,
    Next,
}

/// What a native text space needs fetched before it can answer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Need {
    /// The window must cover this scalar offset.
    Window { around: u32 },
    /// The toolkit line at this offset (native `Line` unit, T-F4).
    Line { at: u32 },
    /// The text length.
    Length,
}

/// Answer of a text space: ready now, or the data it needs first (native windows only).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Lookup<T> {
    Ready(T),
    Need(Need),
}

impl<T> Lookup<T> {
    pub fn ready(self) -> Option<T> {
        match self {
            Lookup::Ready(v) => Some(v),
            Lookup::Need(_) => None,
        }
    }

    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Lookup<U> {
        match self {
            Lookup::Ready(v) => Lookup::Ready(f(v)),
            Lookup::Need(n) => Lookup::Need(n),
        }
    }
}

/// Unwrap a [`Lookup::Ready`] or return the `Need` from the enclosing function.
#[macro_export]
macro_rules! ready {
    ($e:expr) => {
        match $e {
            $crate::Lookup::Ready(v) => v,
            $crate::Lookup::Need(n) => return $crate::Lookup::Need(n),
        }
    };
}

/// Result of stepping one unit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Step {
    Moved(Range<u32>),
    /// No further unit in that direction; the range is the current one.
    Edge(Range<u32>),
}

/// A readable text. All offsets are Unicode scalar offsets (TU-1). Implementations never
/// panic on any offset: out-of-range offsets clamp (TL-4, T-3).
pub trait TextSpace {
    /// Length in scalars, if known.
    fn len(&self) -> Option<u32>;

    fn is_empty(&self) -> bool {
        self.len() == Some(0)
    }

    /// The `unit` containing `at`. At `at == len` the answer is the empty range `len..len`
    /// (or, for `Line`, the last empty line).
    fn unit_at(&self, at: u32, unit: Unit) -> Lookup<Range<u32>>;

    /// Append the text in `r` to `out`.
    fn read(&self, r: Range<u32>, out: &mut String) -> Lookup<()>;

    /// The unit after (`Next`) or before (`Prev`) the one containing `at`.
    fn step(&self, at: u32, unit: Unit, dir: Dir) -> Lookup<Step> {
        let cur = ready!(self.unit_at(at, unit));
        match dir {
            Dir::Next => {
                let Some(len) = self.len() else {
                    return Lookup::Need(Need::Length);
                };
                if cur.end >= len {
                    Lookup::Ready(Step::Edge(cur))
                } else {
                    self.unit_at(cur.end, unit).map(Step::Moved)
                }
            }
            Dir::Prev => {
                if cur.start == 0 {
                    Lookup::Ready(Step::Edge(cur))
                } else {
                    self.unit_at(cur.start - 1, unit).map(Step::Moved)
                }
            }
        }
    }
}

/// A [`TextSpace`] over a resident slice of scalars.
///
/// `Paragraph` is a hard line (up to and including `\n`). `Line` is a hard line split into soft
/// lines of at most `max_line` scalars (0 = no wrapping; browse mode uses 100, TL-3). `Word` and
/// `Sentence` never cross a hard line; a `\n` is a word of its own.
#[derive(Copy, Clone, Debug)]
pub struct CharsSpace<'a> {
    chars: &'a [char],
    max_line: u16,
    word: WordLevel,
}

impl<'a> CharsSpace<'a> {
    pub fn new(chars: &'a [char], max_line: u16) -> Self {
        Self {
            chars,
            max_line,
            word: WordLevel::Fine,
        }
    }

    /// The same text with another word level (default: fine).
    #[must_use]
    pub fn with_word_level(mut self, word: WordLevel) -> Self {
        self.word = word;
        self
    }

    pub fn chars(&self) -> &'a [char] {
        self.chars
    }

    fn paragraph(&self, at: usize) -> Range<usize> {
        let s = self.chars;
        let at = at.min(s.len());
        let start = s[..at]
            .iter()
            .rposition(|c| *c == '\n')
            .map_or(0, |p| p + 1);
        let end = s[at..]
            .iter()
            .position(|c| *c == '\n')
            .map_or(s.len(), |p| at + p + 1);
        start..end
    }

    fn unit_at_usize(&self, at: usize, unit: Unit) -> Range<usize> {
        let s = self.chars;
        if s.is_empty() {
            return 0..0;
        }
        if at >= s.len() {
            // Past the end: the empty range, except that a text not ending in `\n` has its
            // last unit ending at len, which is what the caret at the very end sits on.
            return s.len()..s.len();
        }
        match unit {
            Unit::Char => seg::char_range(s, at),
            Unit::Paragraph => self.paragraph(at),
            Unit::Line => {
                let p = self.paragraph(at);
                if self.max_line == 0 {
                    return p;
                }
                for r in seg::soft_lines(&s[p.clone()], self.max_line) {
                    let abs = p.start + r.start..p.start + r.end;
                    if at < abs.end {
                        return abs;
                    }
                }
                p
            }
            Unit::Word => {
                if s[at] == '\n' {
                    return at..at + 1;
                }
                let p = self.paragraph(at);
                let content_end = if s[p.end - 1] == '\n' {
                    p.end - 1
                } else {
                    p.end
                };
                let r = seg::word_range_level(&s[p.start..content_end], at - p.start, self.word);
                p.start + r.start..p.start + r.end
            }
            Unit::Sentence => {
                let p = self.paragraph(at);
                let r = seg::sentence_range(&s[p.clone()], at - p.start);
                p.start + r.start..p.start + r.end
            }
        }
    }
}

fn to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

impl TextSpace for CharsSpace<'_> {
    fn len(&self) -> Option<u32> {
        Some(to_u32(self.chars.len()))
    }

    /// Like the default, but a text that ends in `\n` has a final empty line and paragraph
    /// that the caret can move onto.
    fn step(&self, at: u32, unit: Unit, dir: Dir) -> Lookup<Step> {
        let cur = self.unit_at_usize(at as usize, unit);
        let len = self.chars.len();
        let r = |x: Range<usize>| to_u32(x.start)..to_u32(x.end);
        match dir {
            Dir::Next if cur.end >= len => {
                let tail_line = matches!(unit, Unit::Line | Unit::Paragraph)
                    && !cur.is_empty()
                    && self.chars.last() == Some(&'\n');
                Lookup::Ready(if tail_line {
                    Step::Moved(to_u32(len)..to_u32(len))
                } else {
                    Step::Edge(r(cur))
                })
            }
            Dir::Next => Lookup::Ready(Step::Moved(r(self.unit_at_usize(cur.end, unit)))),
            Dir::Prev if cur.start == 0 => Lookup::Ready(Step::Edge(r(cur))),
            Dir::Prev => Lookup::Ready(Step::Moved(r(self.unit_at_usize(cur.start - 1, unit)))),
        }
    }

    fn unit_at(&self, at: u32, unit: Unit) -> Lookup<Range<u32>> {
        let r = self.unit_at_usize(at as usize, unit);
        Lookup::Ready(to_u32(r.start)..to_u32(r.end))
    }

    fn read(&self, r: Range<u32>, out: &mut String) -> Lookup<()> {
        let end = (r.end as usize).min(self.chars.len());
        let start = (r.start as usize).min(end);
        out.extend(&self.chars[start..end]);
        Lookup::Ready(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn unit(space: &CharsSpace<'_>, at: u32, u: Unit) -> String {
        let r = space.unit_at(at, u).ready().unwrap_or(0..0);
        let mut out = String::new();
        let _ = space.read(r, &mut out);
        out
    }

    #[test]
    fn units_over_two_paragraphs() {
        let c = cs("Привет, мир. Ещё раз.\nvery long second line here\n");
        let sp = CharsSpace::new(&c, 13);
        assert_eq!(unit(&sp, 1, Unit::Char), "р");
        assert_eq!(unit(&sp, 1, Unit::Word), "Привет");
        assert_eq!(unit(&sp, 0, Unit::Sentence), "Привет, мир. ");
        assert_eq!(unit(&sp, 14, Unit::Sentence), "Ещё раз.\n");
        assert_eq!(unit(&sp, 3, Unit::Paragraph), "Привет, мир. Ещё раз.\n");
        assert_eq!(unit(&sp, 3, Unit::Line), "Привет, мир. ");
        assert_eq!(unit(&sp, 20, Unit::Word), ".");
        assert_eq!(unit(&sp, 21, Unit::Word), "\n");
    }

    #[test]
    fn step_walks_lines_and_reports_edges() {
        let c = cs("one\ntwo\n");
        let sp = CharsSpace::new(&c, 0);
        assert_eq!(
            sp.step(0, Unit::Line, Dir::Next),
            Lookup::Ready(Step::Moved(4..8))
        );
        assert_eq!(
            sp.step(4, Unit::Line, Dir::Next),
            Lookup::Ready(Step::Moved(8..8))
        );
        assert_eq!(
            sp.step(8, Unit::Line, Dir::Next),
            Lookup::Ready(Step::Edge(8..8))
        );
        assert_eq!(
            sp.step(5, Unit::Line, Dir::Prev),
            Lookup::Ready(Step::Moved(0..4))
        );
        assert_eq!(
            sp.step(2, Unit::Line, Dir::Prev),
            Lookup::Ready(Step::Edge(0..4))
        );
    }

    #[test]
    fn out_of_range_offsets_clamp() {
        let c = cs("ab");
        let sp = CharsSpace::new(&c, 0);
        assert_eq!(sp.unit_at(99, Unit::Word), Lookup::Ready(2..2));
        let empty = CharsSpace::new(&[], 0);
        assert_eq!(empty.unit_at(5, Unit::Line), Lookup::Ready(0..0));
        let mut out = String::new();
        let _ = sp.read(1..99, &mut out);
        assert_eq!(out, "b");
    }
}
