use std::sync::Arc;

use gorca_model::Utterance;
use gorca_output_api::Verbalized;

/// How much punctuation is spoken by name (ADR-0007 VB-1). The same four levels as the
/// `speech.punctuation` setting; the core maps one to the other.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Punct {
    None,
    #[default]
    Some,
    Most,
    All,
}

/// How a capital letter is announced when a single character is spoken.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Caps {
    /// A higher pitch. The text carries no marker today: the server cannot be told yet, so
    /// this speaks a capital like its lower-case letter.
    #[default]
    Pitch,
    /// The word "cap" before the letter.
    Say,
    Off,
}

/// How a unit of text is spoken.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum How {
    /// One character: symbols, spaces and controls by name, always (VB-1).
    Char,
    /// A word: punctuation by the level; only whitespace is "blank" (VB-2).
    Word,
    /// A line, like a word.
    Line,
    /// Running text (inserted text, say-all chunks): punctuation by the level.
    Text,
}

#[derive(Copy, Clone, Debug, Default)]
pub struct VerbalizeOpts {
    pub punct: Punct,
    pub caps: Caps,
}

/// Turns an utterance into server text and a unit of text into what is spoken. Verbalisation
/// runs before enqueueing, on the producer's thread or `cpu`, never on the speech lane
/// (section 8.1, VB-4).
#[derive(Debug, Default)]
pub struct Verbalizer;

/// Names of symbols spoken at the `some` level and above.
const SOME: &[(char, &str)] = &[
    ('@', "at"),
    ('#', "number"),
    ('$', "dollar"),
    ('%', "percent"),
    ('^', "caret"),
    ('&', "and"),
    ('*', "star"),
    ('+', "plus"),
    ('=', "equals"),
    ('<', "less"),
    ('>', "greater"),
    ('/', "slash"),
    ('\\', "backslash"),
    ('|', "bar"),
    ('~', "tilde"),
    ('_', "underscore"),
    ('\u{20ac}', "euro"),
    ('\u{a3}', "pound"),
    ('\u{a9}', "copyright"),
];

/// Added at `most`.
const MOST: &[(char, &str)] = &[
    ('(', "left paren"),
    (')', "right paren"),
    ('[', "left bracket"),
    (']', "right bracket"),
    ('{', "left brace"),
    ('}', "right brace"),
    ('"', "quote"),
    ('\'', "apostrophe"),
    ('`', "grave"),
    ('\u{ab}', "left quote"),
    ('\u{bb}', "right quote"),
    ('\u{2014}', "dash"),
    ('\u{2013}', "dash"),
];

/// Added at `all`.
const ALL: &[(char, &str)] = &[
    ('.', "dot"),
    (',', "comma"),
    (';', "semicolon"),
    (':', "colon"),
    ('!', "bang"),
    ('?', "question"),
    ('-', "dash"),
    ('\u{2026}', "ellipsis"),
];

fn lookup(table: &'static [(char, &'static str)], c: char) -> Option<&'static str> {
    table.iter().find(|(k, _)| *k == c).map(|(_, v)| *v)
}

/// The name of a symbol at `level`, or `None` when it is not spoken by name there.
fn symbol_name(c: char, level: Punct) -> Option<&'static str> {
    match level {
        Punct::None => None,
        Punct::Some => lookup(SOME, c),
        Punct::Most => lookup(SOME, c).or_else(|| lookup(MOST, c)),
        Punct::All => lookup(SOME, c)
            .or_else(|| lookup(MOST, c))
            .or_else(|| lookup(ALL, c)),
    }
}

/// The name of a single character, whatever the level (VB-1): every symbol has one.
fn char_name(c: char) -> Option<&'static str> {
    symbol_name(c, Punct::All).or(match c {
        ' ' | '\u{a0}' => Some("space"),
        '\t' => Some("tab"),
        '\n' | '\r' | '\u{2028}' | '\u{2029}' => Some("blank"),
        _ => None,
    })
}

/// A unit that holds nothing but embedded-object marks (an inline image or link inside a
/// paragraph): it is said as "object", never as "blank".
fn only_objects(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| matches!(c, '\u{FFFC}' | '\u{FEFF}'))
        && s.contains('\u{FFFC}')
}

/// Text read from an application, without the phantom U+FEFF Gecko puts after each astral
/// scalar (and the zero-width no-break space nobody means to say) and without the object
/// replacement mark U+FFFC that stands for an embedded child: neither is ever spoken.
fn without_phantoms(s: &str) -> std::borrow::Cow<'_, str> {
    if s.contains(['\u{FEFF}', '\u{FFFC}']) {
        std::borrow::Cow::Owned(
            s.chars()
                .filter(|c| !matches!(*c, '\u{FEFF}' | '\u{FFFC}'))
                .collect(),
        )
    } else {
        std::borrow::Cow::Borrowed(s)
    }
}

const PHONETIC: [&str; 26] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel", "india", "juliet",
    "kilo", "lima", "mike", "november", "oscar", "papa", "quebec", "romeo", "sierra", "tango",
    "uniform", "victor", "whiskey", "x-ray", "yankee", "zulu",
];

impl Verbalizer {
    pub fn verbalize(&self, u: &Utterance) -> Verbalized {
        let mut text = String::new();
        for seg in &u.segments {
            if seg.text.is_empty() {
                continue;
            }
            if !text.is_empty() {
                // A description is a pause: "Name, edit. Help text".
                text.push_str(if seg.kind == gorca_model::SegmentKind::Description {
                    ". "
                } else {
                    ", "
                });
            }
            text.push_str(&seg.text);
        }
        Verbalized {
            text: Arc::from(text),
            lang: u.lang.clone(),
        }
    }

    /// What to say for the unit `s` read from a text (ADR-0007 section 3.2). Empty input is
    /// "blank".
    pub fn unit(&self, s: &str, how: How, o: &VerbalizeOpts) -> String {
        if only_objects(s) {
            return "object".to_owned();
        }
        let s = without_phantoms(s);
        let s = s.as_ref();
        match how {
            How::Char => Self::one_char(s, *o),
            How::Word | How::Line | How::Text => Self::run(s, *o),
        }
    }

    /// One character (a grapheme: the first scalar decides its name).
    fn one_char(s: &str, o: VerbalizeOpts) -> String {
        let Some(c) = s.chars().next() else {
            return "blank".to_owned();
        };
        if let Some(name) = char_name(c) {
            return name.to_owned();
        }
        if c.is_uppercase() && o.caps == Caps::Say {
            return format!("cap {s}");
        }
        s.to_owned()
    }

    /// A word, a line or running text: whitespace-only is "blank" (VB-2); the symbols of the
    /// punctuation level are replaced by their names; the line break at the end is dropped.
    fn run(s: &str, o: VerbalizeOpts) -> String {
        let s = s.trim_end_matches(['\n', '\r']);
        if s.chars().all(char::is_whitespace) {
            return "blank".to_owned();
        }
        if o.punct == Punct::None {
            return s.to_owned();
        }
        let mut out = String::with_capacity(s.len() + 8);
        for c in s.chars() {
            match symbol_name(c, o.punct) {
                Some(name) => {
                    if !out.is_empty() && !out.ends_with(' ') {
                        out.push(' ');
                    }
                    out.push_str(name);
                    out.push(' ');
                }
                None if c == ' ' && out.ends_with(' ') => {}
                None => out.push(c),
            }
        }
        let trimmed = out.trim_end().len();
        out.truncate(trimmed);
        out
    }

    /// Spell `s` letter by letter (`text.spell_*`), or with the phonetic alphabet for Latin
    /// letters when `phonetic` is set. Every character is named, so spelling a word that holds
    /// a space or a symbol says so.
    pub fn spell(&self, s: &str, phonetic: bool, o: &VerbalizeOpts) -> String {
        if only_objects(s) {
            return "object".to_owned();
        }
        let s = without_phantoms(s);
        let s = s.trim_end_matches(['\n', '\r']);
        if s.is_empty() {
            return "blank".to_owned();
        }
        let mut parts: Vec<String> = Vec::new();
        for c in s.chars() {
            let lower = c.to_ascii_lowercase();
            let part = if phonetic && lower.is_ascii_lowercase() {
                let idx = usize::from(lower as u8 - b'a');
                PHONETIC
                    .get(idx)
                    .map_or_else(String::new, |p| (*p).to_owned())
            } else {
                let mut buf = [0u8; 4];
                Self::one_char(c.encode_utf8(&mut buf), *o)
            };
            if c.is_uppercase() && o.caps == Caps::Say && phonetic {
                parts.push(format!("cap {part}"));
            } else {
                parts.push(part);
            }
        }
        parts.join(", ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::speech::SegmentKind;
    use gorca_model::{Epoch, Prio, Segment, UttId};
    use smallvec::smallvec;

    #[test]
    fn a_description_is_set_off_by_its_kind_and_content_is_never_mistaken_for_it() {
        let seg = |t: &str, kind| Segment {
            text: Arc::from(t),
            kind,
        };
        let u = Utterance {
            id: UttId(1),
            segments: smallvec![
                seg("Name", SegmentKind::Text),
                seg("edit", SegmentKind::Role),
                seg("Help", SegmentKind::Description),
                // Page text that begins with a sentence break stays a comma-joined segment.
                seg(". Dot", SegmentKind::Text)
            ],
            lang: None,
            epoch: Epoch(1),
            prio: Prio::Text,
            may_refine: false,
            survives_focus_change: false,
            ttl_secs: 0,
            refines: None,
        };
        assert_eq!(&*Verbalizer.verbalize(&u).text, "Name, edit. Help, . Dot");
    }

    #[test]
    fn segments_are_joined_and_empty_ones_skipped() {
        let seg = |t: &str, kind| Segment {
            text: Arc::from(t),
            kind,
        };
        let u = Utterance {
            id: UttId(1),
            segments: smallvec![
                seg("OK", SegmentKind::Text),
                seg("", SegmentKind::Value),
                seg("button", SegmentKind::Role)
            ],
            lang: Some(Arc::from("en")),
            epoch: Epoch(1),
            prio: Prio::Text,
            may_refine: false,
            survives_focus_change: false,
            ttl_secs: 0,
            refines: None,
        };
        let v = Verbalizer.verbalize(&u);
        assert_eq!(&*v.text, "OK, button");
        assert_eq!(v.lang.as_deref(), Some("en"));
    }

    fn opts(punct: Punct) -> VerbalizeOpts {
        VerbalizeOpts {
            punct,
            caps: Caps::Pitch,
        }
    }

    #[test]
    fn single_characters_are_named_whatever_the_level() {
        let v = Verbalizer;
        for p in [Punct::None, Punct::Some, Punct::Most, Punct::All] {
            let o = opts(p);
            assert_eq!(v.unit(" ", How::Char, &o), "space");
            assert_eq!(v.unit("\t", How::Char, &o), "tab");
            assert_eq!(v.unit("\n", How::Char, &o), "blank");
            assert_eq!(v.unit(".", How::Char, &o), "dot");
            assert_eq!(v.unit("a", How::Char, &o), "a");
            assert_eq!(v.unit("\u{43f}", How::Char, &o), "\u{43f}");
            assert_eq!(v.unit("", How::Char, &o), "blank");
        }
    }

    #[test]
    fn capitals_follow_the_caps_setting() {
        let v = Verbalizer;
        let say = VerbalizeOpts {
            punct: Punct::Some,
            caps: Caps::Say,
        };
        assert_eq!(v.unit("A", How::Char, &say), "cap A");
        assert_eq!(v.unit("a", How::Char, &say), "a");
        assert_eq!(v.unit("A", How::Char, &opts(Punct::Some)), "A");
    }

    #[test]
    fn words_and_lines_apply_the_punctuation_level() {
        let v = Verbalizer;
        assert_eq!(v.unit("a@b.c", How::Word, &opts(Punct::None)), "a@b.c");
        assert_eq!(v.unit("a@b.c", How::Word, &opts(Punct::Some)), "a at b.c");
        assert_eq!(
            v.unit("f(x)", How::Line, &opts(Punct::Most)),
            "f left paren x right paren"
        );
        assert_eq!(
            v.unit("Hi, you.\n", How::Line, &opts(Punct::All)),
            "Hi comma you dot"
        );
    }

    #[test]
    fn blank_lines_and_words_say_blank() {
        let v = Verbalizer;
        assert_eq!(v.unit("   \n", How::Line, &opts(Punct::Some)), "blank");
        assert_eq!(v.unit("\n", How::Word, &opts(Punct::Some)), "blank");
        assert_eq!(v.unit("", How::Text, &opts(Punct::Some)), "blank");
    }

    #[test]
    fn the_phantom_of_an_astral_scalar_is_never_spoken() {
        let v = Verbalizer;
        let o = opts(Punct::Some);
        assert_eq!(v.unit("\u{1F600}\u{FEFF}", How::Char, &o), "\u{1F600}");
        assert_eq!(
            v.unit("hi \u{1F600}\u{FEFF} there", How::Word, &o),
            "hi \u{1F600} there"
        );
        assert_eq!(v.unit("\u{FEFF}", How::Char, &o), "blank");
        assert_eq!(v.spell("a\u{1F600}\u{FEFF}", false, &o), "a, \u{1F600}");
        // An embedded object alone is an object; inside running text it is silent.
        assert_eq!(v.unit("\u{FFFC}", How::Char, &o), "object");
        assert_eq!(v.spell("\u{FFFC}", false, &o), "object");
        assert_eq!(v.unit("a\u{FFFC}b", How::Word, &o), "ab");
    }

    #[test]
    fn spelling_names_every_character() {
        let v = Verbalizer;
        let o = opts(Punct::Some);
        assert_eq!(v.spell("a b", false, &o), "a, space, b");
        assert_eq!(v.spell("ab", true, &o), "alpha, bravo");
        assert_eq!(v.spell("", false, &o), "blank");
        assert_eq!(v.spell("\u{43f}\u{440}", false, &o), "\u{43f}, \u{440}");
    }
}
