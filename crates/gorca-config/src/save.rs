// SPDX-License-Identifier: GPL-3.0-or-later
//! `save`: write the overlay into the original text, keeping comments and order (HR-5).
//!
//! Pure: it takes the file text and returns the new text. Reading the file, the hash guard
//! (refuse when the file changed on disk) and the atomic rename belong to the daemon.

use toml_edit::{DocumentMut, Item, Table, Value, value};

use crate::profiles::{KeyRef, Profile};
use crate::settings::{Overlay, SCHEMA};

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SaveError {
    /// The original is not valid TOML: nothing can be merged safely.
    #[error("the settings file is not valid TOML: {0}")]
    Syntax(String),
    /// SV-3: never rewrite a file written by a newer gorca.
    #[error("the settings file is from a newer version (schema {0})")]
    NewerSchema(i64),
}

fn set(doc: &mut DocumentMut, sec: &str, key: &str, mut new: Value) {
    let slot = &mut doc[sec];
    if !slot.is_table() {
        *slot = Item::Table(Table::new());
    }
    if let Some(t) = slot.as_table_mut() {
        // Keep the old value's decoration: the comment after it stays.
        if let Some(old) = t.get(key).and_then(Item::as_value) {
            *new.decor_mut() = old.decor().clone();
        }
        t[key] = Item::Value(new);
    }
}

/// `original` with the overlay merged in and `schema` set to the current version.
pub fn save(original: &str, overlay: &Overlay) -> Result<String, SaveError> {
    let mut doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| SaveError::Syntax(e.message().to_owned()))?;
    if let Some(n) = doc.get("schema").and_then(Item::as_integer)
        && n > i64::from(SCHEMA)
    {
        return Err(SaveError::NewerSchema(n));
    }
    doc["schema"] = value(i64::from(SCHEMA));
    if let Some(v) = overlay.rate {
        set(&mut doc, "speech", "rate", Value::from(i64::from(v)));
    }
    if let Some(v) = overlay.pitch {
        set(&mut doc, "speech", "pitch", Value::from(i64::from(v)));
    }
    if let Some(v) = overlay.volume {
        set(&mut doc, "speech", "volume", Value::from(i64::from(v)));
    }
    if let Some(v) = overlay.typed_characters {
        set(&mut doc, "echo", "typed_characters", Value::from(v));
    }
    if let Some(v) = overlay.typed_words {
        set(&mut doc, "echo", "typed_words", Value::from(v));
    }
    if let Some(v) = overlay.command_keys {
        set(&mut doc, "echo", "command_keys", Value::from(v));
    }
    if let Some(v) = overlay.verbosity {
        set(&mut doc, "verbosity", "level", Value::from(v.name()));
    }
    Ok(doc.to_string())
}

// ---- ADR-0016: writing deltas into the global file and into profile files -----------------------

/// A [`Value`] as TOML.
fn to_toml(v: &crate::patch::Value) -> Value {
    use crate::patch::Value as V;
    match v {
        V::Bool(b) => Value::from(*b),
        V::Int(n) => Value::from(*n),
        V::Str(s) => Value::from(s.as_str()),
        V::StrList(l) => Value::Array(l.iter().map(String::as_str).collect()),
        V::Map(m) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, b) in m {
                t.insert(k, Value::from(*b));
            }
            Value::InlineTable(t)
        }
    }
}

/// Path segments of a key: `["speech", "rate"]`, `["ext", "foo", "level"]`.
fn segments(key: &KeyRef) -> Vec<String> {
    match key {
        KeyRef::Core(k) => vec![k.def().section.to_owned(), k.def().key.to_owned()],
        KeyRef::Ext(p) => p.split('.').map(str::to_owned).collect(),
    }
}

/// The table at `path` below `t`, created (and a non-table replaced) on the way.
fn table_at<'a>(t: &'a mut Table, path: &[String]) -> Option<&'a mut Table> {
    let Some((first, rest)) = path.split_first() else {
        return Some(t);
    };
    if !t.get(first).is_some_and(Item::is_table) {
        t.insert(first, Item::Table(Table::new()));
    }
    table_at(t.get_mut(first)?.as_table_mut()?, rest)
}

fn set_path(doc: &mut DocumentMut, key: &KeyRef, mut new: Value) -> bool {
    let segs = segments(key);
    let Some((last, parents)) = segs.split_last() else {
        return false;
    };
    let Some(t) = table_at(doc.as_table_mut(), parents) else {
        return false;
    };
    if let Some(old) = t.get(last).and_then(Item::as_value) {
        *new.decor_mut() = old.decor().clone();
    }
    t[last.as_str()] = Item::Value(new);
    true
}

fn remove_at(t: &mut Table, path: &[String]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return t.remove(first).is_some();
    }
    let Some(child) = t.get_mut(first).and_then(Item::as_table_mut) else {
        return false;
    };
    let removed = remove_at(child, rest);
    if removed && child.is_empty() {
        t.remove(first);
    }
    removed
}

fn parse_for_edit(original: &str) -> Result<DocumentMut, SaveError> {
    let doc: DocumentMut = original
        .parse()
        .map_err(|e: toml_edit::TomlError| SaveError::Syntax(e.message().to_owned()))?;
    if let Some(n) = doc.get("schema").and_then(Item::as_integer)
        && n > i64::from(SCHEMA)
    {
        return Err(SaveError::NewerSchema(n));
    }
    Ok(doc)
}

/// `original` with the given keys set (comments, order and the rest of the file kept), as used
/// for the global file and for profile files. Empty `original` gives a minimal valid file.
pub fn set_keys(
    original: &str,
    values: &[(KeyRef, crate::patch::Value)],
) -> Result<String, SaveError> {
    let mut doc = parse_for_edit(original)?;
    doc["schema"] = value(i64::from(SCHEMA));
    for (k, v) in values {
        set_path(&mut doc, k, to_toml(v));
    }
    Ok(doc.to_string())
}

/// `original` without the given keys; sections left empty are dropped. Everything else is kept.
pub fn remove_keys(original: &str, keys: &[KeyRef]) -> Result<String, SaveError> {
    let mut doc = parse_for_edit(original)?;
    for k in keys {
        remove_at(doc.as_table_mut(), &segments(k));
    }
    Ok(doc.to_string())
}

/// Set `enabled` in the `[profile]` table of a profile file.
pub fn set_profile_enabled(original: &str, enabled: bool) -> Result<String, SaveError> {
    let mut doc = parse_for_edit(original)?;
    doc["schema"] = value(i64::from(SCHEMA));
    if let Some(t) = table_at(doc.as_table_mut(), &["profile".to_owned()]) {
        t["enabled"] = value(enabled);
    }
    Ok(doc.to_string())
}

/// A new profile file (ADR-0016 section 2.8): header, triggers, then `values` on top of the
/// profile's own keys.
pub fn render_profile(p: &Profile, values: &[(KeyRef, crate::patch::Value)]) -> String {
    let mut doc = DocumentMut::new();
    doc["schema"] = value(i64::from(SCHEMA));
    let mut head = Table::new();
    head["name"] = value(p.name.as_str());
    head["enabled"] = value(p.enabled);
    head["manual"] = value(p.manual);
    doc["profile"] = Item::Table(head);
    if !p.triggers.is_empty() {
        let mut aot = toml_edit::ArrayOfTables::new();
        for t in &p.triggers {
            aot.push(t.to_table());
        }
        if let Some(h) = doc["profile"].as_table_mut() {
            h.insert("trigger", Item::ArrayOfTables(aot));
        }
    }
    for (k, v) in p.values.iter() {
        set_path(&mut doc, &KeyRef::Core(k), to_toml(v));
    }
    for (path, v) in &p.ext {
        set_path(&mut doc, &KeyRef::Ext(path.clone()), to_toml(v));
    }
    for (k, v) in values {
        set_path(&mut doc, k, to_toml(v));
    }
    doc.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::load::load;
    use crate::presets::Presets;
    use crate::settings::Verbosity;

    #[test]
    fn comments_and_order_survive_and_values_are_written() {
        let original = "# my settings\nschema = 1\n\n[speech]\n# faster please\nrate = 50 # old\npitch = 40\n\n[keymap.global]\n\"gorca+w\" = \"info.window_title\"\n";
        let o = Overlay {
            rate: Some(65),
            typed_words: Some(true),
            verbosity: Some(Verbosity::Brief),
            ..Overlay::default()
        };
        let out = save(original, &o).unwrap();
        assert!(out.starts_with("# my settings\n"));
        assert!(
            out.contains("# faster please\nrate = 65 # old\npitch = 40"),
            "{out}"
        );
        assert!(out.contains("[keymap.global]\n\"gorca+w\" = \"info.window_title\""));
        let l = load(Some(&out), &Presets::builtin());
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        assert_eq!(l.settings.speech.rate, 65);
        assert_eq!(l.settings.speech.pitch, 40);
        assert!(l.settings.echo.typed_words);
        assert_eq!(l.settings.verbosity.level, Verbosity::Brief);
    }

    #[test]
    fn a_missing_file_becomes_a_minimal_valid_one() {
        let o = Overlay {
            volume: Some(80),
            ..Overlay::default()
        };
        let out = save("", &o).unwrap();
        assert!(out.contains("schema = 1"));
        assert_eq!(
            load(Some(&out), &Presets::builtin()).settings.speech.volume,
            80
        );
    }

    #[test]
    fn newer_files_and_broken_files_are_refused() {
        assert_eq!(
            save("schema = 7\n", &Overlay::default()),
            Err(SaveError::NewerSchema(7))
        );
        assert!(matches!(
            save("[speech\n", &Overlay::default()),
            Err(SaveError::Syntax(_))
        ));
    }

    #[test]
    fn a_non_table_section_is_replaced_rather_than_panicking() {
        let o = Overlay {
            rate: Some(10),
            ..Overlay::default()
        };
        let out = save("speech = 3\n", &o).unwrap();
        assert!(out.contains("rate = 10"), "{out}");
    }
}
