//! Typing-echo model (ADR-0007 section 7, TE-1 to TE-7). Pure state: time is passed in as
//! milliseconds from any monotonic origin, so the Reader's fake clock drives the tests.
//!
//! Flow: every typed printable key pushes an expectation (whether or not the character is
//! spoken, so the echo setting never changes dedupe). A later text insert in the focus subtree is
//! matched against the oldest expectations: a match means "already echoed from the key", no
//! match means the insert has another source (paste, input method, autocorrect, script) and is
//! spoken as text.

use crate::caret::Say;

/// Expectations live this long; older ones mean the app ignored the key (TE-2).
pub const EXPECT_TTL_MS: u64 = 1000;
/// Ring capacity (TE-2).
pub const RING: usize = 16;
/// Inserted text spoken in full up to this many scalars, then "and N more" (TE-3).
pub const MAX_INSERT_SCALARS: usize = 256;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
struct Expect {
    ch: char,
    t_ms: u64,
}

/// Result of matching an insert against the expectations.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Consume {
    /// The insert was exactly the oldest `n` expected characters; they are consumed and the
    /// insert is not spoken again.
    Matched { n: usize },
    /// The insert does not correspond to typed keys; nothing was consumed.
    Unmatched,
}

/// Where an unmatched insert came from, as far as the caller knows (TE-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum InsertSource {
    Paste,
    Ime,
    Unknown,
}

/// Why an utterance was produced, recorded for the presentation log and tests (TE-7).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Reason {
    Typed,
    TypedWord,
    Erased,
    Inserted(InsertSource),
    CaretMove { say: Say, correlated: bool },
    Boundary,
    Selection,
}

/// Fixed ring of the last [`RING`] typed characters.
#[derive(Clone, Debug)]
pub struct ExpectRing {
    buf: [Expect; RING],
    head: usize,
    len: usize,
}

impl Default for ExpectRing {
    fn default() -> Self {
        Self::new()
    }
}

impl ExpectRing {
    pub fn new() -> Self {
        Self {
            buf: [Expect { ch: '\0', t_ms: 0 }; RING],
            head: 0,
            len: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }

    fn at(&self, i: usize) -> Expect {
        self.buf[(self.head + i) % RING]
    }

    fn pop_front(&mut self, n: usize) {
        let n = n.min(self.len);
        self.head = (self.head + n) % RING;
        self.len -= n;
    }

    /// Record a typed character. When the ring is full the oldest entry is overwritten.
    pub fn push(&mut self, ch: char, now_ms: u64) {
        if self.len == RING {
            self.pop_front(1);
        }
        self.expire(now_ms);
        let slot = (self.head + self.len) % RING;
        self.buf[slot] = Expect { ch, t_ms: now_ms };
        self.len += 1;
    }

    /// Milliseconds since the newest expectation was pushed, if any.
    pub fn newest_age_ms(&self, now_ms: u64) -> Option<u64> {
        (self.len > 0).then(|| now_ms.saturating_sub(self.at(self.len - 1).t_ms))
    }

    /// Consume up to `n` of the oldest unexpired expectations without comparing text: an insert
    /// arrived without its text (the application did not send it) and is taken to be the typed
    /// keys. Returns how many were consumed.
    pub fn forget(&mut self, n: usize, now_ms: u64) -> usize {
        self.expire(now_ms);
        let n = n.min(self.len);
        self.pop_front(n);
        n
    }

    /// Drop expectations older than [`EXPECT_TTL_MS`].
    pub fn expire(&mut self, now_ms: u64) {
        while self.len > 0 && now_ms.saturating_sub(self.at(0).t_ms) > EXPECT_TTL_MS {
            self.pop_front(1);
        }
    }

    /// Match an insert's text against the unconsumed expectations (TE-2). Fast typing
    /// coalesced into one insert matches several expectations at once.
    ///
    /// The insert normally starts at the oldest expectation. When it does not, but starts at a
    /// later one, the older keys never produced an insert (the application refused or dropped
    /// them: a full field, a lost key): they are stale, are dropped with the match, and the
    /// insert is still the typed keys', not text from elsewhere. Without this the stale front
    /// entry makes every later insert "unexplained" and each following key is spoken twice,
    /// once from the key and once from its insert, until the entry expires.
    pub fn consume_insert(&mut self, text: &str, now_ms: u64) -> Consume {
        self.expire(now_ms);
        if text.is_empty() {
            return Consume::Unmatched;
        }
        let n = text.chars().count();
        for skip in 0..self.len {
            if skip + n > self.len {
                break;
            }
            if text
                .chars()
                .enumerate()
                .all(|(i, c)| self.at(skip + i).ch == c)
            {
                self.pop_front(skip + n);
                return Consume::Matched { n };
            }
        }
        Consume::Unmatched
    }
}

/// What to do with a text insert (TE-2, TE-3).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InsertAction<'a> {
    /// Already echoed from the keys; stay silent.
    Echoed { n: usize },
    /// Speak `text`, then "and `more` more" when `more > 0`.
    Speak {
        text: &'a str,
        more: usize,
        source: InsertSource,
    },
}

/// Decide what to do with an insert of `text`. `inserted_text` is the `echo.inserted_text`
/// setting; when it is off an unmatched insert yields `None`.
pub fn on_insert<'a>(
    ring: &mut ExpectRing,
    text: &'a str,
    now_ms: u64,
    source: InsertSource,
    inserted_text: bool,
) -> Option<InsertAction<'a>> {
    match ring.consume_insert(text, now_ms) {
        Consume::Matched { n } => Some(InsertAction::Echoed { n }),
        Consume::Unmatched if !inserted_text || text.is_empty() => None,
        Consume::Unmatched => {
            let (text, more) = truncate_inserted(text);
            Some(InsertAction::Speak { text, more, source })
        }
    }
}

/// Cut `text` to [`MAX_INSERT_SCALARS`] scalars; returns the kept prefix and how many scalars
/// were dropped.
pub fn truncate_inserted(text: &str) -> (&str, usize) {
    match text.char_indices().nth(MAX_INSERT_SCALARS) {
        None => (text, 0),
        Some((cut, _)) => (&text[..cut], text[cut..].chars().count()),
    }
}

/// True for characters that end a typed word (TE-4): whitespace and punctuation.
pub fn is_separator(c: char) -> bool {
    !(c.is_alphanumeric() || c == '_' || crate::seg::is_mark(c))
}

/// Fallback word tracker for when the text window does not cover the word (TE-4). Reset by any
/// caret move, erase, or focus change.
#[derive(Clone, Debug, Default)]
pub struct TypedWordBuffer {
    word: String,
}

impl TypedWordBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.word.clear();
    }

    /// Feed a typed character. Returns `true` and fills `out` with the finished word when `ch`
    /// is a separator that closes a non-empty word.
    pub fn push(&mut self, ch: char, out: &mut String) -> bool {
        if is_separator(ch) {
            if self.word.is_empty() {
                return false;
            }
            out.clear();
            out.push_str(&self.word);
            self.word.clear();
            true
        } else {
            self.word.push(ch);
            false
        }
    }
}

/// The word that ends just before the separator at `s[sep]`, read from the window (TE-4).
/// `None` when the separator follows another separator or the start of `s`.
pub fn word_before(s: &[char], sep: usize) -> Option<std::ops::Range<usize>> {
    let sep = sep.min(s.len());
    let mut start = sep;
    while start > 0 && !is_separator(s[start - 1]) {
        start -= 1;
    }
    (start < sep).then_some(start..sep)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_chars_are_deduped_against_one_coalesced_insert() {
        let mut r = ExpectRing::new();
        for (i, c) in "при".chars().enumerate() {
            r.push(c, 100 + i as u64);
        }
        assert_eq!(r.consume_insert("при", 200), Consume::Matched { n: 3 });
        assert!(r.is_empty());
    }

    #[test]
    fn partial_inserts_consume_in_order() {
        let mut r = ExpectRing::new();
        for c in "abc".chars() {
            r.push(c, 0);
        }
        assert_eq!(r.consume_insert("a", 10), Consume::Matched { n: 1 });
        assert_eq!(r.consume_insert("bc", 10), Consume::Matched { n: 2 });
    }

    #[test]
    fn mismatch_consumes_nothing() {
        let mut r = ExpectRing::new();
        r.push('a', 0);
        assert_eq!(r.consume_insert("ab", 1), Consume::Unmatched);
        assert_eq!(r.consume_insert("x", 1), Consume::Unmatched);
        assert_eq!(r.len(), 1);
        assert_eq!(r.consume_insert("", 1), Consume::Unmatched);
    }

    #[test]
    fn keys_whose_insert_never_came_do_not_block_the_next_insert() {
        // 'd' was typed but the field refused it; 'z' then went in.
        let mut r = ExpectRing::new();
        r.push('d', 0);
        r.push('z', 10);
        assert_eq!(r.consume_insert("z", 20), Consume::Matched { n: 1 });
        assert!(r.is_empty(), "the stale entry went with the match");
        // Text that is none of the typed keys still consumes nothing.
        r.push('d', 30);
        r.push('z', 40);
        assert_eq!(r.consume_insert("q", 50), Consume::Unmatched);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn the_newest_expectation_has_an_age() {
        let mut r = ExpectRing::new();
        assert_eq!(r.newest_age_ms(10), None);
        r.push('a', 100);
        r.push('b', 140);
        assert_eq!(r.newest_age_ms(150), Some(10));
    }

    #[test]
    fn forget_consumes_without_comparing() {
        let mut r = ExpectRing::new();
        for c in "abc".chars() {
            r.push(c, 0);
        }
        assert_eq!(r.forget(2, 10), 2);
        assert_eq!(r.consume_insert("c", 10), Consume::Matched { n: 1 });
        assert_eq!(r.forget(5, 10), 0);
        r.push('x', 0);
        assert_eq!(r.forget(1, EXPECT_TTL_MS + 5), 0, "expired");
    }

    #[test]
    fn expectations_expire_after_a_second() {
        let mut r = ExpectRing::new();
        r.push('a', 0);
        assert_eq!(r.consume_insert("a", EXPECT_TTL_MS + 1), Consume::Unmatched);
        assert!(r.is_empty());
        r.push('b', 0);
        r.push('c', 900);
        r.expire(1100);
        assert_eq!(r.len(), 1, "only the old one is dropped");
    }

    #[test]
    fn ring_overwrites_the_oldest() {
        let mut r = ExpectRing::new();
        for i in 0..20u32 {
            r.push(char::from_u32('a' as u32 + i).unwrap_or('?'), 0);
        }
        assert_eq!(r.len(), RING);
        assert_eq!(r.consume_insert("e", 0), Consume::Matched { n: 1 });
    }

    #[test]
    fn untyped_inserts_are_spoken_and_truncated() {
        let mut r = ExpectRing::new();
        let big: String = "ж".repeat(300);
        match on_insert(&mut r, &big, 0, InsertSource::Paste, true) {
            Some(InsertAction::Speak { text, more, source }) => {
                assert_eq!(text.chars().count(), MAX_INSERT_SCALARS);
                assert_eq!(more, 44);
                assert_eq!(source, InsertSource::Paste);
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(on_insert(&mut r, "x", 0, InsertSource::Ime, false), None);
        r.push('q', 0);
        assert_eq!(
            on_insert(&mut r, "q", 1, InsertSource::Unknown, true),
            Some(InsertAction::Echoed { n: 1 })
        );
    }

    #[test]
    fn typed_word_buffer_emits_on_separator() {
        let mut b = TypedWordBuffer::new();
        let mut out = String::new();
        for c in "мир".chars() {
            assert!(!b.push(c, &mut out));
        }
        assert!(b.push(' ', &mut out));
        assert_eq!(out, "мир");
        assert!(!b.push(' ', &mut out), "a second separator closes nothing");
        b.push('x', &mut out);
        b.reset();
        assert!(!b.push('.', &mut out));
    }

    #[test]
    fn word_before_reads_the_window() {
        let s: Vec<char> = "say hello, ".chars().collect();
        assert_eq!(word_before(&s, 9), Some(4..9));
        assert_eq!(word_before(&s, 10), None);
        assert_eq!(word_before(&s, 0), None);
        assert_eq!(word_before(&s, 99), None);
    }
}
