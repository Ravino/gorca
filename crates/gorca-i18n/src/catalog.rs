// SPDX-License-Identifier: MIT OR Apache-2.0
//! The embedded catalogs: one immutable table per language, indexed by [`MsgId`].
//!
//! The tables are built once (first use) from the `catalog/<lang>/*.msg` files compiled into the
//! binary. Every template is parsed at load, so formatting a message is a walk over pre-split
//! pieces (no parsing, no hashing: the id is an index). A message the language does not translate
//! falls back to the English text; the CI test `every_message_is_translated` keeps that empty.

use std::collections::HashMap;
use std::sync::OnceLock;

use crate::lang::{Lang, language};
use crate::template::{Arg, Template, TemplateError};

include!(concat!(env!("OUT_DIR"), "/msg_id.rs"));

/// One problem in a catalog source.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum CatalogError {
    #[error("line {line}: expected `key = template`")]
    NoEquals { line: usize },
    #[error("line {line}: key `{key}`: {source}")]
    Template {
        line: usize,
        key: String,
        source: TemplateError,
    },
    #[error("line {line}: key `{key}` is defined twice")]
    Duplicate { line: usize, key: String },
}

/// Parse `key = template` lines (`#` comments and blank lines are skipped).
pub fn parse_catalog(text: &str) -> (Vec<(String, Template)>, Vec<CatalogError>) {
    let mut out = Vec::new();
    let mut errs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (i, raw) in text.lines().enumerate() {
        let line = i + 1;
        let l = raw.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((k, v)) = l.split_once('=') else {
            errs.push(CatalogError::NoEquals { line });
            continue;
        };
        let key = k.trim().to_owned();
        if !seen.insert(key.clone()) {
            errs.push(CatalogError::Duplicate { line, key });
            continue;
        }
        match Template::parse(v.trim()) {
            Ok(t) => out.push((key, t)),
            Err(source) => errs.push(CatalogError::Template { line, key, source }),
        }
    }
    (out, errs)
}

/// The table of one language.
#[derive(Debug)]
pub struct Catalog {
    lang: Lang,
    msgs: Box<[Template]>,
}

fn sources(lang: Lang) -> &'static [&'static str] {
    match lang {
        Lang::En => SOURCES_EN,
        Lang::Ru => SOURCES_RU,
    }
}

fn load(lang: Lang) -> (HashMap<String, Template>, Vec<CatalogError>) {
    let mut all = HashMap::new();
    let mut errs = Vec::new();
    for src in sources(lang) {
        let (items, e) = parse_catalog(src);
        errs.extend(e);
        all.extend(items);
    }
    (all, errs)
}

impl Catalog {
    fn build(lang: Lang) -> Catalog {
        let (mut own, _) = load(lang);
        let (mut en, _) = load(Lang::En);
        let msgs = MsgId::ALL
            .iter()
            .map(|id| {
                let mine = own.remove(id.key());
                let base = en.remove(id.key());
                mine.or(base).unwrap_or_else(Template::empty)
            })
            .collect();
        Catalog { lang, msgs }
    }

    /// The language of this table.
    pub fn lang(&self) -> Lang {
        self.lang
    }

    fn template(&self, id: MsgId) -> Option<&Template> {
        self.msgs.get(id as usize)
    }

    /// The text of a message without placeholders. Asking for one that has placeholders is a
    /// bug in the caller (it must use [`Catalog::format`]): it panics in debug builds and tests,
    /// and in release speaks the English text if that one is plain, else nothing.
    pub fn get(&self, id: MsgId) -> &str {
        if let Some(p) = self.template(id).and_then(Template::plain) {
            return p;
        }
        debug_assert!(
            false,
            "message `{}` has placeholders: use msg!(.., name = value), not t()",
            id.key()
        );
        if self.lang == Lang::En {
            ""
        } else {
            catalog(Lang::En).get(id)
        }
    }

    /// Format a message.
    pub fn format(&self, id: MsgId, args: &[(&str, Arg<'_>)]) -> String {
        let mut s = String::new();
        self.format_into(&mut s, id, args);
        s
    }

    /// Append a formatted message to `out` (no allocation of its own).
    pub fn format_into(&self, out: &mut String, id: MsgId, args: &[(&str, Arg<'_>)]) {
        if let Some(t) = self.template(id) {
            t.format_into(out, self.lang, args);
        }
    }
}

/// The embedded catalog of a language.
pub fn catalog(lang: Lang) -> &'static Catalog {
    static EN: OnceLock<Catalog> = OnceLock::new();
    static RU: OnceLock<Catalog> = OnceLock::new();
    match lang {
        Lang::En => EN.get_or_init(|| Catalog::build(Lang::En)),
        Lang::Ru => RU.get_or_init(|| Catalog::build(Lang::Ru)),
    }
}

/// The text of a message without placeholders, in the current language.
pub fn t(id: MsgId) -> &'static str {
    catalog(language()).get(id)
}

/// A message with placeholders, in the current language.
pub fn tf(id: MsgId, args: &[(&str, Arg<'_>)]) -> String {
    catalog(language()).format(id, args)
}

/// A message in an explicit language (the verbalizer carries its own).
pub fn t_in(lang: Lang, id: MsgId) -> &'static str {
    catalog(lang).get(id)
}

/// A message with placeholders in an explicit language.
pub fn tf_in(lang: Lang, id: MsgId, args: &[(&str, Arg<'_>)]) -> String {
    catalog(lang).format(id, args)
}

/// What is wrong with the embedded catalog of a language (empty when it is complete).
#[derive(Debug, Default)]
pub struct Report {
    pub errors: Vec<CatalogError>,
    /// Keys of the language that are not in the English catalog.
    pub unknown: Vec<String>,
    /// English messages the language does not translate.
    pub missing: Vec<MsgId>,
    /// Messages whose placeholder names differ from the English ones.
    pub placeholder_mismatch: Vec<MsgId>,
}

/// Check the embedded catalog of `lang` against the English one.
pub fn check(lang: Lang) -> Report {
    let (own, errors) = load(lang);
    let (en, _) = load(Lang::En);
    let mut r = Report {
        errors,
        ..Report::default()
    };
    r.unknown = own
        .keys()
        .filter(|k| MsgId::from_key(k).is_none())
        .cloned()
        .collect();
    r.unknown.sort();
    for id in MsgId::ALL {
        match (own.get(id.key()), en.get(id.key())) {
            (None, _) => r.missing.push(*id),
            (Some(a), Some(b)) if a.names() != b.names() => r.placeholder_mismatch.push(*id),
            _ => {}
        }
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_cleanly() {
        for l in Lang::ALL {
            let r = check(l);
            assert!(r.errors.is_empty(), "{l:?}: {:?}", r.errors);
            assert!(r.unknown.is_empty(), "{l:?}: unknown keys {:?}", r.unknown);
            assert!(
                r.placeholder_mismatch.is_empty(),
                "{l:?}: placeholders differ: {:?}",
                r.placeholder_mismatch
            );
        }
    }

    #[test]
    fn every_message_is_translated() {
        for l in Lang::ALL {
            let r = check(l);
            assert!(
                r.missing.is_empty(),
                "{l:?} lacks {} messages: {:?}",
                r.missing.len(),
                r.missing.iter().map(|m| m.key()).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn a_plain_message_is_returned_as_it_is() {
        for l in Lang::ALL {
            for id in MsgId::ALL {
                // Every message either is plain or has placeholders; `get` must only be asked
                // for the plain ones (checked here without tripping the debug assertion).
                if catalog(l).template(*id).and_then(Template::plain).is_some() {
                    let _ = catalog(l).get(*id);
                }
            }
        }
    }

    #[test]
    #[should_panic(expected = "has placeholders")]
    fn t_on_a_message_with_placeholders_fails_loudly_in_tests() {
        let id = MsgId::ALL
            .iter()
            .find(|id| {
                catalog(Lang::En)
                    .template(**id)
                    .and_then(Template::plain)
                    .is_none()
            })
            .copied()
            .expect("the catalog has a message with placeholders");
        let _ = catalog(Lang::En).get(id);
    }

    #[test]
    fn the_text_editing_messages_have_plurals_in_both_languages() {
        let n = |l, n: i32| tf_in(l, MsgId::TextChars, &[("n", Arg::from(n))]);
        assert_eq!(n(Lang::En, 1), "1 character");
        assert_eq!(n(Lang::En, 5), "5 characters");
        assert_eq!(n(Lang::Ru, 1), "1 символ");
        assert_eq!(n(Lang::Ru, 3), "3 символа");
        assert_eq!(n(Lang::Ru, 11), "11 символов");
        assert_eq!(t_in(Lang::En, MsgId::TextNewLine), "new line");
        assert_eq!(t_in(Lang::En, MsgId::StateAvailable), "not unavailable");
        assert_eq!(t_in(Lang::Ru, MsgId::StateAvailable), "доступно");
    }

    #[test]
    fn ids_round_trip_through_keys() {
        for id in MsgId::ALL {
            assert_eq!(MsgId::from_key(id.key()), Some(*id));
        }
    }

    #[test]
    fn a_duplicate_and_a_bad_line_are_reported() {
        let (items, errs) = parse_catalog("a = 1\nb\na = 2\nc = {x\n");
        assert_eq!(items.len(), 1);
        assert_eq!(errs.len(), 3);
    }
}
