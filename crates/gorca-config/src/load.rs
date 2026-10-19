// SPDX-License-Identifier: GPL-3.0-or-later
//! `load`: parse, migrate, validate and layer. It never fails and never panics (ADR-0005 7.4).
//!
//! Parsing goes to a TOML tree first and every key is extracted on its own (CV-1), so one bad
//! key never invalidates the others.

use toml_edit::{Document, DocumentMut, Item};

use crate::diag::{Diag, DiagMsg, did_you_mean, fnv1a};
use crate::keymap::{ContextName, LayerEntry, ResolvedKeymap, merge_layer};
use crate::patch::{KeyId, SettingsPatch, Value, enum_name, set_value};
use crate::presets::Presets;
use crate::settings::{BoundaryMode, Caps, SCHEMA, SETTINGS, SettingType, Settings, sections};

/// Files larger than this are ignored (ADR-0005 7.4).
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

const KNOWN_MODIFIERS: &[&str] = &["Insert", "KP_Insert", "Caps_Lock"];

/// The result of loading. Always usable.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub settings: Settings,
    /// The keys the file sets (valid ones only), as a sparse layer: the user base L5 of the
    /// profile stack (ADR-0016 UB-1 needs to tell "set" from "default").
    pub patch: SettingsPatch,
    pub keymap: ResolvedKeymap,
    pub diags: Vec<Diag>,
    /// FNV-1a of the file text (of the empty string when the file is missing).
    pub hash: u64,
    /// The `schema` the file declared (the current one when absent).
    pub schema: u32,
    /// The file came from a newer gorca: `save` must refuse (SV-3).
    pub newer_schema: bool,
    /// `false` when the file could not be parsed or was too large: everything is defaults and a
    /// reload should keep the last good settings (HR-2).
    pub file_ok: bool,
}

/// Load `text` (`None` for a missing file). Never fails.
pub fn load(text: Option<&str>, presets: &Presets) -> Loaded {
    let mut diags = Vec::new();
    let hash = fnv1a(text.unwrap_or("").as_bytes());
    let mut out = Loaded {
        settings: Settings::default(),
        patch: SettingsPatch::new(),
        keymap: ResolvedKeymap::default(),
        diags: Vec::new(),
        hash,
        schema: SCHEMA,
        newer_schema: false,
        file_ok: true,
    };
    let mut user_entries = Vec::new();
    if let Some(text) = text {
        if text.len() > MAX_FILE_BYTES {
            diags.push(Diag::error(
                "",
                None,
                DiagMsg::TooLarge {
                    limit: MAX_FILE_BYTES,
                },
            ));
            out.file_ok = false;
        } else {
            match Document::parse(text) {
                Ok(doc) => {
                    let migrated = migrate_text(text, &doc, &mut diags, &mut out);
                    match migrated {
                        Some(m) => {
                            if let Ok(doc2) = Document::parse(m.as_str()) {
                                user_entries = read_document(&doc2, &mut out, &mut diags);
                            }
                        }
                        None => user_entries = read_document(&doc, &mut out, &mut diags),
                    }
                }
                Err(e) => {
                    let span = e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end)));
                    diags.push(Diag::error(
                        "",
                        span,
                        DiagMsg::Syntax(e.message().to_owned()),
                    ));
                    out.file_ok = false;
                }
            }
        }
    }
    build_keymap(&mut out, user_entries, presets, &mut diags);
    out.diags = diags;
    out
}

fn clamp_u32(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn span_of(item: &Item) -> Option<(u32, u32)> {
    item.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end)))
}

/// One schema migration step: from version `n` to `n + 1`.
pub type Migration = fn(&mut DocumentMut, &mut Vec<Diag>);

/// The chain of migrations; entry `i` migrates schema `i + 1` to `i + 2`. Empty while
/// `SCHEMA == 1`; every future step gets a golden-file test (SV-4).
pub(crate) const MIGRATIONS: &[Migration] = &[];

/// Run `chain` from `from` up to `SCHEMA` on `doc`.
pub(crate) fn migrate_with(
    doc: &mut DocumentMut,
    from: u32,
    chain: &[Migration],
    diags: &mut Vec<Diag>,
) {
    let start = usize::try_from(from.saturating_sub(1)).unwrap_or(usize::MAX);
    for step in chain.iter().skip(start) {
        step(doc, diags);
    }
}

/// Returns migrated text when the file declares an older schema.
fn migrate_text(
    text: &str,
    doc: &Document<&str>,
    diags: &mut Vec<Diag>,
    out: &mut Loaded,
) -> Option<String> {
    let declared = declared_schema(doc, diags, out);
    if declared >= SCHEMA {
        return None;
    }
    let mut m: DocumentMut = text.parse().ok()?;
    migrate_with(&mut m, declared, MIGRATIONS, diags);
    out.schema = SCHEMA;
    Some(m.to_string())
}

fn declared_schema(doc: &Document<&str>, diags: &mut Vec<Diag>, out: &mut Loaded) -> u32 {
    let Some(item) = doc.as_table().get("schema") else {
        // SV-1: absent means current.
        return SCHEMA;
    };
    match item.as_integer() {
        Some(n) if n >= 1 => {
            let n32 = u32::try_from(n).unwrap_or(u32::MAX);
            out.schema = n32.min(SCHEMA);
            if n32 > SCHEMA {
                out.newer_schema = true;
                diags.push(Diag::warning(
                    "schema",
                    span_of(item),
                    DiagMsg::NewerSchema {
                        found: n,
                        supported: SCHEMA,
                    },
                ));
                return SCHEMA;
            }
            n32
        }
        Some(_) | None => {
            diags.push(Diag::error("schema", span_of(item), DiagMsg::BadSchema));
            SCHEMA
        }
    }
}

fn read_document(doc: &Document<&str>, out: &mut Loaded, diags: &mut Vec<Diag>) -> Vec<LayerEntry> {
    let root = doc.as_table();
    let quiet = out.newer_schema;
    let mut user_entries = Vec::new();

    for (key, item) in root {
        match key {
            "schema" => {}
            "keymap" => user_entries = read_keymap_section(item, out, diags, quiet),
            s if sections().any(|x| x == s) => {
                read_section(s, item, out, diags, quiet);
            }
            other => {
                if !quiet {
                    diags.push(Diag::warning(
                        other,
                        span_of(item),
                        DiagMsg::UnknownKey {
                            suggestion: did_you_mean(other, sections().chain(["schema", "keymap"])),
                        },
                    ));
                }
            }
        }
    }
    user_entries
}

fn read_section(section: &str, item: &Item, out: &mut Loaded, diags: &mut Vec<Diag>, quiet: bool) {
    let Some(t) = item.as_table_like() else {
        diags.push(Diag::error(
            section,
            span_of(item),
            DiagMsg::WrongType {
                expected: "a table",
            },
        ));
        return;
    };
    for (key, v) in t.iter() {
        let Some(def) = SETTINGS
            .iter()
            .find(|d| d.section == section && d.key == key)
        else {
            if !quiet {
                diags.push(Diag::warning(
                    format!("{section}.{key}"),
                    span_of(v),
                    DiagMsg::UnknownKey {
                        suggestion: did_you_mean(
                            key,
                            SETTINGS
                                .iter()
                                .filter(|d| d.section == section)
                                .map(|d| d.key),
                        ),
                    },
                ));
            }
            continue;
        };
        if let Some(key) = KeyId::of(def.section, def.key) {
            apply_key(key, v, out, diags);
        }
    }
}

/// Read one schema row's TOML value as a typed [`Value`], reporting problems; `None` keeps the
/// default (CV-1). Shared by the global file and the profile files (PK-1: same type and range).
pub(crate) fn parse_value(
    key: KeyId,
    v: &Item,
    path: &str,
    diags: &mut Vec<Diag>,
) -> Option<Value> {
    let def = key.def();
    let wrong = |expected: &'static str, diags: &mut Vec<Diag>| {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::WrongType { expected },
        ));
    };
    match (def.section, def.key, def.ty) {
        ("keyboard", "modifiers", _) => read_modifiers(v, path, diags).map(Value::StrList),
        ("speech", "caps", _) => read_caps(v, path, diags)
            .map(|c| Value::StrList(c.names().into_iter().map(str::to_owned).collect())),
        ("editing", "boundary", _) => read_boundary(v, path, diags)
            .map(|m| Value::StrList(m.names().into_iter().map(str::to_owned).collect())),
        (_, _, SettingType::Bool) => {
            let b = v.as_bool();
            if b.is_none() {
                wrong("true or false", diags);
            }
            b.map(Value::Bool)
        }
        (_, _, SettingType::Int { min, max }) => read_int(v, path, min, max, diags).map(Value::Int),
        (_, _, SettingType::Enum(names)) => {
            let Some(text) = v.as_str() else {
                wrong("a string", diags);
                return None;
            };
            let name = enum_name(key, text);
            if name.is_none() {
                diags.push(Diag::error(
                    path,
                    span_of(v),
                    DiagMsg::UnknownEnum {
                        value: text.to_owned(),
                        allowed: names,
                    },
                ));
            }
            name.map(|n| Value::Str(n.to_owned()))
        }
        (_, _, SettingType::Str) => {
            let s = v.as_str();
            if s.is_none() {
                wrong("a string", diags);
            }
            s.map(|x| Value::Str(x.to_owned()))
        }
        (_, _, SettingType::StrList) => read_str_list(v, path, diags).map(Value::StrList),
        (_, _, SettingType::Map(_)) => {
            let Some(t) = v.as_table_like() else {
                wrong("a table of true or false", diags);
                return None;
            };
            let mut m = std::collections::BTreeMap::new();
            for (name, item) in t.iter() {
                match item.as_bool() {
                    Some(b) => {
                        m.insert(name.to_owned(), b);
                    }
                    None => diags.push(Diag::error(
                        format!("{path}.{name}"),
                        span_of(item),
                        DiagMsg::WrongType {
                            expected: "true or false",
                        },
                    )),
                }
            }
            Some(Value::Map(m))
        }
    }
}

fn apply_key(key: KeyId, v: &Item, out: &mut Loaded, diags: &mut Vec<Diag>) {
    let path = key.def().path();
    if let Some(val) = parse_value(key, v, &path, diags)
        && set_value(&mut out.settings, key, &val)
    {
        let _ = out.patch.set(key, val);
    }
}

fn read_int(v: &Item, path: &str, min: i64, max: i64, diags: &mut Vec<Diag>) -> Option<i64> {
    let Some(n) = v.as_integer() else {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::WrongType {
                expected: "a whole number",
            },
        ));
        return None;
    };
    let used = n.clamp(min, max);
    if used != n {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::OutOfRange {
                value: n,
                min,
                max,
                used,
            },
        ));
    }
    Some(used)
}

/// A list of non-empty strings, at most 32 of them.
fn read_str_list(v: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Vec<String>> {
    let Some(arr) = v.as_array() else {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::WrongType {
                expected: "a list of strings",
            },
        ));
        return None;
    };
    let mut out: Vec<String> = Vec::new();
    for e in arr {
        match e.as_str() {
            Some(x) if !x.is_empty() && out.len() < 32 => out.push(x.to_owned()),
            Some(_) | None => diags.push(Diag::error(
                path,
                e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end))),
                DiagMsg::WrongType {
                    expected: "a non-empty string",
                },
            )),
        }
    }
    Some(out)
}

/// `speech.caps`: a list of `pitch`, `sound` and `word` (any case). A single string still reads
/// (those three and the old `say` and `off`). Anything invalid (a wrong type, an unknown name)
/// reports a problem and keeps the default, so a typo never silences the indication; `[]` and
/// `"off"` turn it off on purpose.
fn read_caps(v: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Caps> {
    let mut caps = Caps::OFF;
    let mut bad = false;
    let mut name = |text: Option<&str>, span: Option<(u32, u32)>, diags: &mut Vec<Diag>| {
        let known = text.is_some_and(|t| caps.add(&t.to_ascii_lowercase()));
        if known {
            return;
        }
        bad = true;
        diags.push(Diag::error(
            path,
            span,
            match text {
                Some(t) => DiagMsg::UnknownEnum {
                    value: t.to_owned(),
                    allowed: Caps::NAMES,
                },
                None => DiagMsg::WrongType {
                    expected: "a list of strings",
                },
            },
        ));
    };
    if let Some(arr) = v.as_array() {
        for e in arr {
            let span = e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end)));
            name(e.as_str(), span, diags);
        }
    } else {
        name(v.as_str(), span_of(v), diags);
    }
    if bad {
        return None;
    }
    if caps.sound {
        diags.push(Diag::warning(
            path,
            span_of(v),
            DiagMsg::Other(
                "\"sound\" does not play yet on any speech backend (planned with the audio cue sink); the other options work"
                    .to_owned(),
            ),
        ));
    }
    Some(caps)
}

/// `editing.boundary`: a list of `cue`, `say` and `repeat`, or one of the strings `off`, `cue`,
/// `say`, `cue+say`, `repeat`.
fn read_boundary(v: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<BoundaryMode> {
    let mut mode = BoundaryMode::OFF;
    let mut bad = false;
    let mut name = |text: Option<&str>, span: Option<(u32, u32)>, diags: &mut Vec<Diag>| {
        if text.is_some_and(|t| mode.add(&t.to_ascii_lowercase())) {
            return;
        }
        bad = true;
        diags.push(Diag::error(
            path,
            span,
            match text {
                Some(t) => DiagMsg::UnknownEnum {
                    value: t.to_owned(),
                    allowed: BoundaryMode::NAMES,
                },
                None => DiagMsg::WrongType {
                    expected: "a list of strings",
                },
            },
        ));
    };
    if let Some(arr) = v.as_array() {
        for e in arr {
            let span = e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end)));
            name(e.as_str(), span, diags);
        }
    } else {
        name(v.as_str(), span_of(v), diags);
    }
    (!bad).then_some(mode)
}

fn read_modifiers(v: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Vec<String>> {
    let Some(arr) = v.as_array() else {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::WrongType {
                expected: "a list of key names",
            },
        ));
        return None;
    };
    let mut out: Vec<String> = Vec::new();
    for e in arr {
        let Some(name) = e.as_str() else {
            diags.push(Diag::error(
                path,
                e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end))),
                DiagMsg::WrongType {
                    expected: "a key name string",
                },
            ));
            continue;
        };
        match KNOWN_MODIFIERS
            .iter()
            .find(|k| k.eq_ignore_ascii_case(name))
        {
            Some(k) => {
                if !out.iter().any(|o| o == k) {
                    out.push((*k).to_owned());
                }
            }
            None => diags.push(Diag::error(
                path,
                e.span().map(|r| (clamp_u32(r.start), clamp_u32(r.end))),
                DiagMsg::UnknownEnum {
                    value: name.to_owned(),
                    allowed: "Insert, KP_Insert or Caps_Lock",
                },
            )),
        }
    }
    if out.is_empty() {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::Other("no valid modifier left, using the default".to_owned()),
        ));
        return None;
    }
    Some(out)
}

/// `[keymap.<context>]` tables of the user file, in file order.
fn read_keymap_section(
    item: &Item,
    out: &mut Loaded,
    diags: &mut Vec<Diag>,
    quiet: bool,
) -> Vec<LayerEntry> {
    let mut entries = Vec::new();
    let Some(t) = item.as_table_like() else {
        diags.push(Diag::error(
            "keymap",
            span_of(item),
            DiagMsg::WrongType {
                expected: "a table",
            },
        ));
        return entries;
    };
    for (key, v) in t.iter() {
        if key == "replace_preset" {
            if let Some(k) = KeyId::of("keymap", "replace_preset") {
                apply_key(k, v, out, diags);
            }
            continue;
        }
        let Some(ctx) = ContextName::parse(key) else {
            if !quiet {
                diags.push(Diag::warning(
                    format!("keymap.{key}"),
                    span_of(v),
                    if key.starts_with("app:") || v.is_table_like() && key.contains(':') {
                        DiagMsg::Other("application contexts need extensions, ignored".to_owned())
                    } else {
                        DiagMsg::UnknownContext
                    },
                ));
            }
            continue;
        };
        collect_context(ctx, v, &mut entries, diags);
    }
    entries
}

fn collect_context(
    ctx: ContextName,
    v: &Item,
    entries: &mut Vec<LayerEntry>,
    diags: &mut Vec<Diag>,
) {
    let Some(t) = v.as_table_like() else {
        diags.push(Diag::error(
            format!("keymap.{}", ctx.name()),
            span_of(v),
            DiagMsg::WrongType {
                expected: "a table",
            },
        ));
        return;
    };
    for (chord, cmd) in t.iter() {
        match cmd.as_str() {
            Some(c) => entries.push(LayerEntry {
                ctx,
                chord_text: chord.to_owned(),
                command: c.to_owned(),
                span: span_of(cmd),
            }),
            None => diags.push(Diag::error(
                format!("keymap.{}.\"{chord}\"", ctx.name()),
                span_of(cmd),
                DiagMsg::WrongType {
                    expected: "a command name",
                },
            )),
        }
    }
}

/// Preset layer (unless `replace_preset`), then the user layer.
fn build_keymap(
    out: &mut Loaded,
    user_entries: Vec<LayerEntry>,
    presets: &Presets,
    diags: &mut Vec<Diag>,
) {
    let mut map = ResolvedKeymap::default();
    if !out.settings.keyboard.replace_preset {
        let text = presets.get(out.settings.keyboard.preset, out.settings.keyboard.layout);
        let mut preset_diags = Vec::new();
        match Document::parse(text) {
            Ok(doc) => {
                let entries = preset_entries(&doc, &mut preset_diags);
                merge_layer(&mut map, entries, "preset", &mut preset_diags);
            }
            Err(e) => preset_diags.push(Diag::error(
                "preset",
                None,
                DiagMsg::Syntax(format!("built-in preset is invalid: {}", e.message())),
            )),
        }
        // A diagnostic here is a bug in a shipped file: the test suite keeps this list empty. At
        // runtime the preset diagnostics are still reported, never hidden.
        for mut d in preset_diags {
            d.span = None;
            d.path = format!("preset.{}", d.path);
            diags.push(d);
        }
    }
    merge_layer(&mut map, user_entries, "user", diags);
    out.keymap = map;
}

fn preset_entries(doc: &Document<&str>, diags: &mut Vec<Diag>) -> Vec<LayerEntry> {
    let mut entries = Vec::new();
    let Some(km) = doc.as_table().get("keymap").and_then(Item::as_table_like) else {
        return entries;
    };
    for (key, v) in km.iter() {
        if let Some(ctx) = ContextName::parse(key) {
            collect_context(ctx, v, &mut entries, diags);
        }
    }
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::Severity;
    use crate::{Backend, Layout, Preset, WordLevel};

    fn load_text(t: &str) -> Loaded {
        load(Some(t), &Presets::builtin())
    }

    #[test]
    fn missing_file_is_all_defaults_without_a_problem() {
        let l = load(None, &Presets::builtin());
        assert_eq!(l.settings, Settings::default());
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert!(l.file_ok);
        assert_eq!(l.schema, SCHEMA);
    }

    #[test]
    fn minimal_hand_written_files_are_valid() {
        let l = load_text("[speech]\nrate = 70\n");
        assert_eq!(l.settings.speech.rate, 70);
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert!(!l.newer_schema);
    }

    #[test]
    fn one_bad_key_does_not_invalidate_the_others() {
        let l = load_text(
            "[speech]\nrate = \"fast\"\npitch = 60\nvolume = 400\n[echo]\ntyped_words = 3\ntyped_characters = false\n",
        );
        assert_eq!(l.settings.speech.rate, 50, "wrong type: default");
        assert_eq!(l.settings.speech.pitch, 60);
        assert_eq!(l.settings.speech.volume, 100, "clamped to the maximum");
        assert!(!l.settings.echo.typed_words);
        assert!(!l.settings.echo.typed_characters);
        assert_eq!(l.diags.len(), 3, "{:?}", l.diags);
        assert!(l.diags.iter().all(|d| d.severity == Severity::Error));
        assert!(l.file_ok);
    }

    #[test]
    fn unknown_keys_and_sections_get_did_you_mean() {
        let l = load_text("[speech]\nvolum = 5\n[keybord]\nx = 1\n");
        let msgs: Vec<_> = l.diags.iter().map(ToString::to_string).collect();
        assert!(
            msgs.iter()
                .any(|m| m.contains("speech.volum") && m.contains("did you mean `volume`")),
            "{msgs:?}"
        );
        assert!(
            msgs.iter()
                .any(|m| m.contains("keybord") && m.contains("keyboard")),
            "{msgs:?}"
        );
    }

    #[test]
    fn unknown_enum_values_fall_back_with_a_diagnostic() {
        let l = load_text("[keyboard]\npreset = \"emacs\"\nlayout = \"laptop\"\n");
        assert_eq!(l.settings.keyboard.preset, Preset::Nvda);
        assert_eq!(l.settings.keyboard.layout, Layout::Laptop);
        assert_eq!(l.diags.len(), 1);
    }

    #[test]
    fn syntax_errors_give_defaults_and_a_span() {
        let text = "[speech]\nrate = = 5\n";
        let l = load_text(text);
        assert!(!l.file_ok);
        assert_eq!(l.settings, Settings::default());
        assert_eq!(l.diags.len(), 1);
        let (line, _) = l.diags[0].line_col(text).unwrap();
        assert_eq!(line, 2);
    }

    #[test]
    fn oversized_and_binary_inputs_never_panic() {
        let big = "#".repeat(MAX_FILE_BYTES + 1);
        let l = load_text(&big);
        assert!(!l.file_ok);
        assert_eq!(l.diags.len(), 1);
        for junk in [
            "\u{0}\u{1}",
            "[[[[",
            "a = ",
            "= 1",
            "[a.b.c]\nx=[1,{y=2}]",
            "schema = -3",
            "[keymap]\nglobal = 4",
        ] {
            let l = load_text(junk);
            assert!(l.settings.speech.rate <= 100);
        }
    }

    #[test]
    fn schema_rules() {
        // SV-1: absent is current.
        assert_eq!(load_text("").schema, SCHEMA);
        // SV-3: newer: known keys honoured, unknown ignored with one diagnostic.
        let l = load_text("schema = 9\n[speech]\nrate = 61\nfuture = 1\n[future]\nx = 1\n");
        assert!(l.newer_schema);
        assert_eq!(l.settings.speech.rate, 61);
        assert_eq!(l.diags.len(), 1, "{:?}", l.diags);
        assert!(matches!(
            l.diags[0].msg,
            DiagMsg::NewerSchema { found: 9, .. }
        ));
        // Nonsense schema values are reported and treated as current.
        let l = load_text("schema = 0\n");
        assert!(matches!(l.diags[0].msg, DiagMsg::BadSchema));
        assert!(!l.newer_schema);
    }

    #[test]
    fn migration_chain_runs_each_step_in_order() {
        fn m1(d: &mut DocumentMut, _: &mut Vec<Diag>) {
            d["a"] = toml_edit::value(1);
        }
        fn m2(d: &mut DocumentMut, _: &mut Vec<Diag>) {
            let a = d["a"].as_integer().unwrap_or(0);
            d["a"] = toml_edit::value(a + 10);
        }
        let mut doc: DocumentMut = "".parse().unwrap();
        migrate_with(&mut doc, 1, &[m1, m2], &mut Vec::new());
        assert_eq!(doc["a"].as_integer(), Some(11));
        let mut doc: DocumentMut = "a = 5".parse().unwrap();
        migrate_with(&mut doc, 2, &[m1, m2], &mut Vec::new());
        assert_eq!(
            doc["a"].as_integer(),
            Some(15),
            "starts at the declared version"
        );
    }

    #[test]
    fn user_keymap_layers_over_the_preset() {
        let l = load_text(
            "[keymap.global]\n\"gorca+w\" = \"info.window_title\"\n\"gorca+F12\" = \"none\"\n",
        );
        let g = l.keymap.table(ContextName::Global).unwrap();
        let w = crate::ChordSpec::parse("gorca+w").unwrap();
        assert_eq!(g[&w].as_deref(), Some("info.window_title"));
        let f12 = crate::ChordSpec::parse("gorca+F12").unwrap();
        assert_eq!(g[&f12], None, "unbound by the user");
        assert!(g.len() > 5, "the preset is still there");
    }

    #[test]
    fn replace_preset_starts_from_an_empty_keymap() {
        let l = load_text(
            "[keymap]\nreplace_preset = true\n[keymap.global]\n\"gorca+w\" = \"info.window_title\"\n",
        );
        assert_eq!(l.keymap.table(ContextName::Global).unwrap().len(), 1);
    }

    #[test]
    fn app_contexts_are_ignored_until_extensions_exist() {
        let l = load_text("[keymap.\"app:firefox\".browse]\n\"j\" = \"x\"\n");
        assert_eq!(l.diags.len(), 1);
        assert!(l.diags[0].to_string().contains("extensions"));
    }

    #[test]
    fn caps_reads_a_list_of_independent_options() {
        let c = |t: &str| load_text(t);
        let l = c("[speech]\ncaps = [\"pitch\", \"sound\", \"word\"]\n");
        assert!(
            l.diags
                .iter()
                .all(|d| d.severity == crate::Severity::Warning),
            "{:?}",
            l.diags
        );
        assert_eq!(l.settings.speech.caps.names(), ["pitch", "sound", "word"]);
        let l = c("[speech]\ncaps = [\"word\", \"sound\"]\n");
        assert_eq!(l.settings.speech.caps.names(), ["sound", "word"]);
        let l = c("[speech]\ncaps = []\n");
        assert!(l.diags.is_empty() && !l.settings.speech.caps.any());
        // The values of the old single-choice setting still read.
        let l = c("[speech]\ncaps = \"say\"\n");
        assert_eq!(l.settings.speech.caps.names(), ["word"]);
        let l = c("[speech]\ncaps = \"off\"\n");
        assert!(l.diags.is_empty() && !l.settings.speech.caps.any());
        let l = c("[speech]\ncaps = \"pitch\"\n");
        assert_eq!(l.settings.speech.caps, Caps::PITCH);
        // Names read in any case.
        let l = c("[speech]\ncaps = [\"Pitch\", \"WORD\"]\n");
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert_eq!(l.settings.speech.caps.names(), ["pitch", "word"]);
        let l = c("[speech]\ncaps = \"Pitch\"\n");
        assert_eq!(l.settings.speech.caps, Caps::PITCH);
        // Anything invalid keeps the default and says so: indication is never silenced by a typo.
        for bad in ["3", "\"loud\"", "[\"lod\"]", "[\"pitch\", \"loud\"]", "[1]"] {
            let l = c(&format!("[speech]\ncaps = {bad}\n"));
            assert_eq!(l.diags.len(), 1, "{bad}: {:?}", l.diags);
            assert!(l.diags[0].to_string().contains("using the default"));
            assert_eq!(l.settings.speech.caps, Caps::PITCH, "{bad}");
        }
        let l = c("[speech]\ncaps = [\"pitch\", \"loud\"]\n");
        assert!(l.diags[0].to_string().contains("loud"));
    }

    #[test]
    fn sound_is_accepted_with_a_warning_that_it_does_not_play_yet() {
        let l = load_text("[speech]\ncaps = [\"pitch\", \"sound\"]\n");
        assert_eq!(l.diags.len(), 1);
        assert_eq!(l.diags[0].severity, crate::Severity::Warning);
        assert!(l.diags[0].to_string().contains("does not play yet"));
        assert!(l.settings.speech.caps.sound && l.settings.speech.caps.pitch);
    }

    #[test]
    fn caps_pitch_change_is_a_percent() {
        let l = load_text("[speech]\ncaps_pitch_change = 12\n");
        assert_eq!(l.settings.speech.caps_pitch_change, 12);
        assert_eq!(Settings::default().speech.caps_pitch_change, 30);
        let l = load_text("[speech]\ncaps_pitch_change = 101\n");
        assert_eq!(l.diags.len(), 1);
    }

    #[test]
    fn editing_defaults_and_bad_values() {
        let d = Settings::default().editing;
        assert!(d.announce_clipboard);
        assert_eq!(d.boundary, BoundaryMode::CUE);
        assert_eq!(d.word, WordLevel::Auto);
        let l = load_text("[editing]\nboundary = \"loud\"\nword = \"huge\"\n");
        assert_eq!(l.diags.len(), 2, "{:?}", l.diags);
        assert_eq!(l.settings.editing, d);
    }

    #[test]
    fn every_setting_row_is_read() {
        // Setting every key to a non-default value must change `Settings`, so a row without a
        // matching arm in `apply_key` is caught.
        let text = "\
[keyboard]\npreset=\"orca\"\nlayout=\"laptop\"\nmodifiers=[\"Insert\"]\ncaps_lock_layout_guard=false\n\
multi_press_ms=300\ndouble_tap_guard_ms=20\ninterrupt=\"never\"\nstop_speech_on_control=false\npause_speech_on_shift=false\n\
[keymap]\nreplace_preset=true\n[speech]\nrate=10\npitch=11\nvolume=12\nrate_step=7\nvoice=\"x\"\nlanguage=\"ru\"\noutput_module=\"rhvoice\"\nbackend=\"ssip\"\npunctuation=\"all\"\ncaps=[\"word\"]\ncaps_pitch_change=10\n\
[echo]\ntyped_characters=false\ntyped_words=true\ncommand_keys=true\ninserted_text=false\n\
[editing]\nannounce_clipboard=false\nboundary=[\"cue\",\"say\"]\nword=\"coarse\"\n[text]\nsay_all_moves_caret=false\ncaret_edge_ms=50\ncaret_late_ms=300\n[browse]\nsay_all_on_load=false\ndescription_pause=false\n\
[terminal]\nsettle_ms=40\nburst_lines=20\nprompt_markers=[\"> \"]\nprompt_regex=\"^x\"\nsecret_regex=\"y$\"\n[mouse]\nenabled=false\n[verbosity]\nlevel=\"brief\"\nconfirmations=\"brief\"\n";
        let l = load_text(text);
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        let d = Settings::default();
        assert_ne!(l.settings.keyboard.preset, d.keyboard.preset);
        assert_ne!(l.settings.keyboard, d.keyboard);
        assert_ne!(l.settings.speech, d.speech);
        assert_ne!(l.settings.echo, d.echo);
        assert_ne!(l.settings.verbosity, d.verbosity);
        assert_eq!(l.settings.speech.rate_step, 7);
        assert_eq!(l.settings.keyboard.modifiers, ["Insert"]);
        assert_ne!(l.settings.text, d.text);
        assert_ne!(l.settings.browse, d.browse);
        assert_eq!(l.settings.terminal.settle_ms, 40);
        assert_eq!(l.settings.terminal.burst_lines, 20);
        assert_eq!(l.settings.terminal.prompt_markers, ["> "]);
        assert_eq!(l.settings.terminal.prompt_regex, "^x");
        assert_eq!(l.settings.terminal.secret_regex, "y$");
        assert_eq!(l.settings.speech.language, "ru");
        assert_eq!(l.settings.speech.output_module, "rhvoice");
        assert_eq!(l.settings.speech.backend, Backend::Ssip);
        assert_eq!(
            l.settings.editing.boundary,
            BoundaryMode {
                cue: true,
                say: true,
                repeat: false
            }
        );
        assert_eq!(l.settings.editing.word, WordLevel::Coarse);
        assert!(!l.settings.editing.announce_clipboard);
        assert!(!l.settings.mouse.enabled);
        assert_eq!(SETTINGS.len(), 44);
    }

    #[test]
    fn diagnostics_are_deterministic() {
        let t = "[speech]\nrate = \"x\"\nbogus = 1\n[nope]\n";
        let a = load_text(t);
        let b = load_text(t);
        assert_eq!(a.diags, b.diags);
        assert_eq!(a.hash, b.hash);
    }
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    fn b(text: &str) -> (BoundaryMode, usize) {
        let l = load(Some(text), &crate::Presets::builtin());
        (l.settings.editing.boundary, l.diags.len())
    }

    #[test]
    fn boundary_reads_strings_and_lists() {
        assert_eq!(b("[editing]\nboundary = \"off\"\n"), (BoundaryMode::OFF, 0));
        assert_eq!(b("[editing]\nboundary = []\n"), (BoundaryMode::OFF, 0));
        assert_eq!(b("[editing]\nboundary = \"cue\"\n"), (BoundaryMode::CUE, 0));
        assert_eq!(b("[editing]\nboundary = \"say\"\n"), (BoundaryMode::SAY, 0));
        assert_eq!(
            b("[editing]\nboundary = \"repeat\"\n"),
            (BoundaryMode::REPEAT, 0)
        );
        let both = BoundaryMode {
            cue: true,
            say: true,
            repeat: false,
        };
        assert_eq!(b("[editing]\nboundary = \"cue+say\"\n"), (both, 0));
        assert_eq!(b("[editing]\nboundary = [\"say\", \"cue\"]\n"), (both, 0));
        assert_eq!(
            b("[editing]\nboundary = \"silent\"\n"),
            (BoundaryMode::OFF, 0)
        );
        assert_eq!(b("[editing]\nboundary = [\"cue\", \"loud\"]\n").1, 1);
        assert_eq!(both.name(), "cue+say");
        assert_eq!(BoundaryMode::OFF.name(), "off");
    }
}
