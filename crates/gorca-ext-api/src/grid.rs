// SPDX-License-Identifier: MIT OR Apache-2.0
//! Read-only rows of a terminal snapshot (`TermSnapshot::text`): the contract helper both sides
//! share, so the core can review a screen and read the unit at the caret without knowing which
//! extension produced it.
//!
//! The text is the visible screen, rows joined by `'\n'`, offsets in Unicode scalars (the same
//! geometry as `ext-vte`'s `Screen`, which a parity test there keeps in step). A text that ends
//! in `'\n'` has no phantom last row; a caret at its end sits on the empty row after it.

use crate::terminal::{Unit, WordLevel, word_at_level};

/// Rows kept per screen (a hostile application cannot make the index unbounded).
pub const MAX_ROWS: usize = 4096;

/// The rows of one screen text.
#[derive(Clone, Debug, Default)]
pub struct Rows<'a> {
    text: &'a str,
    /// `(byte start, scalar start)` of each row.
    starts: Vec<(usize, u32)>,
    chars: u32,
    ends_nl: bool,
}

impl<'a> Rows<'a> {
    pub fn new(text: &'a str) -> Self {
        let mut starts: Vec<(usize, u32)> = Vec::new();
        let mut chars: u32 = 0;
        if !text.is_empty() {
            starts.push((0, 0));
            for (b, c) in text.char_indices() {
                chars = chars.saturating_add(1);
                if c == '\n' && starts.len() < MAX_ROWS {
                    starts.push((b + 1, chars));
                }
            }
            // The text ended with '\n': no phantom last row.
            if text.ends_with('\n') && starts.last().is_some_and(|s| s.0 == text.len()) {
                starts.pop();
            }
        }
        Rows {
            text,
            starts,
            chars,
            ends_nl: text.ends_with('\n'),
        }
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Row `i` without its newline (`""` past the end).
    pub fn line(&self, i: usize) -> &'a str {
        let Some(&(s, _)) = self.starts.get(i) else {
            return "";
        };
        let e = match self.starts.get(i + 1) {
            Some(&(n, _)) => n.saturating_sub(1),
            None => self.text.len() - usize::from(self.ends_nl),
        };
        self.text.get(s..e.max(s)).unwrap_or("")
    }

    /// Row index and column (in scalars) of scalar offset `off`; past the end it is the end of
    /// the last row.
    pub fn locate(&self, off: u32) -> (usize, u32) {
        if self.starts.is_empty() {
            return (0, 0);
        }
        if off >= self.chars && self.ends_nl {
            return (self.starts.len(), 0);
        }
        let idx = self
            .starts
            .partition_point(|&(_, c)| c <= off)
            .saturating_sub(1);
        let col = off.saturating_sub(self.starts.get(idx).map_or(0, |s| s.1));
        if idx + 1 == self.starts.len() {
            let len = u32::try_from(self.line(idx).chars().count()).unwrap_or(u32::MAX);
            return (idx, col.min(len));
        }
        (idx, col)
    }
}

/// The character, word or line at scalar offset `caret` of a screen `text`: the unit a review or
/// "say current" command reads. `None` for a blank position.
pub fn unit_at(text: &str, caret: u32, unit: Unit, level: WordLevel) -> Option<String> {
    let rows = Rows::new(text);
    let (row, col) = rows.locate(caret);
    let line = rows.line(row);
    match unit {
        Unit::Char => line.chars().nth(col as usize).map(String::from),
        Unit::Word => word_at_level(line, col, level),
        Unit::Line | Unit::Text => {
            let t = line.trim_end();
            (!t.trim().is_empty()).then(|| t.to_owned())
        }
    }
}

/// The whitespace-delimited word at column `col`, or the one before when `col` is on a blank.
pub fn word_at(line: &str, col: u32) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let mut i = col as usize;
    if i >= chars.len() || chars[i].is_whitespace() {
        let mut j = i.min(chars.len());
        while j > 0 && chars[j - 1].is_whitespace() {
            j -= 1;
        }
        if j == 0 {
            return None;
        }
        i = j - 1;
    }
    let mut s = i;
    while s > 0 && !chars[s - 1].is_whitespace() {
        s -= 1;
    }
    let mut e = i;
    while e + 1 < chars.len() && !chars[e + 1].is_whitespace() {
        e += 1;
    }
    Some(chars[s..=e].iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_and_locate() {
        let r = Rows::new("ab\ncde\n");
        assert_eq!(r.line_count(), 2);
        assert_eq!(r.line(0), "ab");
        assert_eq!(r.line(1), "cde");
        assert_eq!(r.locate(0), (0, 0));
        assert_eq!(r.locate(3), (1, 0));
        assert_eq!(r.locate(5), (1, 2));
        // The caret at the very end is on the row after the last newline.
        assert_eq!(r.locate(7), (2, 0));
        assert_eq!(Rows::new("").locate(4), (0, 0));
        assert_eq!(Rows::new("xy").locate(9), (0, 2));
    }

    #[test]
    fn units_at_the_caret() {
        let t = "echo hello world\nsecond";
        assert_eq!(
            unit_at(t, 5, Unit::Char, WordLevel::Coarse).as_deref(),
            Some("h")
        );
        assert_eq!(
            unit_at(t, 7, Unit::Word, WordLevel::Coarse).as_deref(),
            Some("hello")
        );
        assert_eq!(
            unit_at(t, 20, Unit::Line, WordLevel::Coarse).as_deref(),
            Some("second")
        );
        assert_eq!(unit_at("  \n", 1, Unit::Line, WordLevel::Coarse), None);
    }
}
