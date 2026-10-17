// SPDX-License-Identifier: GPL-3.0-or-later
//! Symbol tables per language (ADR-0007 VB-1/VB-2), modelled on NVDA's `symbols.dic`.
//!
//! A table maps a symbol to a spoken name, the lowest punctuation level at which the name is
//! spoken, and a preserve rule (keep the mark in the text so the server still pauses). The
//! tables are data files embedded at build time (`data/symbols-<lang>.dic`); the English table
//! is the base of every language, so a symbol a language does not list is named in English.

use std::collections::HashMap;
use std::sync::OnceLock;

const EN_DIC: &str = include_str!("../data/symbols-en.dic");
const RU_DIC: &str = include_str!("../data/symbols-ru.dic");

/// A language with its own symbol table. Anything else speaks the English one.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

impl Lang {
    /// The table language for a locale-ish code (`ru`, `ru_RU.UTF-8`, `ru-RU`); English when
    /// the language has no table.
    pub fn from_code(code: &str) -> Self {
        let base = code
            .trim()
            .split(['.', '@', '_', '-'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match base.as_str() {
            "ru" => Lang::Ru,
            _ => Lang::En,
        }
    }
}

/// The level at which a symbol is spoken by name. `Char` is spoken only when one character is
/// read; `None` is spoken at every level.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Level {
    None,
    Some,
    Most,
    All,
    Char,
}

impl Level {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Level::None,
            "some" => Level::Some,
            "most" => Level::Most,
            "all" => Level::All,
            "char" => Level::Char,
            _ => return None,
        })
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Preserve {
    Never,
    Always,
    Norep,
}

#[derive(Clone, Debug)]
pub(crate) struct Sym {
    name: Box<str>,
    level: Level,
    preserve: Preserve,
}

/// The context-dependent rules, as in NVDA's `complexSymbols`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Rule {
    SentenceDot,
    SentenceBang,
    SentenceQuestion,
    PhraseSemi,
    PhraseColon,
    MultipleDot,
    DecimalPoint,
    DecimalComma,
    InwordApostrophe,
    NegativeNumber,
}

const RULES: [(&str, Rule); 10] = [
    ("sentence.dot", Rule::SentenceDot),
    ("sentence.bang", Rule::SentenceBang),
    ("sentence.question", Rule::SentenceQuestion),
    ("phrase.semi", Rule::PhraseSemi),
    ("phrase.colon", Rule::PhraseColon),
    ("multiple.dot", Rule::MultipleDot),
    ("decimal.point", Rule::DecimalPoint),
    ("decimal.comma", Rule::DecimalComma),
    ("inword.apostrophe", Rule::InwordApostrophe),
    ("negative.number", Rule::NegativeNumber),
];

#[derive(Clone, Debug, Default)]
pub struct Table {
    simple: HashMap<char, Sym>,
    /// Multi-character symbols (`...`), longest first.
    multi: Vec<(Box<str>, Sym)>,
    rules: HashMap<usize, Sym>,
    blank: Box<str>,
    object: Box<str>,
    cap: Box<str>,
    /// Spoken names of the four levels (`none` to `all`) and the confirmation sentence of a
    /// level change (`{}` stands for the level name).
    levels: [Box<str>; 4],
    confirm: Box<str>,
    /// The other `@word.*` entries (lock keys, history, sleep mode, battery): UI words the
    /// Reader composes its messages from, so a message is spoken in the speech language.
    words: HashMap<Box<str>, Box<str>>,
}

fn rule_index(r: Rule) -> usize {
    RULES.iter().position(|(_, x)| *x == r).unwrap_or(0)
}

impl Table {
    /// Overlay the lines of `src` onto the table. Lines that do not parse are skipped; a test
    /// proves the embedded files have none.
    fn overlay(&mut self, src: &str) -> usize {
        let mut bad = 0;
        for line in src.lines() {
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if self.line(line).is_none() {
                bad += 1;
            }
        }
        bad
    }

    fn line(&mut self, line: &str) -> Option<()> {
        let mut f = line.split('\t');
        let key = f.next()?;
        let name = f.next()?;
        if let Some(id) = key.strip_prefix('@') {
            match id {
                "word.blank" => self.blank = name.into(),
                "word.object" => self.object = name.into(),
                "word.cap" => self.cap = name.into(),
                "word.confirm" => self.confirm = name.into(),
                other if other.starts_with("word.") => {
                    self.words
                        .insert(other["word.".len()..].into(), name.into());
                }
                "level.none" => self.levels[0] = name.into(),
                "level.some" => self.levels[1] = name.into(),
                "level.most" => self.levels[2] = name.into(),
                "level.all" => self.levels[3] = name.into(),
                _ => {
                    let (_, rule) = RULES.iter().find(|(n, _)| *n == id)?;
                    let sym = Self::sym(name, f.next(), f.next())?;
                    self.rules.insert(rule_index(*rule), sym);
                }
            }
            return Some(());
        }
        let sym = Self::sym(name, f.next(), f.next())?;
        if let Some(hex) = key.strip_prefix("U+") {
            let c = char::from_u32(u32::from_str_radix(hex, 16).ok()?)?;
            self.simple.insert(c, sym);
        } else {
            let mut it = key.chars();
            let c = it.next()?;
            if it.next().is_none() {
                self.simple.insert(c, sym);
            } else {
                self.multi.retain(|(k, _)| **k != *key);
                self.multi.push((key.into(), sym));
                self.multi.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
            }
        }
        Some(())
    }

    fn sym(name: &str, level: Option<&str>, preserve: Option<&str>) -> Option<Sym> {
        let level = match level {
            Some(l) => Level::parse(l)?,
            None => Level::All,
        };
        let preserve = match preserve {
            None | Some("never") => Preserve::Never,
            Some("always") => Preserve::Always,
            Some("norep") => Preserve::Norep,
            Some(_) => return None,
        };
        Some(Sym {
            name: name.into(),
            level,
            preserve,
        })
    }

    fn build(lang: Lang) -> Self {
        let mut t = Table::default();
        t.overlay(EN_DIC);
        if lang == Lang::Ru {
            t.overlay(RU_DIC);
        }
        t
    }

    /// The table of `lang`, built once.
    pub fn of(lang: Lang) -> &'static Table {
        static EN: OnceLock<Table> = OnceLock::new();
        static RU: OnceLock<Table> = OnceLock::new();
        match lang {
            Lang::En => EN.get_or_init(|| Table::build(Lang::En)),
            Lang::Ru => RU.get_or_init(|| Table::build(Lang::Ru)),
        }
    }

    pub fn blank(&self) -> &str {
        &self.blank
    }

    pub fn object(&self) -> &str {
        &self.object
    }

    pub fn cap(&self) -> &str {
        &self.cap
    }

    /// A UI word by id (`lock.caps`, `history.empty`, ...); empty when the table has none.
    pub fn word(&self, id: &str) -> &str {
        self.words.get(id).map_or("", |w| &**w)
    }

    /// A UI word with each `{}` replaced, in order, by the next of `args`.
    pub fn word_with(&self, id: &str, args: &[&str]) -> String {
        let mut out = String::new();
        let mut rest = self.word(id);
        for a in args {
            let Some((head, tail)) = rest.split_once("{}") else {
                break;
            };
            out.push_str(head);
            out.push_str(a);
            rest = tail;
        }
        out.push_str(rest);
        out
    }

    /// The spoken name of a punctuation level (rank 0 `none` to 3 `all`).
    pub fn level_name(&self, rank: u8) -> &str {
        self.levels.get(usize::from(rank)).map_or("", |n| &**n)
    }

    /// What is said when the level changes to `rank`, in full or as the bare level name.
    pub fn level_confirmation(&self, rank: u8, brief: bool) -> String {
        let name = self.level_name(rank);
        if brief {
            name.to_owned()
        } else {
            self.confirm.replace("{}", name)
        }
    }

    /// The name of one character, whatever the level (VB-1), or `None` for a plain character.
    pub fn char_name(&self, c: char) -> Option<&str> {
        self.simple
            .get(&c)
            .map(|s| &*s.name)
            .filter(|n| !n.is_empty())
    }

    /// Replace the symbols of running text by `rank` (0 none, 1 some, 2 most, 3 all), with
    /// NVDA's rules: marks that end a sentence or phrase keep their pause, a run of four or
    /// more of one symbol is counted, a decimal point and a minus sign before a number are
    /// handled in place, and an apostrophe inside a word is a mark of its own.
    pub fn process(&self, s: &str, rank: u8) -> String {
        let cs: Vec<char> = s.chars().collect();
        let mut out = String::with_capacity(s.len() + 16);
        let mut i = 0;
        while i < cs.len() {
            let c = cs[i];
            // Spaces at the end of the text are dropped.
            if c == ' ' && cs[i..].iter().all(|x| *x == ' ') {
                break;
            }
            if c == '\n' {
                out.push('\n');
                i += 1;
                continue;
            }
            if let Some((rule, len)) = Self::rule_at(&cs, i)
                && let Some(sym) = self.rules.get(&rule_index(rule))
            {
                let text: String = cs[i..i + len].iter().collect();
                emit(&mut out, sym, &text, rank);
                i += len;
                continue;
            }
            if let Some((key, sym)) = self.multi.iter().find(|(k, _)| {
                let n = k.chars().count();
                cs.len() - i >= n && k.chars().zip(&cs[i..]).all(|(a, b)| a == *b)
            }) {
                emit(&mut out, sym, key, rank);
                i += key.chars().count();
                continue;
            }
            if let Some(sym) = self.simple.get(&c) {
                let run = cs[i..].iter().take_while(|x| **x == c).count();
                if run >= 4 {
                    let text: String = cs[i..i + run].iter().collect();
                    if usize::from(rank) >= level_rank(sym.level) && !sym.name.is_empty() {
                        out.push(' ');
                        out.push_str(&run.to_string());
                        out.push(' ');
                        out.push_str(&sym.name);
                        out.push(' ');
                    } else if sym.preserve != Preserve::Never {
                        out.push_str(&text);
                    } else {
                        out.push(' ');
                    }
                    i += run;
                    continue;
                }
                emit(&mut out, sym, c.encode_utf8(&mut [0; 4]), rank);
                i += 1;
                continue;
            }
            out.push(c);
            i += 1;
        }
        collapse(&out)
    }

    fn rule_at(cs: &[char], i: usize) -> Option<(Rule, usize)> {
        let c = cs[i];
        let prev = i.checked_sub(1).map(|p| cs[p]);
        let next = cs.get(i + 1).copied();
        let ends = |n: Option<char>| n.is_none_or(char::is_whitespace);
        let closer = |n: Option<char>| {
            n.is_none_or(|x| {
                x.is_whitespace() || matches!(x, '"' | '\'' | '\u{201d}' | '\u{2019}' | ')')
            })
        };
        let after_text = |p: Option<char>| p.is_some_and(|x| !x.is_whitespace() && x != c);
        match c {
            '.' => {
                if after_text(prev) && closer(next) {
                    return Some((Rule::SentenceDot, 1));
                }
                let run = cs[i..].iter().take_while(|x| **x == '.').count();
                if run >= 4 {
                    return Some((Rule::MultipleDot, run));
                }
                let before_ok = prev.is_none_or(|p| p.is_ascii_digit() || p == ' ' || p == '-');
                if before_ok && next.is_some_and(|n| n.is_ascii_digit()) {
                    return Some((Rule::DecimalPoint, 1));
                }
                None
            }
            '!' if after_text(prev) && closer(next) => Some((Rule::SentenceBang, 1)),
            '?' if after_text(prev) && closer(next) => Some((Rule::SentenceQuestion, 1)),
            ';' if after_text(prev) && ends(next) => Some((Rule::PhraseSemi, 1)),
            ':' if after_text(prev) && ends(next) => Some((Rule::PhraseColon, 1)),
            ',' => {
                let before_ok = prev.is_none_or(|p| p.is_ascii_digit() || p == ' ' || p == '-');
                (before_ok && next.is_some_and(|n| n.is_ascii_digit()))
                    .then_some((Rule::DecimalComma, 1))
            }
            '\'' | '\u{2019}' if prev.is_some_and(char::is_alphanumeric) => {
                Some((Rule::InwordApostrophe, 1))
            }
            '-' | '\u{2212}' => {
                let not_word = !prev.is_some_and(|p| p.is_alphanumeric() || p == '_');
                let mut n = next;
                if n.is_some_and(|x| matches!(x, '$' | '\u{a3}' | '\u{20ac}' | '\u{a5}' | '.')) {
                    n = cs.get(i + 2).copied();
                }
                (not_word && n.is_some_and(|x| x.is_ascii_digit()))
                    .then_some((Rule::NegativeNumber, 1))
            }
            _ => None,
        }
    }
}

fn level_rank(l: Level) -> usize {
    match l {
        Level::None => 0,
        Level::Some => 1,
        Level::Most => 2,
        Level::All => 3,
        Level::Char => 4,
    }
}

/// NVDA's replacement of one match: the name when the level reaches the symbol, and the mark
/// itself kept when the symbol is preserved.
fn emit(out: &mut String, sym: &Sym, text: &str, rank: u8) {
    let reached = usize::from(rank) >= level_rank(sym.level);
    let keep = sym.preserve == Preserve::Always || (sym.preserve == Preserve::Norep && !reached);
    if reached && !sym.name.is_empty() {
        out.push(' ');
        out.push_str(&sym.name);
        out.push_str(if keep { text } else { " " });
    } else if keep {
        out.push_str(text);
    } else {
        out.push(' ');
    }
}

/// Runs of spaces become one, and the ends are trimmed of spaces. A name is separated from
/// the word before it; a kept mark stays attached to the name before it ("comma,").
fn collapse(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c == ' ' && (out.is_empty() || out.ends_with(' ') || out.ends_with('\n')) {
            continue;
        }
        out.push(c);
    }
    let n = out.trim_end_matches(' ').len();
    out.truncate(n);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_tables_parse_without_a_bad_line() {
        let mut t = Table::default();
        assert_eq!(t.overlay(EN_DIC), 0);
        assert_eq!(t.overlay(RU_DIC), 0);
        assert!(t.simple.len() > 80);
        assert_eq!(t.rules.len(), RULES.len());
    }

    #[test]
    fn ui_words_are_localised_with_an_english_fallback() {
        let en = Table::of(Lang::En);
        let ru = Table::of(Lang::Ru);
        assert_eq!(
            en.word_with("lock.on", &[en.word("lock.caps")]),
            "Caps Lock on"
        );
        assert_eq!(
            ru.word_with("lock.off", &[ru.word("lock.num")]),
            "Num Lock выключен"
        );
        assert_eq!(en.word_with("battery.hm", &["2", "5"]), "2 h 5 min");
        assert_eq!(en.word("no.such"), "");
    }

    #[test]
    fn level_changes_are_confirmed_in_the_language() {
        let en = Table::of(Lang::En);
        assert_eq!(en.level_confirmation(1, false), "punctuation some");
        assert_eq!(en.level_confirmation(1, true), "some");
        let ru = Table::of(Lang::Ru);
        assert_eq!(ru.level_name(0), "без пунктуации");
        assert_eq!(ru.level_name(1), "немного");
        assert_eq!(ru.level_name(2), "большинство");
        assert_eq!(ru.level_name(3), "вся");
        assert_eq!(ru.level_confirmation(2, false), "пунктуация большинство");
        assert_eq!(ru.level_confirmation(0, false), "пунктуация без пунктуации");
    }

    #[test]
    fn language_codes_select_a_table() {
        assert_eq!(Lang::from_code("ru"), Lang::Ru);
        assert_eq!(Lang::from_code("ru_RU.UTF-8"), Lang::Ru);
        assert_eq!(Lang::from_code("ru-RU"), Lang::Ru);
        assert_eq!(Lang::from_code("de"), Lang::En);
        assert_eq!(Lang::from_code(""), Lang::En);
    }

    #[test]
    fn russian_names_come_from_the_russian_table() {
        let ru = Table::of(Lang::Ru);
        assert_eq!(ru.char_name('?'), Some("вопросительный знак"));
        assert_eq!(ru.char_name('\u{20bd}'), Some("рубль"));
        assert_eq!(ru.char_name('#'), Some("решётка"));
        assert_eq!(Table::of(Lang::En).char_name('#'), Some("number"));
        assert_eq!(Table::of(Lang::En).char_name('@'), Some("at"));
    }
}
