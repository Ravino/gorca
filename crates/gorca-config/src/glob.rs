// SPDX-License-Identifier: GPL-3.0-or-later
//! The one glob of gorca (ADR-0016 GL-1), shared by profile triggers and extension matchers.
//!
//! - `*` matches any run of characters (possibly empty), `?` exactly one character;
//! - `\` escapes `*`, `?` and `\` (a `\` before any other character is a literal backslash);
//! - ASCII case-insensitive; a non-ASCII character matches only itself (the same relation as
//!   `str::eq_ignore_ascii_case`, which the CHR-0 interim table used);
//! - a pattern is at most [`MAX_GLOB_BYTES`] bytes;
//! - matching is iterative, O(pattern x text) in the worst case, never recursive, and never
//!   allocates.

/// Longest accepted pattern, in bytes.
pub const MAX_GLOB_BYTES: usize = 256;

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum GlobError {
    #[error("pattern is {len} bytes, the limit is {limit}")]
    TooLong { len: usize, limit: usize },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Tok {
    Lit(char),
    One,
    Star,
}

/// A compiled pattern.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Glob {
    src: Box<str>,
    toks: Box<[Tok]>,
}

impl Glob {
    /// Compile `pattern`.
    pub fn new(pattern: &str) -> Result<Glob, GlobError> {
        if pattern.len() > MAX_GLOB_BYTES {
            return Err(GlobError::TooLong {
                len: pattern.len(),
                limit: MAX_GLOB_BYTES,
            });
        }
        let mut toks: Vec<Tok> = Vec::with_capacity(pattern.len());
        let mut it = pattern.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '*' => {
                    // `**` is `*`: collapsing keeps storms of stars linear.
                    if toks.last() != Some(&Tok::Star) {
                        toks.push(Tok::Star);
                    }
                }
                '?' => toks.push(Tok::One),
                '\\' => match it.peek() {
                    Some(&n @ ('*' | '?' | '\\')) => {
                        toks.push(Tok::Lit(n));
                        it.next();
                    }
                    Some(_) | None => toks.push(Tok::Lit('\\')),
                },
                other => toks.push(Tok::Lit(other)),
            }
        }
        Ok(Glob {
            src: pattern.into(),
            toks: toks.into_boxed_slice(),
        })
    }

    /// The pattern as written.
    pub fn as_str(&self) -> &str {
        &self.src
    }

    /// `text` matches the whole pattern.
    pub fn matches(&self, text: &str) -> bool {
        let toks = &*self.toks;
        let (mut pi, mut ti) = (0usize, 0usize);
        // Where to resume after the last `*`: (token after it, text offset it has eaten up to).
        let mut star: Option<(usize, usize)> = None;
        loop {
            let next = text.get(ti..).and_then(|r| r.chars().next());
            if let Some(tok) = toks.get(pi) {
                match (tok, next) {
                    (Tok::Star, _) => {
                        star = Some((pi + 1, ti));
                        pi += 1;
                        continue;
                    }
                    (Tok::One, Some(c)) => {
                        pi += 1;
                        ti += c.len_utf8();
                        continue;
                    }
                    (Tok::Lit(l), Some(c)) if l.eq_ignore_ascii_case(&c) => {
                        pi += 1;
                        ti += c.len_utf8();
                        continue;
                    }
                    (Tok::One | Tok::Lit(_), _) => {}
                }
            } else if next.is_none() {
                return true;
            }
            // Mismatch: let the last `*` eat one more character, or fail.
            let Some((sp, st)) = star else {
                return false;
            };
            let Some(eaten) = text.get(st..).and_then(|r| r.chars().next()) else {
                return false;
            };
            let st = st + eaten.len_utf8();
            star = Some((sp, st));
            pi = sp;
            ti = st;
        }
    }

    /// The pattern has no wildcard: the literal text it matches (escapes resolved).
    pub fn literal(&self) -> Option<String> {
        let mut s = String::with_capacity(self.toks.len());
        for t in &*self.toks {
            match t {
                Tok::Lit(c) => s.push(*c),
                Tok::One | Tok::Star => return None,
            }
        }
        Some(s)
    }

    /// Bytes of literal text before the first wildcard (the PM-1 tie-break).
    pub fn literal_prefix_len(&self) -> usize {
        self.toks
            .iter()
            .take_while(|t| matches!(t, Tok::Lit(_)))
            .map(|t| match t {
                Tok::Lit(c) => c.len_utf8(),
                Tok::One | Tok::Star => 0,
            })
            .sum()
    }

    /// Number of non-wildcard characters (PF-4 wants at least three).
    pub fn literal_chars(&self) -> usize {
        self.toks
            .iter()
            .filter(|t| matches!(t, Tok::Lit(_)))
            .count()
    }
}

/// `text` as a pattern that matches only itself: `*`, `?` and `\` are escaped (PE-6).
pub fn escape_literal(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    for c in text.chars() {
        if matches!(c, '*' | '?' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(p: &str, t: &str) -> bool {
        Glob::new(p).unwrap().matches(t)
    }

    #[test]
    fn star_question_and_literals() {
        assert!(m("firefox", "firefox"));
        assert!(!m("firefox", "firefox-esr"));
        assert!(m("firefox*", "firefox-esr"));
        assert!(m("*fox", "firefox"));
        assert!(m("f*x", "firefox"));
        assert!(m("f?refox", "firefox"));
        assert!(!m("f?refox", "frefox"));
        assert!(m("*", ""));
        assert!(m("*", "anything"));
        assert!(m("", ""));
        assert!(!m("", "x"));
        assert!(m("org.gnome.*", "org.gnome.Terminal"));
        assert!(!m("org.gnome.*", "org.kde.konsole"));
        assert!(m("a*b*c", "aXXbYYc"));
        assert!(!m("a*b*c", "aXXbYY"));
        assert!(m("*a", "bbbaa"));
    }

    #[test]
    fn escapes() {
        assert!(m(r"\*", "*"));
        assert!(!m(r"\*", "x"));
        assert!(m(r"a\?b", "a?b"));
        assert!(!m(r"a\?b", "axb"));
        assert!(m(r"a\\b", r"a\b"));
        // A backslash before an ordinary character, or at the end, is a literal backslash.
        assert!(m(r"a\b", r"a\b"));
        assert!(m(r"ab\", r"ab\"));
        assert!(!m(r"ab\", "ab"));
        // Escaped star next to a real star.
        assert!(m(r"\**", "*anything"));
        assert!(!m(r"\**", "anything"));
    }

    #[test]
    fn ascii_case_folds_and_non_ascii_is_exact() {
        assert!(m("FireFox", "firefox"));
        assert!(m("firefox", "FIREFOX"));
        assert!(m("\u{e9}cole", "\u{e9}cole"));
        // Non-ASCII is compared exactly: E-acute upper and lower case differ.
        assert!(!m("\u{c9}cole", "\u{e9}cole"));
        assert!(m("\u{e9}*", "\u{e9}t\u{e9}"));
        // `?` is one character, not one byte.
        assert!(m("?", "\u{e9}"));
        assert!(!m("?", "\u{e9}\u{e9}"));
        assert!(m("a?c", "a\u{1f600}c"));
    }

    #[test]
    fn agrees_with_eq_ignore_ascii_case_on_literals() {
        // GL-1 widens CHR-0's `eq_ignore_ascii_case` to patterns, but for escaped literals it is
        // the same relation, including non-ASCII.
        let words = [
            "code",
            "Code",
            "CODE",
            "Visual Studio Code",
            "\u{c9}diteur",
            "\u{e9}diteur",
            "a*b",
            "",
            "\\",
        ];
        for a in words {
            for b in words {
                let g = Glob::new(&escape_literal(a)).unwrap();
                assert_eq!(g.matches(b), a.eq_ignore_ascii_case(b), "{a:?} vs {b:?}");
            }
        }
    }

    #[test]
    fn length_cap() {
        let ok = "a".repeat(MAX_GLOB_BYTES);
        assert!(Glob::new(&ok).is_ok());
        let long = "a".repeat(MAX_GLOB_BYTES + 1);
        assert_eq!(
            Glob::new(&long),
            Err(GlobError::TooLong {
                len: MAX_GLOB_BYTES + 1,
                limit: MAX_GLOB_BYTES
            })
        );
    }

    #[test]
    fn star_storms_do_not_blow_up() {
        // Worst cases for naive recursive matchers: O(n*m) here, never exponential.
        let pat = format!("{}b", "a*".repeat(100));
        let g = Glob::new(&pat).unwrap();
        let text = "a".repeat(4000);
        assert!(!g.matches(&text));
        let pat2 = "*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*a*b";
        assert!(!Glob::new(pat2).unwrap().matches(&"a".repeat(4096)));
        assert!(Glob::new("*****").unwrap().matches("x"));
    }

    #[test]
    fn literal_helpers() {
        let g = Glob::new("code*").unwrap();
        assert_eq!(g.literal(), None);
        assert_eq!(g.literal_prefix_len(), 4);
        assert_eq!(g.literal_chars(), 4);
        let g = Glob::new(r"a\*b").unwrap();
        assert_eq!(g.literal().as_deref(), Some("a*b"));
        assert_eq!(g.literal_prefix_len(), 3);
        let g = Glob::new("??x").unwrap();
        assert_eq!(g.literal_prefix_len(), 0);
        assert_eq!(g.literal_chars(), 1);
    }

    #[test]
    fn escape_literal_matches_only_itself() {
        for name in ["*", "code*", "a?b", r"c:\x", "..", "a/b", "\u{202e}rtl"] {
            let g = Glob::new(&escape_literal(name)).unwrap();
            assert!(g.matches(name), "{name:?}");
            assert_eq!(g.literal().as_deref(), Some(name));
        }
        let star = Glob::new(&escape_literal("*")).unwrap();
        assert!(!star.matches("anything"));
        let code = Glob::new(&escape_literal("code*")).unwrap();
        assert!(!code.matches("codeium"));
    }

    /// A tiny deterministic generator (no new crate): property check against a brute-force
    /// recursive reference on short alphabets.
    #[test]
    fn agrees_with_a_reference_matcher() {
        fn reference(p: &[char], t: &[char]) -> bool {
            match p.first() {
                None => t.is_empty(),
                Some('*') => (0..=t.len()).any(|i| reference(&p[1..], &t[i..])),
                Some('?') => !t.is_empty() && reference(&p[1..], &t[1..]),
                Some(c) => {
                    t.first().is_some_and(|x| x.eq_ignore_ascii_case(c))
                        && reference(&p[1..], &t[1..])
                }
            }
        }
        let alphabet = ['a', 'B', 'b', '*', '?', '\u{e9}'];
        let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..4000 {
            let pl = usize::try_from(next() % 7).unwrap_or(0);
            let tl = usize::try_from(next() % 9).unwrap_or(0);
            let p: Vec<char> = (0..pl)
                .map(|_| alphabet[usize::try_from(next() % 6).unwrap_or(0)])
                .collect();
            // Text has no wildcards.
            let mut t: Vec<char> = (0..tl)
                .map(|_| alphabet[usize::try_from(next() % 3).unwrap_or(0)])
                .collect();
            if next() % 4 == 0 {
                t.push('\u{e9}');
            }
            let ps: String = p.iter().collect();
            let ts: String = t.iter().collect();
            // `reference` has no escapes: only compare patterns without backslashes (none here).
            assert_eq!(
                Glob::new(&ps).unwrap().matches(&ts),
                reference(&p, &t),
                "{ps:?} vs {ts:?}"
            );
        }
    }
}
