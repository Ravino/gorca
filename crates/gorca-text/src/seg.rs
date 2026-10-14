//! Unit segmentation: one implementation of every rule, shared by native text, browse mode and
//! review (ADR-0007 section 3.1, TL-1 to TL-4).
//!
//! All functions take a slice of Unicode scalars and return half-open scalar index ranges into
//! it. They never panic: an index past the end is clamped to the last scalar, and an empty slice
//! yields `0..0`. Ranges of the `Line` family include the `\n` terminator, so the units of one
//! kind tile the text (this is also the AT-SPI convention).
//!
//! Allocation: `char_range`, `word_range` and `soft_lines` never allocate. `sentence_range`
//! builds a temporary `String` of one paragraph; it is not on the per-keystroke path.

use std::ops::Range;

use unicode_segmentation::UnicodeSegmentation;

/// Scalars looked at on each side of the index when the grapheme rules need context. A cluster
/// longer than this (a run of more than 48 regional indicators, a pathological Zalgo sequence)
/// is cut at the window edge.
const GRAPHEME_WINDOW: usize = 48;
const WINDOW_BYTES: usize = (2 * GRAPHEME_WINDOW + 1) * 4;

/// True for scalars that cannot extend or be extended by their neighbours in the grapheme
/// rules: Latin, Greek and Cyrillic letters, digits, ASCII and Latin-1. (CR LF is handled
/// separately.) Used as the allocation-free, branch-cheap fast path of [`char_range`].
const fn simple(c: char) -> bool {
    matches!(c as u32, 0..0x300 | 0x370..=0x482 | 0x48A..=0x52F)
}

/// True for combining marks and other scalars that continue the preceding character: they never
/// start a word or a character of their own. A table of the ranges that matter for the target
/// languages and for emoji sequences; checked against the Unicode grapheme rules in the tests.
pub fn is_mark(c: char) -> bool {
    let u = c as u32;
    matches!(u,
        0x0300..=0x036F
        | 0x0483..=0x0489
        | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2 | 0x05C4..=0x05C5 | 0x05C7
        | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0900..=0x0903 | 0x093A..=0x093C | 0x093E..=0x094F | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x200C | 0x200D
        | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0x1F3FB..=0x1F3FF
        | 0xE0100..=0xE01EF)
}

/// The extended grapheme cluster containing `s[i]` (TL-2). Offsets stay scalar indices.
pub fn char_range(s: &[char], i: usize) -> Range<usize> {
    let len = s.len();
    if len == 0 {
        return 0..0;
    }
    let i = i.min(len - 1);
    let c = s[i];
    let prev = if i > 0 { Some(s[i - 1]) } else { None };
    let next = s.get(i + 1).copied();
    if simple(c) && prev.is_none_or(simple) && next.is_none_or(simple) {
        if c == '\r' && next == Some('\n') {
            return i..i + 2;
        }
        if c == '\n' && prev == Some('\r') {
            return i - 1..i + 1;
        }
        return i..i + 1;
    }
    char_range_slow(s, i)
}

fn char_range_slow(s: &[char], i: usize) -> Range<usize> {
    let lo = i.saturating_sub(GRAPHEME_WINDOW);
    let hi = (i + 1 + GRAPHEME_WINDOW).min(s.len());
    let mut buf = [0u8; WINDOW_BYTES];
    let mut at = 0usize;
    // Byte offset of every scalar of the window would need an array; instead walk the clusters
    // and count scalars as we go.
    for c in &s[lo..hi] {
        at += c.encode_utf8(&mut buf[at..]).len();
    }
    let Ok(text) = std::str::from_utf8(&buf[..at]) else {
        return i..i + 1;
    };
    let mut start = lo;
    for g in text.graphemes(true) {
        let n = g.chars().count();
        if i < start + n {
            return start..start + n;
        }
        start += n;
    }
    i..i + 1
}

#[derive(Copy, Clone, Eq, PartialEq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' || is_mark(c) {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The word containing `s[i]` (TL-1). `s` is one line: the caller clamps to hard lines.
///
/// A word includes its trailing spaces (NVDA and Orca convention); each punctuation character,
/// emoji or symbol is a word of its own (with its trailing spaces); leading spaces belong to the
/// word before them, or form a word of their own at the start of the line. Combining marks
/// continue the word they follow, so decomposed Cyrillic and Latin letters are not split.
pub fn word_range(s: &[char], i: usize) -> Range<usize> {
    if s.is_empty() {
        return 0..0;
    }
    // Everything below works on cluster starts, so a range never ends inside a cluster.
    let next = |p: usize| char_range(s, p).end;
    let prev = |p: usize| char_range(s, p - 1).start;
    let cls = |p: usize| class(s[p]);
    let run_start = |p: usize| {
        let cl = cls(p);
        let mut q = p;
        if cl != Class::Punct {
            while q > 0 && cls(prev(q)) == cl {
                q = prev(q);
            }
        }
        q
    };
    let run_end = |p: usize| {
        let cl = cls(p);
        if cl == Class::Punct {
            return next(p);
        }
        let mut e = p;
        while e < s.len() && cls(e) == cl {
            e = next(e);
        }
        e
    };
    let i = char_range(s, i.min(s.len() - 1)).start;
    match cls(i) {
        Class::Space => {
            let a = run_start(i);
            let b = run_end(i);
            if a > 0 { run_start(prev(a))..b } else { a..b }
        }
        Class::Word | Class::Punct => {
            let a = run_start(i);
            let mut b = run_end(i);
            while b < s.len() && cls(b) == Class::Space {
                b = next(b);
            }
            a..b
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Sentences

/// Abbreviations after which a full stop does not end the sentence (lowercase, without the
/// stop). Titles and name prefixes only: terminal abbreviations such as "etc." are already
/// handled by UAX #29 when a lowercase word follows.
const ABBREVIATIONS: &[&str] = &[
    "mr", "mrs", "ms", "dr", "prof", "sr", "jr", "st", "mt", "vs", "gen", "col", "sgt", "capt",
    "lt", "rev", "hon", "fig", "no", "inc", "ltd", "г", "гг", "им", "ул", "акад", "проф", "доц",
    "тов", "гр", "св", "пр", "рис", "стр", "т",
];

fn token_is_abbreviation(token: &[char]) -> bool {
    if token.is_empty() {
        return false;
    }
    // A lone capital letter is an initial: "J. Smith", "А. С. Пушкин".
    if token.len() == 1 && token[0].is_uppercase() {
        return true;
    }
    ABBREVIATIONS.iter().any(|a| {
        a.chars().count() == token.len()
            && a.chars()
                .zip(token)
                .all(|(x, y)| y.to_lowercase().eq(std::iter::once(x)))
    })
}

/// True when the sentence text ends with `<abbreviation>.` (and optional spaces).
fn ends_with_abbreviation(sent: &[char]) -> bool {
    let mut end = sent.len();
    while end > 0 && sent[end - 1].is_whitespace() {
        end -= 1;
    }
    if end == 0 || sent[end - 1] != '.' {
        return false;
    }
    let dot = end - 1;
    let mut start = dot;
    while start > 0 && sent[start - 1].is_alphabetic() {
        start -= 1;
    }
    token_is_abbreviation(&sent[start..dot])
}

/// The sentence containing `s[i]` (UAX #29 sentence boundaries plus an abbreviation guard).
/// `s` should be bounded by the caller to a paragraph or a block: the work is linear in `s`.
pub fn sentence_range(s: &[char], i: usize) -> Range<usize> {
    if s.is_empty() {
        return 0..0;
    }
    let i = i.min(s.len() - 1);
    // A sentence never crosses a hard line break (SB4), so work on the paragraph only.
    let ps = s[..i].iter().rposition(|c| *c == '\n').map_or(0, |p| p + 1);
    let pe = s[i..]
        .iter()
        .position(|c| *c == '\n')
        .map_or(s.len(), |p| i + p + 1);
    let para: String = s[ps..pe].iter().collect();
    let mut cur: Option<Range<usize>> = None;
    let mut at = ps;
    for seg in para.split_sentence_bounds() {
        let n = seg.chars().count();
        let next = at..at + n;
        at += n;
        cur = Some(match cur {
            Some(c) if ends_with_abbreviation(&s[c.clone()]) => c.start..next.end,
            Some(c) => {
                if i < c.end {
                    return c;
                }
                next
            }
            None => next,
        });
    }
    match cur {
        Some(c) if i < c.end => c,
        _ => ps..pe,
    }
}

// ---------------------------------------------------------------------------------------------
// Lines

/// Hard lines (split after `\n`) further split into soft lines of at most `max` scalars
/// (NVDA `maxLineLength`, TL-3). `max == 0` disables soft wrapping. Ranges include the `\n`
/// terminator and tile the text; text that ends with `\n` (and the empty text) has an empty
/// last line.
pub fn soft_lines(s: &[char], max: u16) -> SoftLines<'_> {
    SoftLines {
        s,
        pos: 0,
        max: usize::from(max),
        tail_done: false,
    }
}

#[derive(Clone, Debug)]
pub struct SoftLines<'a> {
    s: &'a [char],
    pos: usize,
    max: usize,
    tail_done: bool,
}

impl Iterator for SoftLines<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Range<usize>> {
        let len = self.s.len();
        if self.pos >= len {
            let tail = !self.tail_done && (len == 0 || self.s[len - 1] == '\n');
            self.tail_done = true;
            return tail.then_some(len..len);
        }
        let start = self.pos;
        let (content_end, hard_end) = match self.s[start..].iter().position(|c| *c == '\n') {
            Some(p) => (start + p, start + p + 1),
            None => (len, len),
        };
        let end = if self.max == 0 || content_end - start <= self.max {
            hard_end
        } else {
            self.cut(start, content_end)
        };
        self.pos = end;
        Some(start..end)
    }
}

impl SoftLines<'_> {
    /// Cut point of a hard line that is longer than `max`: after the last whitespace that fits,
    /// else at `max`, never in front of a combining mark.
    fn cut(&self, start: usize, content_end: usize) -> usize {
        let limit = start + self.max;
        if let Some(p) = (start + 1..=limit)
            .rev()
            .find(|p| self.s[*p - 1].is_whitespace())
        {
            return p;
        }
        let mut cut = limit;
        while cut > start + 1 && cut < content_end && is_mark(self.s[cut]) {
            cut -= 1;
        }
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cs(s: &str) -> Vec<char> {
        s.chars().collect()
    }

    fn text(s: &[char], r: Range<usize>) -> String {
        s[r].iter().collect()
    }

    fn w(s: &str, i: usize) -> String {
        let c = cs(s);
        text(&c, word_range(&c, i))
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
        assert_eq!(word_range(&[], 3), 0..0);
        assert_eq!(char_range(&[], 3), 0..0);
        assert_eq!(sentence_range(&[], 3), 0..0);
    }

    #[test]
    fn cyrillic_words() {
        assert_eq!(w("привет мир", 2), "привет ");
        assert_eq!(w("привет мир", 7), "мир");
        assert_eq!(w("Съешь же ещё", 10), "ещё");
    }

    #[test]
    fn decomposed_letters_stay_in_the_word() {
        // "е" + U+0308 (diaeresis) must not be split from the letter.
        let s = "ещe\u{308} раз";
        assert_eq!(w(s, 2), "ещe\u{308} ");
        assert_eq!(w(s, 3), "ещe\u{308} ");
    }

    #[test]
    fn graphemes() {
        let s = cs("a\u{308}\r\n\u{1F1E9}\u{1F1EA}\u{1F468}\u{200D}\u{1F469}x");
        assert_eq!(char_range(&s, 0), 0..2);
        assert_eq!(char_range(&s, 1), 0..2);
        assert_eq!(char_range(&s, 2), 2..4);
        assert_eq!(char_range(&s, 3), 2..4);
        assert_eq!(char_range(&s, 4), 4..6);
        assert_eq!(char_range(&s, 6), 6..9);
        assert_eq!(char_range(&s, 9), 9..10);
        assert_eq!(char_range(&s, 99), 9..10);
    }

    #[test]
    fn emoji_sequence_is_one_word() {
        let s = cs("x \u{1F468}\u{200D}\u{1F469} y");
        assert_eq!(word_range(&s, 3), 2..6);
    }

    #[test]
    fn sentences() {
        let s = cs("Hello there. How are you? Fine!");
        assert_eq!(text(&s, sentence_range(&s, 0)), "Hello there. ");
        assert_eq!(text(&s, sentence_range(&s, 14)), "How are you? ");
        assert_eq!(text(&s, sentence_range(&s, 30)), "Fine!");
    }

    #[test]
    fn sentences_cyrillic_and_newline() {
        let s = cs("Привет. Как дела?\nХорошо.");
        assert_eq!(text(&s, sentence_range(&s, 3)), "Привет. ");
        assert_eq!(text(&s, sentence_range(&s, 10)), "Как дела?\n");
        assert_eq!(text(&s, sentence_range(&s, 20)), "Хорошо.");
    }

    #[test]
    fn abbreviation_guard() {
        let s = cs("See Dr. Smith today. Then go.");
        assert_eq!(text(&s, sentence_range(&s, 0)), "See Dr. Smith today. ");
        let s = cs("Мы видели А. С. Пушкина. Он был там.");
        assert_eq!(
            text(&s, sentence_range(&s, 20)),
            "Мы видели А. С. Пушкина. "
        );
        let s = cs("Ушёл. Ул. Ленина дальше.");
        assert_eq!(text(&s, sentence_range(&s, 0)), "Ушёл. ");
    }

    #[test]
    fn soft_lines_tile_and_respect_max() {
        let s = cs("aaa bbb ccc ddd\nxx\n");
        let lines: Vec<_> = soft_lines(&s, 8).collect();
        assert_eq!(
            lines
                .iter()
                .map(|r| text(&s, r.clone()))
                .collect::<Vec<_>>(),
            ["aaa bbb ", "ccc ddd\n", "xx\n", ""]
        );
        let lines: Vec<_> = soft_lines(&s, 0).collect();
        assert_eq!(lines, vec![0..16, 16..19, 19..19]);
        assert_eq!(soft_lines(&[], 5).collect::<Vec<_>>(), vec![0..0]);
    }

    #[test]
    fn soft_line_without_space_cuts_at_max_but_not_before_a_mark() {
        let s = cs("abcdef\u{301}ghij");
        let lines: Vec<_> = soft_lines(&s, 6).collect();
        assert_eq!(lines[0], 0..5);
        let s = cs("abcdefghij");
        assert_eq!(
            soft_lines(&s, 4).collect::<Vec<_>>(),
            vec![0..4, 4..8, 8..10]
        );
    }
}
