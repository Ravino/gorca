//! `save`: write the overlay into the original text, keeping comments and order (HR-5).
//!
//! Pure: it takes the file text and returns the new text. Reading the file, the hash guard
//! (refuse when the file changed on disk) and the atomic rename belong to the daemon.

use toml_edit::{DocumentMut, Item, Table, Value, value};

use crate::settings::{Overlay, SCHEMA};

#[derive(Debug, Eq, PartialEq, thiserror::Error)]
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
