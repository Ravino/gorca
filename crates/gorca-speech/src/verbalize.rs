use std::sync::Arc;

use gorca_model::{SegmentKind, Utterance};
use gorca_output_api::{CAPITAL_PITCH_DELTA, Verbalized};

use crate::symbols::Lang;
use crate::symbols::Table;

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
    /// A higher pitch for a capital spoken alone: the Reader marks the utterance
    /// (`SegmentKind::Capital`) and the speech client raises the pitch for it.
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
    /// Selects the symbol table (names, levels, preserve rules).
    pub lang: Lang,
}

impl Punct {
    /// The rank of the level in a symbol table (`none` 0 up to `all` 3).
    fn rank(self) -> u8 {
        match self {
            Punct::None => 0,
            Punct::Some => 1,
            Punct::Most => 2,
            Punct::All => 3,
        }
    }
}

/// Turns an utterance into server text and a unit of text into what is spoken. Verbalisation
/// runs before enqueueing, on the producer's thread or `cpu`, never on the speech lane
/// (section 8.1, VB-4).
#[derive(Debug, Default)]
pub struct Verbalizer;

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

/// The Cyrillic letters in the order of the `spell-cyrillic` message.
const CYRILLIC: &str = "абвгдеёжзийклмнопрстуфхцчшщъыьэюя";

/// The phonetic word of `c` (a Latin letter from `spell-latin`, a Russian letter from
/// `spell-cyrillic`), if the language has one. The words of a message are separated by `|`.
fn phonetic_word(c: char, lang: gorca_i18n::Lang) -> Option<&'static str> {
    use gorca_i18n::MsgId as M;
    let lower = c.to_lowercase().next().unwrap_or(c);
    let (list, idx) = if lower.is_ascii_lowercase() {
        (
            gorca_i18n::t_in(lang, M::SpellLatin),
            usize::from(lower as u8 - b'a'),
        )
    } else {
        (
            gorca_i18n::t_in(lang, M::SpellCyrillic),
            CYRILLIC.chars().position(|x| x == lower)?,
        )
    };
    list.split('|').nth(idx).filter(|w| !w.is_empty())
}

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
        // A capital spoken alone gets a temporary pitch change (`speech.caps = "pitch"`).
        let capital = u.segments.iter().any(|s| s.kind == SegmentKind::Capital);
        Verbalized {
            text: Arc::from(text),
            lang: u.lang.clone(),
            pitch_delta: if capital { CAPITAL_PITCH_DELTA } else { 0 },
        }
    }

    /// What to say for the unit `s` read from a text (ADR-0007 section 3.2). Empty input is
    /// "blank".
    pub fn unit(&self, s: &str, how: How, o: &VerbalizeOpts) -> String {
        let t = Table::of(o.lang);
        if only_objects(s) {
            return t.object().to_owned();
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
        let t = Table::of(o.lang);
        let Some(c) = s.chars().next() else {
            return t.blank().to_owned();
        };
        if let Some(name) = t.char_name(c) {
            return name.to_owned();
        }
        if c.is_uppercase() && o.caps == Caps::Say {
            return format!("{} {s}", t.cap());
        }
        s.to_owned()
    }

    /// A word, a line or running text: whitespace-only is "blank" (VB-2); the symbols of the
    /// punctuation level are replaced by their names (NVDA rules, see `symbols`); the line
    /// break at the end is dropped.
    fn run(s: &str, o: VerbalizeOpts) -> String {
        let t = Table::of(o.lang);
        let s = s.trim_end_matches(['\n', '\r']);
        if s.chars().all(char::is_whitespace) {
            return t.blank().to_owned();
        }
        let said = t.process(s, o.punct.rank());
        if !said.is_empty() {
            return said;
        }
        // Nothing but marks the level does not name ("?" alone at `some`): saying nothing would
        // leave the user without a landing, so the marks are named like single characters.
        s.chars()
            .filter_map(|c| t.char_name(c))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Spell `s` letter by letter (`text.spell_*`), or with the phonetic alphabet for Latin
    /// letters when `phonetic` is set. Every character is named, so spelling a word that holds
    /// a space or a symbol says so.
    pub fn spell(&self, s: &str, phonetic: bool, o: &VerbalizeOpts) -> String {
        let t = Table::of(o.lang);
        if only_objects(s) {
            return t.object().to_owned();
        }
        let s = without_phantoms(s);
        let s = s.trim_end_matches(['\n', '\r']);
        if s.is_empty() {
            return t.blank().to_owned();
        }
        let mut parts: Vec<String> = Vec::new();
        for c in s.chars() {
            let word = if phonetic {
                phonetic_word(c, o.lang)
            } else {
                None
            };
            let part = if let Some(w) = word {
                w.to_owned()
            } else {
                let mut buf = [0u8; 4];
                Self::one_char(c.encode_utf8(&mut buf), *o)
            };
            if c.is_uppercase() && o.caps == Caps::Say && word.is_some() {
                parts.push(format!("{} {part}", t.cap()));
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
            lang: Lang::En,
        }
    }

    fn ru(punct: Punct) -> VerbalizeOpts {
        VerbalizeOpts {
            lang: Lang::Ru,
            ..opts(punct)
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
            lang: Lang::En,
        };
        assert_eq!(v.unit("A", How::Char, &say), "cap A");
        assert_eq!(v.unit("a", How::Char, &say), "a");
        assert_eq!(v.unit("A", How::Char, &opts(Punct::Some)), "A");
    }

    #[test]
    fn words_and_lines_apply_the_punctuation_level() {
        let v = Verbalizer;
        assert_eq!(v.unit("a@b.c", How::Word, &opts(Punct::None)), "a b c");
        assert_eq!(
            v.unit("a@b.c", How::Word, &opts(Punct::Some)),
            "a at b dot c"
        );
        assert_eq!(
            v.unit("f(x)", How::Line, &opts(Punct::Most)),
            "f left paren(x right paren)"
        );
        assert_eq!(
            v.unit("Hi, you.\n", How::Line, &opts(Punct::All)),
            "Hi comma, you dot."
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

    fn words(v: &Verbalizer, text: &str, o: VerbalizeOpts) -> String {
        v.unit(text, How::Text, &o)
    }

    const SENTENCE: &str = "\u{41f}\u{440}\u{438}\u{432}\u{435}\u{442}, \u{43a}\u{430}\u{43a} \u{434}\u{435}\u{43b}\u{430} \u{442}\u{432}\u{43e}\u{438}?";

    #[test]
    fn a_russian_sentence_by_level() {
        let v = Verbalizer;
        // Levels none to some keep the marks as they are, for the pause; most too (the comma
        // and the question mark are named at `all` only).
        for p in [Punct::None, Punct::Some, Punct::Most] {
            assert_eq!(words(&v, SENTENCE, ru(p)), SENTENCE, "{p:?}");
        }
        assert_eq!(
            words(&v, SENTENCE, ru(Punct::All)),
            "Привет запятая, как дела твои вопросительный знак?"
        );
        // Spelled out by word units too.
        assert_eq!(
            v.unit("Привет, ", How::Word, &ru(Punct::All)),
            "Привет запятая,"
        );
        assert_eq!(v.unit("Привет, ", How::Word, &ru(Punct::Some)), "Привет,");
    }

    #[test]
    fn a_russian_sentence_char_by_char_names_every_symbol() {
        let v = Verbalizer;
        for p in [Punct::None, Punct::Some, Punct::Most, Punct::All] {
            let o = ru(p);
            assert_eq!(v.unit(",", How::Char, &o), "запятая");
            assert_eq!(v.unit("?", How::Char, &o), "вопросительный знак");
            assert_eq!(v.unit(" ", How::Char, &o), "пробел");
            assert_eq!(v.unit("\u{43f}", How::Char, &o), "п");
            assert_eq!(v.unit("\u{2116}", How::Char, &o), "номер");
            assert_eq!(v.unit("\u{ab}", How::Char, &o), "левая ёлочка");
            assert_eq!(v.unit("\u{bb}", How::Char, &o), "правая ёлочка");
            assert_eq!(v.unit("\u{2014}", How::Char, &o), "тире");
            assert_eq!(v.unit("\u{2026}", How::Char, &o), "многоточие");
            assert_eq!(v.unit("\u{20bd}", How::Char, &o), "рубль");
        }
        // The same characters in English speech keep their English names.
        let en = opts(Punct::All);
        assert_eq!(v.unit(",", How::Char, &en), "comma");
        assert_eq!(v.unit("\u{2116}", How::Char, &en), "number sign");
    }

    #[test]
    fn levels_follow_the_nvda_table() {
        let v = Verbalizer;
        let at = |s: &str, p| v.unit(s, How::Word, &opts(p));
        // `some`: symbols such as @ # % + = are named, brackets are not.
        assert_eq!(at("a+b", Punct::None), "a b");
        assert_eq!(at("a+b", Punct::Some), "a plus b");
        assert_eq!(at("(x)", Punct::Some), "(x)");
        assert_eq!(at("(x)", Punct::Most), "left paren(x right paren)");
        // `all` adds the dollar sign and the quote at every level above.
        assert_eq!(at("$5", Punct::Most), "$5");
        assert_eq!(at("$5", Punct::All), "dollar 5");
        assert_eq!(at("\"x\"", Punct::Some), "x");
        assert_eq!(at("\"x\"", Punct::Most), "quote x quote");
    }

    #[test]
    fn repeated_symbols_are_counted() {
        let v = Verbalizer;
        assert_eq!(v.unit("-----", How::Line, &opts(Punct::All)), "5 dash");
        assert_eq!(v.unit("-----", How::Line, &ru(Punct::All)), "5 дефис");
        assert_eq!(v.unit("-----", How::Line, &opts(Punct::Some)), "-----");
        assert_eq!(v.unit("=====", How::Line, &ru(Punct::Some)), "5 равно");
        assert_eq!(
            v.unit("***", How::Line, &opts(Punct::Some)),
            "star star star"
        );
        assert_eq!(
            v.unit("a....b", How::Line, &opts(Punct::All)),
            "a multiple dots....b"
        );
    }

    #[test]
    fn digits_and_letters_around_a_symbol() {
        let v = Verbalizer;
        let en = |s: &str, p| v.unit(s, How::Text, &opts(p));
        // A decimal point and a decimal comma stay in the number at every level.
        assert_eq!(en("3.14", Punct::All), "3.14");
        assert_eq!(en("3.14", Punct::None), "3.14");
        assert_eq!(v.unit("3,14", How::Text, &ru(Punct::All)), "3,14");
        // A minus before a number is a minus; a hyphen inside a word is a dash by level.
        assert_eq!(en("-5", Punct::None), "minus 5");
        assert_eq!(v.unit("-5", How::Text, &ru(Punct::Some)), "минус 5");
        assert_eq!(en("well-known", Punct::Some), "well-known");
        assert_eq!(en("well-known", Punct::Most), "well dash-known");
        // An apostrophe inside a word is left alone until `all`.
        assert_eq!(en("don't", Punct::Most), "don't");
        assert_eq!(en("don't", Punct::All), "don apostrophe t");
        // The end of a sentence keeps its mark.
        assert_eq!(en("Hello.", Punct::Some), "Hello.");
        assert_eq!(en("Hello. Bye", Punct::All), "Hello dot. Bye");
        assert_eq!(en("what?!", Punct::All), "what question bang!");
    }

    #[test]
    fn terminal_command_line_by_level() {
        let v = Verbalizer;
        let line = "ls -la | grep \"x\"";
        assert_eq!(v.unit(line, How::Line, &opts(Punct::None)), "ls -la grep x");
        assert_eq!(v.unit(line, How::Line, &opts(Punct::Some)), "ls -la grep x");
        assert_eq!(
            v.unit(line, How::Line, &opts(Punct::Most)),
            "ls dash-la bar grep quote x quote"
        );
        assert_eq!(
            v.unit(line, How::Line, &ru(Punct::Most)),
            "ls дефис-la вертикальная черта grep кавычка x кавычка"
        );
    }

    #[test]
    fn a_say_all_paragraph_in_russian() {
        let v = Verbalizer;
        let para = "Цена: 3,5 ₽ (скидка — 10%). Звоните «сегодня»!";
        let some = v.unit(para, How::Text, &ru(Punct::Some));
        assert_eq!(
            some,
            "Цена: 3,5 рубль (скидка — 10 процент ). Звоните «сегодня»!"
        );
        let all = v.unit(para, How::Text, &ru(Punct::All));
        assert!(all.contains("двоеточие:"), "{all}");
        assert!(
            all.contains("левая круглая(") || all.contains("левая круглая ("),
            "{all}"
        );
        assert!(
            all.contains("тире—") && all.contains("левая ёлочка«"),
            "{all}"
        );
        assert!(all.ends_with("восклицательный знак!"), "{all}");
    }
}
