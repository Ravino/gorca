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
