//! Browse-mode reading units over the flat text (ADR-0003 section 4.5): character, word and
//! line ranges, and stepping between them. Offsets are Unicode scalar indices into the
//! concatenation of node contributions (own text, plus one `\n` for a `BREAK_AFTER` node).
//!
//! Lines are hard breaks only (block boundaries and newlines); soft wrapping needs extents and
//! is a later refinement. Words never cross a line; the word rule is `gorca_text::seg::word_range`
//! (a word includes its trailing spaces, each punctuation scalar is its own word).

use super::doc::WebDoc;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ReadUnit {
    Char,
    Word,
    Line,
}

/// Word containing `line[idx]` as `[start, end)` (indices into `line`). `idx` is clamped to
/// the last scalar. An empty line yields `(0, 0)`. The rule lives in `gorca_text::seg`
/// (ADR-0007 TL-1).
pub fn word_range_in(line: &[char], idx: usize) -> (usize, usize) {
    let r = gorca_text::seg::word_range(line, idx);
    (r.start, r.end)
}

impl WebDoc {
    /// The flat text in `[start, end)`.
    pub fn text_in(&self, start: u32, end: u32) -> String {
        let mut s = String::new();
        self.read_text(start, end, &mut s);
        s
    }

    /// Range `[start, end)` of the `unit` containing `offset`. `Char` and `Word` return `None`
    /// at or past the end of the text; `Line` accepts `offset == text_len` (an empty last line).
    pub fn unit_range(&self, unit: ReadUnit, offset: u32) -> Option<(u32, u32)> {
        let total = self.text_len();
        match unit {
            ReadUnit::Char => (offset < total).then_some((offset, offset + 1)),
            ReadUnit::Line => (offset <= total).then(|| self.line_range(offset)),
            ReadUnit::Word => {
                if offset >= total {
                    return None;
                }
                let (ls, le) = self.line_range(offset);
                if ls == le {
                    return Some((ls, ls));
                }
                let chars: Vec<char> = self.text_in(ls, le).chars().collect();
                let idx = (offset - ls) as usize;
                let (a, b) = word_range_in(&chars, idx);
                Some((ls + a as u32, ls + b as u32))
            }
        }
    }

    /// The unit after the one containing `offset`.
    pub fn next_unit(&self, unit: ReadUnit, offset: u32) -> Option<(u32, u32)> {
        let total = self.text_len();
        let (_, end) = self.unit_range(unit, offset)?;
        match unit {
            ReadUnit::Char => self.unit_range(unit, end),
            ReadUnit::Line => {
                if end >= total {
                    None
                } else {
                    self.unit_range(unit, end + 1)
                }
            }
            ReadUnit::Word => {
                let (_, le) = self.line_range(offset);
                if end < le {
                    self.unit_range(unit, end)
                } else if le >= total {
                    None
                } else {
                    self.unit_range(unit, le + 1)
                }
            }
        }
    }

    /// The unit before the one containing `offset`.
    pub fn prev_unit(&self, unit: ReadUnit, offset: u32) -> Option<(u32, u32)> {
        let total = self.text_len();
        let offset = offset.min(total);
        let (start, _) = self.unit_range(unit, offset).or_else(|| {
            // Past the end: step back from the last scalar.
            if matches!(unit, ReadUnit::Line) {
                None
            } else {
                self.unit_range(unit, total.checked_sub(1)?)
            }
        })?;
        if start == 0 {
            return None;
        }
        match unit {
            ReadUnit::Char | ReadUnit::Line => self.unit_range(unit, start - 1),
            ReadUnit::Word => {
                let (ls, _) = self.line_range(offset);
                if start > ls {
                    self.unit_range(unit, start - 1)
                } else {
                    // Previous line's last word (or its empty-line unit).
                    let (pls, ple) = self.line_range(start - 1);
                    if pls == ple {
                        Some((pls, pls))
                    } else {
                        self.unit_range(unit, ple - 1)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(s: &str, i: usize) -> String {
        let c: Vec<char> = s.chars().collect();
        let (a, b) = word_range_in(&c, i);
        c[a..b].iter().collect()
    }

    #[test]
    fn words_include_trailing_space() {
        assert_eq!(w("hello  world", 0), "hello  ");
        assert_eq!(w("hello  world", 3), "hello  ");
        assert_eq!(w("hello  world", 6), "hello  ");
        assert_eq!(w("hello  world", 7), "world");
    }

    #[test]
    fn punctuation_is_its_own_word() {
        assert_eq!(w("a, b", 1), ", ");
        assert_eq!(w("a, b", 0), "a");
        assert_eq!(w("a, b", 2), ", ");
    }

    #[test]
    fn leading_spaces_and_empty() {
        assert_eq!(w("  x", 0), "  ");
        assert_eq!(word_range_in(&[], 3), (0, 0));
    }
}
