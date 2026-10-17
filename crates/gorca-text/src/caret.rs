//! Caret-move classification: which unit to speak for which key (ADR-0007 section 5.2, CR-2 to
//! CR-4). Pure tables and one helper that turns a unit choice into a range over any
//! [`TextSpace`], so native fields and browse mode present identically.

use std::ops::Range;

use gorca_model::{EditQuirks, WordLanding};

use crate::space::{Lookup, TextSpace, Unit};

/// The unit a movement key advances by (mirror of the input lane's `KeyUnit`, ADR-0007 4.1).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum KeyUnit {
    Char,
    Word,
    Line,
    Paragraph,
    LineEdge,
    DocEdge,
    Page,
}

/// What is spoken after a caret move.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Say {
    Char,
    Word,
    Line,
    Paragraph,
}

impl From<Say> for Unit {
    fn from(s: Say) -> Unit {
        match s {
            Say::Char => Unit::Char,
            Say::Word => Unit::Word,
            Say::Line => Unit::Line,
            Say::Paragraph => Unit::Paragraph,
        }
    }
}

/// CR-2: the unit spoken for a correlated move key.
pub fn say_for_key(key: KeyUnit) -> Say {
    match key {
        // Home and End speak the character at the caret (NVDA, Orca).
        KeyUnit::Char | KeyUnit::LineEdge => Say::Char,
        KeyUnit::Word => Say::Word,
        KeyUnit::Line | KeyUnit::Page | KeyUnit::DocEdge => Say::Line,
        KeyUnit::Paragraph => Say::Paragraph,
    }
}

/// CR-4: the unit spoken for a caret move no key explains (script, mouse, input method).
/// A one-scalar step on the same line speaks the character, anything else speaks the line.
pub fn say_for_uncorrelated(old: u32, new: u32, same_line: bool) -> Say {
    say_for_uncorrelated_by(old.abs_diff(new), same_line)
}

/// [`say_for_uncorrelated`] for a distance the caller measured in text scalars: a window that
/// keeps Gecko's phantom U+FEFF after each astral scalar counts an emoji as one step although
/// its offsets differ by two.
pub fn say_for_uncorrelated_by(distance: u32, same_line: bool) -> Say {
    if distance == 1 && same_line {
        Say::Char
    } else {
        Say::Line
    }
}

/// What to present at a caret position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Presented {
    /// Speak the text in this range.
    Text(Range<u32>),
    /// Nothing under the caret (end of text): speak "blank" (CR-2, `Move{Char}`).
    Blank,
}

/// The range to speak for `say` with the caret at `at`.
pub fn present(space: &dyn TextSpace, at: u32, say: Say) -> Lookup<Presented> {
    let Some(len) = space.len() else {
        return Lookup::Need(crate::Need::Length);
    };
    let r = crate::ready!(space.unit_at(at, say.into()));
    if r.start == r.end {
        return Lookup::Ready(if say == Say::Char || at >= len {
            Presented::Blank
        } else {
            Presented::Text(r)
        });
    }
    Lookup::Ready(Presented::Text(r))
}

/// CR-2 for `Move{Word}`: the word to speak when the caret went from `from` to `at`.
///
/// Toolkits disagree on where Ctrl+Right lands (`EditQuirks::word_landing`, measured in the
/// editing matrix). Windows-style toolkits stop at the START of the next word, where the word at
/// the caret is the one just reached ([`WordLanding::Start`]). GTK, GtkSourceView, Gecko, Blink
/// and readline stop at the END of the word they pass ([`WordLanding::End`]), where the word at
/// the caret is the following punctuation, a space, or nothing at the end of the text, and the
/// word just passed is the one that ends at the caret (Orca speaks the traversed text for the
/// same reason: `script_utilities.py`, `get_word_at_offset_adjusted_for_navigation`).
///
/// So for an END toolkit a forward move that landed right after a non-space scalar, at the end
/// of a word, speaks the word passed: the last word of the traversed span that has a letter or a
/// digit in it, so that Gecko, which lands behind the comma of "hello, " after one Ctrl+Right,
/// still speaks "hello" and not ",". A span of punctuation alone (GtkSourceView and Blink stop
/// between a word and its comma) speaks that punctuation. Every other move speaks the word
/// starting at the caret, and `blank` at the end of the text with nothing under the caret (NVDA).
pub fn present_word(
    space: &dyn TextSpace,
    from: Option<u32>,
    at: u32,
    q: EditQuirks,
) -> Lookup<Presented> {
    if q.word_landing == WordLanding::End
        && let Some(f) = from.filter(|f| *f < at)
        && at > 0
        && let Some(r) = crate::ready!(passed_word(space, f, at))
    {
        return Lookup::Ready(Presented::Text(r));
    }
    present(space, at, Say::Word)
}

/// The word passed by a forward move from `from` to `at` (END toolkits), if the move ended at the
/// end of one.
fn passed_word(space: &dyn TextSpace, from: u32, at: u32) -> Lookup<Option<Range<u32>>> {
    let mut end = at;
    let mut first = true;
    loop {
        let w = crate::ready!(space.unit_at(end - 1, Unit::Word));
        let mut t = String::new();
        crate::ready!(space.read(w.clone(), &mut t));
        let kept = t.trim_end_matches(char::is_whitespace);
        let n = u32::try_from(kept.chars().count()).unwrap_or(u32::MAX);
        let stop = w.start.saturating_add(n);
        // Only the first word has to END where the caret is; behind it the span is walked back.
        if first && (n == 0 || stop != at) {
            return Lookup::Ready(None);
        }
        first = false;
        let lettered = kept.chars().any(char::is_alphanumeric);
        if lettered || w.start <= from || w.start == 0 {
            return Lookup::Ready(Some(w.start..stop));
        }
        end = w.start;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::space::CharsSpace;

    #[test]
    fn key_to_unit_table() {
        assert_eq!(say_for_key(KeyUnit::Char), Say::Char);
        assert_eq!(say_for_key(KeyUnit::LineEdge), Say::Char);
        assert_eq!(say_for_key(KeyUnit::Word), Say::Word);
        assert_eq!(say_for_key(KeyUnit::Line), Say::Line);
        assert_eq!(say_for_key(KeyUnit::Page), Say::Line);
        assert_eq!(say_for_key(KeyUnit::DocEdge), Say::Line);
        assert_eq!(say_for_key(KeyUnit::Paragraph), Say::Paragraph);
    }

    #[test]
    fn uncorrelated_moves() {
        assert_eq!(say_for_uncorrelated(4, 5, true), Say::Char);
        assert_eq!(say_for_uncorrelated(5, 4, true), Say::Char);
        assert_eq!(say_for_uncorrelated(4, 5, false), Say::Line);
        assert_eq!(say_for_uncorrelated(4, 9, true), Say::Line);
        assert_eq!(say_for_uncorrelated(4, 4, true), Say::Line);
    }

    fn word(text: &str, from: Option<u32>, at: u32) -> Lookup<Presented> {
        let c: Vec<char> = text.chars().collect();
        present_word(&CharsSpace::new(&c, 0), from, at, EditQuirks::default())
    }

    #[test]
    fn word_ending_at_the_caret_wins_after_a_forward_move() {
        let t = "Привет, как дела твои?";
        assert_eq!(word(t, Some(0), 6), Lookup::Ready(Presented::Text(0..6)));
        assert_eq!(word(t, Some(6), 11), Lookup::Ready(Presented::Text(8..11)));
        assert_eq!(
            word(t, Some(16), 21),
            Lookup::Ready(Presented::Text(17..21))
        );
        assert_eq!(
            word(t, Some(21), 22),
            Lookup::Ready(Presented::Text(21..22))
        );
    }

    #[test]
    fn word_at_the_caret_wins_when_the_caret_is_at_a_word_start_or_moved_back() {
        let t = "Привет, как дела твои?";
        assert_eq!(word(t, Some(0), 8), Lookup::Ready(Presented::Text(8..12)));
        assert_eq!(
            word(t, Some(22), 17),
            Lookup::Ready(Presented::Text(17..21))
        );
        assert_eq!(word(t, None, 6), Lookup::Ready(Presented::Text(6..8)));
        assert_eq!(word("дела ", Some(0), 5), Lookup::Ready(Presented::Blank));
        assert_eq!(
            word("a\nb", Some(0), 2),
            Lookup::Ready(Presented::Text(2..3))
        );
    }

    #[test]
    fn a_landing_behind_the_trailing_punctuation_speaks_the_word_not_the_mark() {
        // Gecko: Ctrl+Right from 0 in "hello, brave world" lands at 6, behind the comma.
        let t = "hello, brave world";
        assert_eq!(word(t, Some(0), 6), Lookup::Ready(Presented::Text(0..5)));
        // Pango skips the comma and lands at the end of "brave".
        assert_eq!(word(t, Some(5), 12), Lookup::Ready(Presented::Text(7..12)));
        // GtkSourceView and Blink stop between the word and the comma, then behind the comma.
        assert_eq!(word(t, Some(0), 5), Lookup::Ready(Presented::Text(0..5)));
        assert_eq!(word(t, Some(5), 6), Lookup::Ready(Presented::Text(5..6)));
        // A path: readline's Alt+f stops at the end of each name.
        assert_eq!(
            word("/usr/local/bin", Some(0), 4),
            Lookup::Ready(Presented::Text(1..4))
        );
    }

    #[test]
    fn present_word_agrees_with_the_extension_sdk_vectors() {
        // `gorca_ext_api::terminal::word_motion` is the slice form of this model for terminals.
        for &(text, from, at, want) in gorca_ext_api::terminal::WORD_MOTION_VECTORS {
            let c: Vec<char> = text.chars().collect();
            let got = match present_word(&CharsSpace::new(&c, 0), from, at, EditQuirks::default()) {
                Lookup::Ready(Presented::Text(r)) => c[r.start as usize..r.end as usize]
                    .iter()
                    .collect::<String>()
                    .trim()
                    .to_owned(),
                Lookup::Ready(Presented::Blank) => String::new(),
                Lookup::Need(n) => panic!("{text:?}: needs {n:?}"),
            };
            assert_eq!(got, want, "{text:?} {from:?} -> {at}");
        }
    }

    #[test]
    fn a_start_landing_toolkit_speaks_the_word_at_the_caret() {
        let c: Vec<char> = "foo,bar baz".chars().collect();
        let sp = CharsSpace::new(&c, 0);
        let q = EditQuirks {
            word_landing: WordLanding::Start,
        };
        assert_eq!(
            present_word(&sp, Some(0), 4, q),
            Lookup::Ready(Presented::Text(4..8))
        );
        // The same landing under the END row reads the span: "foo".
        assert_eq!(
            present_word(&sp, Some(0), 4, EditQuirks::default()),
            Lookup::Ready(Presented::Text(0..3))
        );
    }

    #[test]
    fn present_blank_at_end() {
        let c: Vec<char> = "ab".chars().collect();
        let sp = CharsSpace::new(&c, 0);
        assert_eq!(
            present(&sp, 0, Say::Char),
            Lookup::Ready(Presented::Text(0..1))
        );
        assert_eq!(present(&sp, 2, Say::Char), Lookup::Ready(Presented::Blank));
        assert_eq!(present(&sp, 2, Say::Line), Lookup::Ready(Presented::Blank));
        assert_eq!(
            present(&sp, 0, Say::Line),
            Lookup::Ready(Presented::Text(0..2))
        );
    }
}
