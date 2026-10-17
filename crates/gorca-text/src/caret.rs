//! Caret-move classification: which unit to speak for which key (ADR-0007 section 5.2, CR-2 to
//! CR-4). Pure tables and one helper that turns a unit choice into a range over any
//! [`TextSpace`], so native fields and browse mode present identically.

use std::ops::Range;

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
/// Toolkits disagree on where Ctrl+Right lands. NVDA's Windows targets stop at the start of the
/// next word, where the word at the caret is the one just reached. GTK and Gecko on Linux stop
/// at the END of the next word, where the word at the caret is the following punctuation, a
/// space, or nothing at the end of the text, and the word just reached is the one that ends at
/// the caret (Orca speaks the traversed text for the same reason: `script_utilities.py`,
/// `get_word_at_offset_adjusted_for_navigation`). So a forward move that landed right after a
/// non-space scalar, at the end of a word, speaks the word ending at the caret. Every other
/// move speaks the word starting at the caret, and `blank` at the end of the text (NVDA).
pub fn present_word(space: &dyn TextSpace, from: Option<u32>, at: u32) -> Lookup<Presented> {
    if from.is_some_and(|f| f < at) && at > 0 {
        let w = crate::ready!(space.unit_at(at - 1, Unit::Word));
        let mut t = String::new();
        crate::ready!(space.read(w.clone(), &mut t));
        let kept = t.trim_end_matches(char::is_whitespace).chars().count();
        let ends_here = u32::try_from(kept).is_ok_and(|k| w.start + k == at);
        if ends_here && w.start < at {
            return Lookup::Ready(Presented::Text(w.start..at));
        }
    }
    present(space, at, Say::Word)
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
        present_word(&CharsSpace::new(&c, 0), from, at)
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
