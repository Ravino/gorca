//! `load`: parse, migrate, validate and layer. It never fails and never panics (ADR-0005 7.4).
//!
//! Parsing goes to a TOML tree first and every key is extracted on its own (CV-1), so one bad
//! key never invalidates the others.

use toml_edit::{Document, DocumentMut, Item};

use crate::diag::{Diag, DiagMsg, did_you_mean, fnv1a};
use crate::keymap::{ContextName, LayerEntry, ResolvedKeymap, merge_layer};
use crate::presets::Presets;
use crate::settings::{
    Confirmations, InterruptPolicy, Layout, Preset, SCHEMA, SETTINGS, Settings, Verbosity, sections,
};

/// Files larger than this are ignored (ADR-0005 7.4).
pub const MAX_FILE_BYTES: usize = 1024 * 1024;

const KNOWN_MODIFIERS: &[&str] = &["Insert", "KP_Insert", "Caps_Lock"];

/// The result of loading. Always usable.
#[derive(Clone, Debug)]
pub struct Loaded {
    pub settings: Settings,
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
                read_section(s, item, &mut out.settings, diags, quiet);
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

fn read_section(section: &str, item: &Item, s: &mut Settings, diags: &mut Vec<Diag>, quiet: bool) {
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
        apply_key(def.section, def.key, v, s, diags);
    }
}

fn apply_key(section: &str, key: &str, v: &Item, s: &mut Settings, diags: &mut Vec<Diag>) {
    let path = format!("{section}.{key}");
    let b = |s: &mut bool, diags: &mut Vec<Diag>| {
        if let Some(x) = v.as_bool() {
            *s = x;
        } else {
            diags.push(Diag::error(
                path.clone(),
                span_of(v),
                DiagMsg::WrongType {
                    expected: "true or false",
                },
            ));
        }
    };
    match (section, key) {
        ("keyboard", "preset") => {
            if let Some(p) = read_enum(v, &path, Preset::parse, Preset::NAMES, diags) {
                s.keyboard.preset = p;
            }
        }
        ("keyboard", "layout") => {
            if let Some(p) = read_enum(v, &path, Layout::parse, Layout::NAMES, diags) {
                s.keyboard.layout = p;
            }
        }
        ("keyboard", "modifiers") => {
            if let Some(m) = read_modifiers(v, &path, diags) {
                s.keyboard.modifiers = m;
            }
        }
        ("keyboard", "caps_lock_layout_guard") => b(&mut s.keyboard.caps_lock_layout_guard, diags),
        ("keyboard", "multi_press_ms") => {
            if let Some(n) = read_int(v, &path, 100, 2000, diags) {
                s.keyboard.multi_press_ms = u32::try_from(n).unwrap_or(500);
            }
        }
        ("keyboard", "double_tap_guard_ms") => {
            if let Some(n) = read_int(v, &path, 0, 100, diags) {
                s.keyboard.double_tap_guard_ms = u32::try_from(n).unwrap_or(15);
            }
        }
        ("keyboard", "interrupt") => {
            if let Some(p) = read_enum(
                v,
                &path,
                InterruptPolicy::parse,
                InterruptPolicy::NAMES,
                diags,
            ) {
                s.keyboard.interrupt = p;
            }
        }
        ("keyboard", "stop_speech_on_control") => b(&mut s.keyboard.stop_speech_on_control, diags),
        ("keyboard", "pause_speech_on_shift") => b(&mut s.keyboard.pause_speech_on_shift, diags),
        ("keymap", "replace_preset") => b(&mut s.keyboard.replace_preset, diags),
        ("speech", "rate") => {
            if let Some(n) = read_int(v, &path, 0, 100, diags) {
                s.speech.rate = u8::try_from(n).unwrap_or(50);
            }
        }
        ("speech", "pitch") => {
            if let Some(n) = read_int(v, &path, 0, 100, diags) {
                s.speech.pitch = u8::try_from(n).unwrap_or(50);
            }
        }
        ("speech", "volume") => {
            if let Some(n) = read_int(v, &path, 0, 100, diags) {
                s.speech.volume = u8::try_from(n).unwrap_or(100);
            }
        }
        ("speech", "rate_step") => {
            if let Some(n) = read_int(v, &path, 1, 25, diags) {
                s.speech.rate_step = u8::try_from(n).unwrap_or(5);
            }
        }
        ("speech", "voice") => {
            if let Some(x) = v.as_str() {
                x.clone_into(&mut s.speech.voice);
            } else {
                diags.push(Diag::error(
                    path,
                    span_of(v),
                    DiagMsg::WrongType {
                        expected: "a string",
                    },
                ));
            }
        }
        ("echo", "typed_characters") => b(&mut s.echo.typed_characters, diags),
        ("echo", "typed_words") => b(&mut s.echo.typed_words, diags),
        ("echo", "command_keys") => b(&mut s.echo.command_keys, diags),
        ("verbosity", "level") => {
            if let Some(p) = read_enum(v, &path, Verbosity::parse, Verbosity::NAMES, diags) {
                s.verbosity.level = p;
            }
        }
        ("verbosity", "confirmations") => {
            if let Some(p) = read_enum(v, &path, Confirmations::parse, Confirmations::NAMES, diags)
            {
                s.verbosity.confirmations = p;
            }
        }
        // `SETTINGS` and this match are kept in step by the `every_setting_row_is_read` test.
        _ => {}
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

fn read_enum<T>(
    v: &Item,
    path: &str,
    parse: fn(&str) -> Option<T>,
    names: &'static str,
    diags: &mut Vec<Diag>,
) -> Option<T> {
    let Some(text) = v.as_str() else {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::WrongType {
                expected: "a string",
            },
        ));
        return None;
    };
    let parsed = parse(text);
    if parsed.is_none() {
        diags.push(Diag::error(
            path,
            span_of(v),
            DiagMsg::UnknownEnum {
                value: text.to_owned(),
                allowed: names,
            },
        ));
    }
    parsed
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
            apply_key("keymap", "replace_preset", v, &mut out.settings, diags);
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
    fn every_setting_row_is_read() {
        // Setting every key to a non-default value must change `Settings`, so a row without a
        // matching arm in `apply_key` is caught.
        let text = "\
[keyboard]\npreset=\"orca\"\nlayout=\"laptop\"\nmodifiers=[\"Insert\"]\ncaps_lock_layout_guard=false\n\
multi_press_ms=300\ndouble_tap_guard_ms=20\ninterrupt=\"never\"\nstop_speech_on_control=false\npause_speech_on_shift=false\n\
[keymap]\nreplace_preset=true\n[speech]\nrate=10\npitch=11\nvolume=12\nrate_step=7\nvoice=\"x\"\n\
[echo]\ntyped_characters=false\ntyped_words=true\ncommand_keys=true\n[verbosity]\nlevel=\"brief\"\nconfirmations=\"brief\"\n";
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
        assert_eq!(SETTINGS.len(), 20);
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
