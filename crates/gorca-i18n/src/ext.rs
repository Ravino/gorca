// SPDX-License-Identifier: MIT OR Apache-2.0
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

/// Who put a namespace into the registry.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Owner {
    /// The daemon (first-party extensions' embedded catalogs, tests): never replaced by a loaded
    /// extension.
    Builtin,
    /// A loaded extension; removed when the host that loaded it stops.
    Extension,
}

struct Entry {
    owner: Owner,
    cats: [Option<Arc<ExtCatalog>>; Lang::ALL.len()],
}

type Registry = HashMap<String, Entry>;

fn registry() -> &'static RwLock<Registry> {
    static R: std::sync::OnceLock<RwLock<Registry>> = std::sync::OnceLock::new();
    R.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Why a loaded extension's catalog was refused.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum RegisterError {
    #[error("namespace `{0}` is already registered")]
    Taken(String),
    #[error("too many extension namespaces")]
    TooMany,
    #[error("invalid namespace `{0}`")]
    BadName(String),
}

/// Register (or replace) the catalog of a *built-in* namespace `ns` for `lang` (the daemon's
/// embedded copies of the first-party extensions' catalogs). A loaded extension must use
/// [`register_extension`], which can never replace this. Returns the lines that did not parse.
pub fn register(ns: &str, lang: Lang, text: &str) -> Vec<CatalogError> {
    let (cat, errs) = ExtCatalog::parse(text);
    let mut g = registry().write().unwrap_or_else(PoisonError::into_inner);
    if !g.contains_key(ns) && g.len() >= MAX_NAMESPACES {
        return errs;
    }
    let e = g.entry(ns.to_owned()).or_insert_with(|| Entry {
        owner: Owner::Builtin,
        cats: Default::default(),
    });
    e.owner = Owner::Builtin;
    e.cats[lang.index()] = Some(Arc::new(cat));
    errs
}

/// Register the catalogs of a loaded extension (one text per language) under its namespace, all
/// or nothing. A namespace that is already registered (by the core, a first-party extension or
/// another extension) is refused: an extension can never overwrite someone else's texts.
/// Returns the lines that did not parse.
pub fn register_extension(
    ns: &str,
    files: &[(Lang, &str)],
) -> Result<Vec<CatalogError>, RegisterError> {
    let valid = !ns.is_empty()
        && ns.len() <= 64
        && ns
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.');
    if !valid {
        return Err(RegisterError::BadName(ns.to_owned()));
    }
    let mut errs = Vec::new();
    let mut cats: [Option<Arc<ExtCatalog>>; Lang::ALL.len()] = Default::default();
    for (lang, text) in files {
        let (cat, e) = ExtCatalog::parse(text);
        errs.extend(e);
        cats[lang.index()] = Some(Arc::new(cat));
    }
    let mut g = registry().write().unwrap_or_else(PoisonError::into_inner);
    if g.contains_key(ns) {
        return Err(RegisterError::Taken(ns.to_owned()));
    }
    if g.len() >= MAX_NAMESPACES {
        return Err(RegisterError::TooMany);
    }
    g.insert(
        ns.to_owned(),
        Entry {
            owner: Owner::Extension,
            cats,
        },
    );
    Ok(errs)
}

/// Forget the catalogs of a loaded extension (it was unloaded). Built-in namespaces stay.
pub fn unregister_extension(ns: &str) {
    let mut g = registry().write().unwrap_or_else(PoisonError::into_inner);
    if g.get(ns).is_some_and(|e| e.owner == Owner::Extension) {
        g.remove(ns);
    }
}

/// Format `key` of extension `ns` in `lang`, falling back to the extension's English text.
pub fn format(ns: &str, key: &str, lang: Lang, args: &[(&str, Arg<'_>)]) -> Option<String> {
    let g = registry().read().unwrap_or_else(PoisonError::into_inner);
    let entry = &g.get(ns)?.cats;
    let (cat, l) = match entry[lang.index()]
        .as_ref()
        .filter(|c| c.msgs.contains_key(key))
    {
        Some(c) => (c, lang),
        None => (entry[Lang::En.index()].as_ref()?, Lang::En),
    };
    let t = cat.msgs.get(key)?;
    let mut s = String::new();
    t.format_capped(&mut s, l, args, MAX_OUT_CHARS * 4);
    if let Some((i, _)) = s.char_indices().nth(MAX_OUT_CHARS) {
        s.truncate(i);
    }
    // A resolved text is never itself a reference.
    s.retain(|c| c != START && c != SEP);
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

/// Remove the reference marker characters from text that comes from an application (an
/// accessible name, a screen line) before it is handed to an extension, so that app text can
/// never be taken for a message reference when the extension passes it back.
pub fn sanitize(text: &str) -> Cow<'_, str> {
    if text.contains([START, SEP]) {
        Cow::Owned(text.chars().filter(|&c| c != START && c != SEP).collect())
    } else {
        Cow::Borrowed(text)
    }
}

/// Resolve a message reference in `lang`; any other text is returned as it is. A reference to a
/// message the extension does not have resolves to its key, so the user hears something
/// recognisable and the extension author sees the mistake. Trusted callers only (the core's own
/// text); text produced by an extension goes through [`resolve_for`].
pub fn resolve(text: &str, lang: Lang) -> Cow<'_, str> {
    resolve_in(text, lang, None)
}

/// Like [`resolve`] for text produced by extension `ns`: a reference into any other namespace
/// resolves to nothing, so an extension can only speak its own messages.
pub fn resolve_for<'a>(text: &'a str, lang: Lang, ns: &str) -> Cow<'a, str> {
    resolve_in(text, lang, Some(ns))
}

/// Text produced by extension `ns` that is going to be resolved later by the core: a reference
/// into any other namespace is dropped (it resolves to nothing), everything else is kept as it is.
pub fn restrict_to<'a>(text: &'a str, ns: &str) -> Cow<'a, str> {
    let foreign = text
        .strip_prefix(START)
        .and_then(|rest| rest.split(SEP).next())
        .and_then(|head| head.split_once(':'))
        .is_some_and(|(n, _)| n != ns);
    if foreign {
        Cow::Borrowed("")
    } else {
        Cow::Borrowed(text)
    }
}

fn resolve_in<'a>(text: &'a str, lang: Lang, only: Option<&str>) -> Cow<'a, str> {
    let Some(rest) = text.strip_prefix(START) else {
        return Cow::Borrowed(text);
    };
    if text.len() > MAX_REF_BYTES {
        return Cow::Borrowed("");
    }
    let mut parts = rest.split(SEP);
    let head = parts.next().unwrap_or("");
    let Some((ns, key)) = head.split_once(':') else {
        return Cow::Owned(sanitize(head).into_owned());
    };
    if only.is_some_and(|o| o != ns) {
        return Cow::Borrowed("");
    }
    let args: Vec<(&str, Arg<'_>)> = parts
        .take(MAX_REF_ARGS)
        .filter_map(|p| p.split_once('='))
        .map(|(n, v)| (n, Arg::Str(v)))
        .collect();
    Cow::Owned(format(ns, key, lang, &args).unwrap_or_else(|| sanitize(key).into_owned()))
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
        let mut many = String::new();
        for i in 0..MAX_ENTRIES + 500 {
            let _ = std::fmt::Write::write_fmt(&mut many, format_args!("k{i} = v\n"));
        }
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
    fn unregister_forgets_an_extension_namespace_but_not_a_builtin_one() {
        register_extension("gone-ext", &[(Lang::En, "a = b\n")]).unwrap();
        assert_eq!(resolve(&encode("gone-ext", "a", &[]), Lang::En), "b");
        unregister_extension("gone-ext");
        assert_eq!(resolve(&encode("gone-ext", "a", &[]), Lang::En), "a");
        register("keep-builtin", Lang::En, "a = kept\n");
        unregister_extension("keep-builtin");
        assert_eq!(resolve(&encode("keep-builtin", "a", &[]), Lang::En), "kept");
    }

    #[test]
    fn an_extension_cannot_take_a_registered_namespace() {
        register("owned-core", Lang::En, "a = core text\n");
        let r = register_extension("owned-core", &[(Lang::En, "a = evil\n")]);
        assert_eq!(r, Err(RegisterError::Taken("owned-core".to_owned())));
        assert_eq!(
            resolve(&encode("owned-core", "a", &[]), Lang::En),
            "core text"
        );
        register_extension("first-ext", &[(Lang::En, "a = one\n")]).unwrap();
        assert!(register_extension("first-ext", &[(Lang::En, "a = two\n")]).is_err());
        assert_eq!(resolve(&encode("first-ext", "a", &[]), Lang::En), "one");
        unregister_extension("first-ext");
        assert!(matches!(
            register_extension("bad name", &[]),
            Err(RegisterError::BadName(_))
        ));
    }

    #[test]
    fn an_extension_cannot_speak_another_extensions_messages() {
        register("scope-a", Lang::En, "a = from a\n");
        let r = encode("scope-a", "a", &[]);
        assert_eq!(resolve_for(&r, Lang::En, "scope-a"), "from a");
        assert_eq!(resolve_for(&r, Lang::En, "scope-b"), "");
        assert_eq!(resolve_for("plain text", Lang::En, "scope-b"), "plain text");
    }

    #[test]
    fn the_fallbacks_never_return_a_reference() {
        register("own-ns", Lang::En, "a = x\n");
        register("victim-ns", Lang::En, "secret = hidden\n");
        let victim = encode("victim-ns", "secret", &[]);
        // No `ns:key` head: the head is returned as text.
        let probe = format!("{START}{victim}");
        let out = resolve_for(&probe, Lang::En, "own-ns").into_owned();
        assert!(!is_reference(&out), "{out:?}");
        assert_eq!(resolve(&out, Lang::En), out);
        // Unknown key of the own namespace: the key is returned as text.
        let probe = format!("{START}own-ns:{victim}");
        let out = resolve_for(&probe, Lang::En, "own-ns").into_owned();
        assert!(!is_reference(&out), "{out:?}");
        assert!(!resolve(&out, Lang::En).contains("hidden"));
    }

    #[test]
    fn restrict_to_drops_only_foreign_references() {
        let own = encode("scope-e", "a", &[]);
        let foreign = encode("scope-f", "a", &[]);
        assert_eq!(restrict_to(&own, "scope-e"), own);
        assert_eq!(restrict_to(&foreign, "scope-e"), "");
        assert_eq!(restrict_to("blank", "scope-e"), "blank");
    }

    #[test]
    fn app_text_with_the_marker_is_never_a_reference() {
        register("scope-c", Lang::En, "a = secret\n");
        let hostile = format!("{START}scope-c:a");
        let clean = sanitize(&hostile);
        assert_eq!(clean, "scope-c:a");
        assert_eq!(resolve(&clean, Lang::En), "scope-c:a");
        assert!(matches!(sanitize("plain"), Cow::Borrowed(_)));
        // A catalog text cannot smuggle the marker into the output either.
        register("scope-d", Lang::En, "a = x\u{1e}y\n");
        let out = resolve(&encode("scope-d", "a", &[]), Lang::En).into_owned();
        assert!(!out.contains(START));
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
