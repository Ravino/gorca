//! Say-all chunking (ADR-0007 section 11): the next stretch of text to hand to the speech
//! server, ending at a sentence boundary when one fits, else at a line or word boundary.

use std::ops::Range;

use crate::space::{Lookup, TextSpace, Unit};

/// The chunk starting at `from` of at most `max` scalars (at least one when text remains).
/// Empty when `from` is at or past the end.
pub fn next_chunk(space: &dyn TextSpace, from: u32, max: u32) -> Lookup<Range<u32>> {
    let max = max.max(1);
    let Some(len) = space.len() else {
        return Lookup::Need(crate::Need::Length);
    };
    if from >= len {
        return Lookup::Ready(len..len);
    }
    let limit = from.saturating_add(max).min(len);
    for unit in [Unit::Sentence, Unit::Line] {
        let r = crate::ready!(space.unit_at(from, unit));
        if r.end > from && r.end <= limit {
            return Lookup::Ready(from..r.end);
        }
    }
    // Neither fits: walk words back from the limit.
    let mut end = limit;
    if end < len {
        let w = crate::ready!(space.unit_at(end, Unit::Word));
        if w.start > from {
            end = w.start;
        }
    }
    Lookup::Ready(from..end.max(from + 1))
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use crate::space::CharsSpace;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn all_chunks(text: &str, max: u32) -> Vec<String> {
        let c = cs(text);
        let sp = CharsSpace::new(&c, 0);
        let mut at = 0;
        let mut out = Vec::new();
        while at < c.len() as u32 {
            let r = next_chunk(&sp, at, max).ready().unwrap_or(at..at);
            assert!(r.end > at, "progress at {at}");
            out.push(c[r.start as usize..r.end as usize].iter().collect());
            at = r.end;
        }
        out
    }

    #[test]
    fn sentences_are_chunks() {
        assert_eq!(
            all_chunks("Раз. Два три. Четыре!", 100),
            ["Раз. ", "Два три. ", "Четыре!"]
        );
    }

    #[test]
    fn long_sentence_is_split_on_words() {
        let chunks = all_chunks("aaa bbb ccc ddd eee fff", 9);
        assert_eq!(chunks.concat(), "aaa bbb ccc ddd eee fff");
        assert!(chunks.iter().all(|c| c.chars().count() <= 9), "{chunks:?}");
    }

    #[test]
    fn unbreakable_text_still_progresses() {
        let chunks = all_chunks("abcdefghijkl", 5);
        assert_eq!(chunks.concat(), "abcdefghijkl");
        assert!(chunks.len() >= 3);
    }

    #[test]
    fn empty_and_end() {
        let c = cs("x");
        let sp = CharsSpace::new(&c, 0);
        assert_eq!(next_chunk(&sp, 1, 10), Lookup::Ready(1..1));
        assert_eq!(next_chunk(&sp, 9, 10), Lookup::Ready(1..1));
    }
}
