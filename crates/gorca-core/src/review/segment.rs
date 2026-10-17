//! Text units inside a zone (ADR-0006 6.9, TX-1).
//!
//! M4a ships [`SimpleUnits`]: a word is a maximal run of non-whitespace, a character is an
//! extended grapheme cluster. M3 replaces it with the shared segmentation and verbalizer behind
//! the same trait. Offsets are Unicode scalar offsets (TU-1).

use std::ops::Range;

use smallvec::SmallVec;
use unicode_segmentation::UnicodeSegmentation;

/// A segmentation unit. M3 adds `Line` and `Sentence`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Unit {
    Char,
    Word,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Dir {
    Fwd,
    Back,
}

pub type Ranges = SmallVec<[Range<u32>; 16]>;

pub trait TextUnits: Send + Sync {
    /// Every unit of `text`, in order.
    fn units(&self, text: &str, unit: Unit) -> Ranges;

    /// The unit that contains `at`, else (for whitespace between words) the next one.
    fn at(&self, text: &str, at: u32, unit: Unit) -> Option<Range<u32>> {
        self.units(text, unit).into_iter().find(|r| r.end > at)
    }

    /// The unit after (`Fwd`) or before (`Back`) the unit at `at`, in the same text.
    fn next(&self, text: &str, at: u32, unit: Unit, dir: Dir) -> Option<Range<u32>> {
        let all = self.units(text, unit);
        let cur_start = all
            .iter()
            .find(|r| r.start <= at && at < r.end)
            .map_or(at, |r| r.start);
        match dir {
            Dir::Fwd => all.into_iter().find(|r| r.start > cur_start),
            Dir::Back => all.into_iter().rev().find(|r| r.start < cur_start),
        }
    }
}

#[derive(Copy, Clone, Debug, Default)]
pub struct SimpleUnits;

impl TextUnits for SimpleUnits {
    fn units(&self, text: &str, unit: Unit) -> Ranges {
        let mut out = Ranges::new();
        match unit {
            Unit::Word => {
                let mut start: Option<u32> = None;
                let mut n: u32 = 0;
                for c in text.chars() {
                    if c.is_whitespace() {
                        if let Some(s) = start.take() {
                            out.push(s..n);
                        }
                    } else if start.is_none() {
                        start = Some(n);
                    }
                    n = n.saturating_add(1);
                }
                if let Some(s) = start {
                    out.push(s..n);
                }
            }
            Unit::Char => {
                let mut n: u32 = 0;
                for g in text.graphemes(true) {
                    let len = u32::try_from(g.chars().count()).unwrap_or(u32::MAX);
                    out.push(n..n.saturating_add(len));
                    n = n.saturating_add(len);
                }
            }
        }
        out
    }
}

/// Units of terminal review (ADR-0018): words by the rule caret navigation uses (`gorca_text`
/// TL-1: a word keeps its trailing spaces, each punctuation mark is a word), so that a review
/// word and a caret word agree in the same terminal line; characters as [`SimpleUnits`]. A run
/// of spaces alone is not a word to stop on.
#[derive(Copy, Clone, Debug, Default)]
pub struct TermUnits;

impl TextUnits for TermUnits {
    fn units(&self, text: &str, unit: Unit) -> Ranges {
        if unit == Unit::Char {
            return SimpleUnits.units(text, unit);
        }
        let chars: Vec<char> = text.chars().collect();
        let mut out = Ranges::new();
        let mut i = 0usize;
        while i < chars.len() {
            let r = gorca_text::seg::word_range(&chars, i);
            if r.end <= i {
                break;
            }
            if chars[r.clone()].iter().any(|c| !c.is_whitespace()) {
                out.push(
                    u32::try_from(r.start).unwrap_or(u32::MAX)
                        ..u32::try_from(r.end).unwrap_or(u32::MAX),
                );
            }
            i = r.end;
        }
        out
    }
}

/// The substring of `text` for the scalar range `r`.
pub fn slice(text: &str, r: &Range<u32>) -> String {
    text.chars()
        .skip(r.start as usize)
        .take(r.end.saturating_sub(r.start) as usize)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_are_runs_of_non_whitespace() {
        let u = SimpleUnits;
        let w = u.units("  Save  as...\tnow ", Unit::Word);
        assert_eq!(w.as_slice(), &[2..6, 8..13, 14..17]);
        assert!(u.units("", Unit::Word).is_empty());
        assert!(u.units("   ", Unit::Word).is_empty());
    }

    #[test]
    fn characters_are_grapheme_clusters_in_scalar_offsets() {
        let u = SimpleUnits;
        // "e" + combining acute is one cluster of two scalars; the flag is two scalars too.
        let c = u.units("ae\u{301}\u{1F1E9}\u{1F1EA}b", Unit::Char);
        assert_eq!(c.as_slice(), &[0..1, 1..3, 3..5, 5..6]);
    }

    #[test]
    fn next_and_previous_skip_whitespace_and_stop_at_the_ends() {
        let u = SimpleUnits;
        let t = "ab  cd e";
        assert_eq!(u.next(t, 0, Unit::Word, Dir::Fwd), Some(4..6));
        assert_eq!(u.next(t, 3, Unit::Word, Dir::Fwd), Some(4..6), "in the gap");
        assert_eq!(u.next(t, 7, Unit::Word, Dir::Fwd), None);
        assert_eq!(u.next(t, 5, Unit::Word, Dir::Back), Some(0..2));
        assert_eq!(
            u.next(t, 3, Unit::Word, Dir::Back),
            Some(0..2),
            "in the gap"
        );
        assert_eq!(u.next(t, 0, Unit::Word, Dir::Back), None);
        assert_eq!(u.next(t, 1, Unit::Char, Dir::Fwd), Some(2..3));
        assert_eq!(u.next(t, 0, Unit::Char, Dir::Back), None);
    }

    #[test]
    fn at_finds_the_containing_or_following_unit() {
        let u = SimpleUnits;
        assert_eq!(u.at("ab  cd", 1, Unit::Word), Some(0..2));
        assert_eq!(u.at("ab  cd", 2, Unit::Word), Some(4..6));
        assert_eq!(u.at("ab  cd", 6, Unit::Word), None);
    }

    #[test]
    fn terminal_words_follow_the_caret_word_rule() {
        let u = TermUnits;
        let w = u.units("ls -l --color=auto", Unit::Word);
        let words: Vec<String> = w.iter().map(|r| slice("ls -l --color=auto", r)).collect();
        assert_eq!(
            words,
            ["ls ", "-", "l ", "-", "-", "color", "=", "auto"],
            "punctuation marks are words of their own"
        );
        let t = "  a/b";
        assert_eq!(
            u.units(t, Unit::Word)
                .iter()
                .map(|r| slice(t, r))
                .collect::<Vec<_>>(),
            ["a", "/", "b"].map(String::from),
            "leading spaces are not a word to stop on"
        );
        assert_eq!(u.units("e\u{301}x", Unit::Char).as_slice(), &[0..2, 2..3]);
    }

    #[test]
    fn slice_is_by_scalar() {
        assert_eq!(slice("héllo wörld", &(1..4)), "éll");
    }
}
