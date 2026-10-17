//! Extension catalogs and message references.
//!
//! An extension ships its strings as `i18n/<lang>.msg` files (same format as the core catalogs)
//! under its own namespace, the extension name. Its code never writes user-facing prose: it
//! returns a *message reference* ([`encode`]) wherever the WIT 0.2 ABI takes a text, and the core
//! resolves the reference in the UI language when the text enters the presentation
//! ([`resolve`]). A text that is not a reference passes through unchanged, so an extension that
//! still sends literals keeps working (with its text in one language).
//!
//! Reference wire form: `U+001E ns ':' key (U+001F name '=' value)*`. The control characters
//! never occur in prose, so a reference cannot be confused with text.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use crate::catalog::{CatalogError, parse_catalog};
use crate::lang::Lang;
use crate::template::{Arg, Template};

/// Caps on what an extension can make the core hold or say (an extension is untrusted).
/// Catalog entries per extension and language.
pub const MAX_ENTRIES: usize = 1024;
/// Bytes of one template; a longer one is dropped at load.
pub const MAX_TEMPLATE_BYTES: usize = 1024;
/// Bytes of a message reference; a longer one resolves to nothing.
pub const MAX_REF_BYTES: usize = 4096;
/// Arguments of one reference.
pub const MAX_REF_ARGS: usize = 16;
/// Characters of a resolved message (the expansion of placeholders is cut here).
pub const MAX_OUT_CHARS: usize = 2048;
/// Bytes of a whole catalog file; a larger one is refused before it is parsed.
pub const MAX_CATALOG_BYTES: usize = 256 * 1024;
/// Extensions with a catalog.
pub const MAX_NAMESPACES: usize = 64;

const START: char = '\u{1e}';
const SEP: char = '\u{1f}';

/// The messages of one extension in one language.
#[derive(Debug, Default)]
pub struct ExtCatalog {
    msgs: HashMap<String, Template>,
}

impl ExtCatalog {
    /// Parse a catalog file. Bounded before anything is parsed or allocated: the raw text over
    /// [`MAX_CATALOG_BYTES`] is refused whole, a line over the template bound is skipped
    /// unparsed, and reading stops at [`MAX_ENTRIES`]. Lines that do not parse are returned.
    pub fn parse(text: &str) -> (ExtCatalog, Vec<CatalogError>) {
        let mut cat = ExtCatalog::default();
        let mut errs = Vec::new();
        if text.len() > MAX_CATALOG_BYTES {
            return (cat, errs);
        }
        for (i, raw) in text.lines().enumerate() {
            if cat.msgs.len() >= MAX_ENTRIES {
                break;
            }
            let l = raw.trim();
            if l.is_empty() || l.starts_with('#') || l.len() > MAX_TEMPLATE_BYTES + 130 {
                continue;
            }
            let (items, e) = parse_catalog(l);
            errs.extend(e.into_iter().map(|e| match e {
                CatalogError::NoEquals { .. } => CatalogError::NoEquals { line: i + 1 },
                other => other,
            }));
            for (k, t) in items {
                if k.len() <= 128 && t.source_len() <= MAX_TEMPLATE_BYTES {
                    cat.msgs.entry(k).or_insert(t);
                }
            }
        }
        (cat, errs)
    }

    /// The keys, for tests and parity checks.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.msgs.keys().map(String::as_str)
    }
}

type Registry = HashMap<String, [Option<Arc<ExtCatalog>>; Lang::ALL.len()]>;

fn registry() -> &'static RwLock<Registry> {
    static R: std::sync::OnceLock<RwLock<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) the catalog of extension `ns` for `lang`. Returns the lines that did not
/// parse.
pub fn register(ns: &str, lang: Lang, text: &str) -> Vec<CatalogError> {
    let (cat, errs) = ExtCatalog::parse(text);
    let mut g = registry().write().unwrap_or_else(PoisonError::into_inner);
    if !g.contains_key(ns) && g.len() >= MAX_NAMESPACES {
        return errs;
    }
    g.entry(ns.to_owned()).or_default()[lang.index()] = Some(Arc::new(cat));
    errs
}

/// Forget the catalogs of extension `ns` (the extension was unloaded).
pub fn unregister(ns: &str) {
    registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(ns);
}

/// Format `key` of extension `ns` in `lang`, falling back to the extension's English text.
pub fn format(ns: &str, key: &str, lang: Lang, args: &[(&str, Arg<'_>)]) -> Option<String> {
    let g = registry().read().unwrap_or_else(PoisonError::into_inner);
    let entry = g.get(ns)?;
    let (cat, l) = match entry[lang.index()].as_ref().filter(|c| c.msgs.contains_key(key)) {
        Some(c) => (c, lang),
        None => (entry[Lang::En.index()].as_ref()?, Lang::En),
    };
    let t = cat.msgs.get(key)?;
    let mut s = String::new();
    t.format_capped(&mut s, l, args, MAX_OUT_CHARS * 4);
    if let Some((i, _)) = s.char_indices().nth(MAX_OUT_CHARS) {
        s.truncate(i);
    }
    Some(s)
}

/// Build a message reference to `key` of extension `ns` with arguments.
pub fn encode(ns: &str, key: &str, args: &[(&str, &str)]) -> String {
    let mut s = String::with_capacity(16 + ns.len() + key.len());
    s.push(START);
    s.push_str(ns);
    s.push(':');
    s.push_str(key);
    for (n, v) in args {
        s.push(SEP);
        s.push_str(n);
        s.push('=');
        s.push_str(v);
    }
    s
}

/// Whether `text` is a message reference.
pub fn is_reference(text: &str) -> bool {
    text.starts_with(START)
}

/// Resolve a message reference in `lang`; any other text is returned as it is. A reference to a
/// message the extension does not have resolves to its key, so the user hears something
/// recognisable and the extension author sees the mistake.
pub fn resolve(text: &str, lang: Lang) -> Cow<'_, str> {
    let Some(rest) = text.strip_prefix(START) else {
        return Cow::Borrowed(text);
    };
    if text.len() > MAX_REF_BYTES {
        return Cow::Borrowed("");
    }
    let mut parts = rest.split(SEP);
    let head = parts.next().unwrap_or("");
    let Some((ns, key)) = head.split_once(':') else {
        return Cow::Owned(head.to_owned());
    };
    let args: Vec<(&str, Arg<'_>)> = parts
        .take(MAX_REF_ARGS)
        .filter_map(|p| p.split_once('='))
        .map(|(n, v)| (n, Arg::Str(v)))
        .collect();
    Cow::Owned(format(ns, key, lang, &args).unwrap_or_else(|| key.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reference_resolves_in_the_language_with_an_english_fallback() {
        register("t-ext", Lang::En, "pct = {v} percent\nonly-en = hello\n");
        register("t-ext", Lang::Ru, "pct = {v} процентов\n");
        let r = encode("t-ext", "pct", &[("v", "50")]);
        assert!(is_reference(&r));
        assert_eq!(resolve(&r, Lang::En), "50 percent");
        assert_eq!(resolve(&r, Lang::Ru), "50 процентов");
        assert_eq!(resolve(&encode("t-ext", "only-en", &[]), Lang::Ru), "hello");
    }

    #[test]
    fn plain_text_passes_through_and_a_missing_key_shows_the_key() {
        assert_eq!(resolve("ext hello", Lang::Ru), "ext hello");
        assert_eq!(resolve(&encode("nope", "k", &[]), Lang::En), "k");
    }

    #[test]
    fn hostile_catalogs_and_references_are_capped() {
        // A short template that expands a long argument many times is cut at the output cap.
        let big = "x".repeat(MAX_REF_BYTES - 200);
        register("hostile", Lang::En, "boom = {a}{a}{a}{a}{a}{a}{a}{a}\n");
        let r = encode("hostile", "boom", &[("a", &big)]);
        let out = resolve(&r, Lang::En);
        assert!(out.chars().count() <= MAX_OUT_CHARS);
        // An over-long reference resolves to nothing.
        let huge = "y".repeat(MAX_REF_BYTES);
        let r = encode("hostile", "boom", &[("a", &huge)]);
        assert_eq!(resolve(&r, Lang::En), "");
        // Too many entries and over-long templates are dropped at load.
        let many: String = (0..MAX_ENTRIES + 500).map(|i| format!("k{i} = v\n")).collect();
        let long = format!("long = {}\n", "z".repeat(MAX_TEMPLATE_BYTES + 1));
        register("hostile2", Lang::En, &format!("{long}{many}"));
        let r = encode("hostile2", "long", &[]);
        assert_eq!(resolve(&r, Lang::En), "long");
        let r = encode("hostile2", "k0", &[]);
        assert_eq!(resolve(&r, Lang::En), "v");
        let last = format!("k{}", MAX_ENTRIES + 400);
        let r = encode("hostile2", &last, &[]);
        assert_eq!(resolve(&r, Lang::En), last);
        // Arguments are never expanded again (no recursion).
        register("rec", Lang::En, "r = {a}\n");
        let r = encode("rec", "r", &[("a", "{a}")]);
        assert_eq!(resolve(&r, Lang::En), "{a}");
    }

    #[test]
    fn an_oversized_catalog_is_refused_before_parsing() {
        let huge = format!("a = b\n{}", "# pad\n".repeat(MAX_CATALOG_BYTES / 4));
        assert!(huge.len() > MAX_CATALOG_BYTES);
        register("oversize", Lang::En, &huge);
        let r = encode("oversize", "a", &[]);
        assert_eq!(resolve(&r, Lang::En), "a", "nothing was loaded");
        // A very long line is skipped without parsing.
        let line = format!("big = {}\nok = fine\n", "{x}".repeat(50_000));
        register("longline", Lang::En, &line);
        let r = encode("longline", "ok", &[]);
        assert_eq!(resolve(&r, Lang::En), "fine");
        let r = encode("longline", "big", &[]);
        assert_eq!(resolve(&r, Lang::En), "big");
    }

    #[test]
    fn unregister_forgets_the_namespace() {
        register("gone-ext", Lang::En, "a = b\n");
        unregister("gone-ext");
        assert_eq!(resolve(&encode("gone-ext", "a", &[]), Lang::En), "a");
    }

    #[test]
    fn plurals_work_with_reference_arguments() {
        register("pl-ext", Lang::Ru, "n = {c} {c#файл|файла|файлов}\n");
        register("pl-ext", Lang::En, "n = {c} file{c#|s}\n");
        let r = encode("pl-ext", "n", &[("c", "3")]);
        assert_eq!(resolve(&r, Lang::Ru), "3 файла");
        assert_eq!(resolve(&r, Lang::En), "3 files");
    }
}
