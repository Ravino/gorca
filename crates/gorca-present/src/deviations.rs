// SPDX-License-Identifier: GPL-3.0-or-later
//! The allowed-differences list (`deviations.toml`, ADR-0022 section 2.11).

use std::collections::BTreeSet;

use toml_edit::DocumentMut;

/// The checked-in list, embedded.
const BUILTIN: &str = include_str!("../deviations.toml");

/// Why a deviations file could not be used.
#[derive(Debug, Eq, PartialEq, thiserror::Error)]
pub enum DeviationsError {
    #[error("deviations.toml is not valid TOML: {0}")]
    Syntax(String),
    #[error("deviation {index} has no `{field}`")]
    Missing { index: usize, field: &'static str },
}

/// The ids allowed to differ, each with a reason.
#[derive(Clone, Debug, Default)]
pub struct Deviations {
    ids: BTreeSet<String>,
}

impl Deviations {
    /// Nothing is allowed.
    pub fn empty() -> Deviations {
        Deviations::default()
    }

    /// The embedded list. A broken checked-in list is a test failure; at run time it degrades to
    /// "nothing allowed", which only makes the shadow stricter.
    pub fn builtin() -> Deviations {
        Deviations::parse(BUILTIN).unwrap_or_default()
    }

    /// Parse a list. Every `[[deviation]]` needs `id`, `surface` and `reason`.
    ///
    /// # Errors
    /// [`DeviationsError`] for invalid TOML or an incomplete row.
    pub fn parse(src: &str) -> Result<Deviations, DeviationsError> {
        let doc: DocumentMut = src
            .parse()
            .map_err(|e: toml_edit::TomlError| DeviationsError::Syntax(e.to_string()))?;
        let mut ids = BTreeSet::new();
        if let Some(rows) = doc.get("deviation").and_then(|i| i.as_array_of_tables()) {
            for (index, row) in rows.iter().enumerate() {
                let get = |field: &'static str| {
                    row.get(field)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or(DeviationsError::Missing { index, field })
                };
                let id = get("id")?;
                get("surface")?;
                get("reason")?;
                ids.insert(id.to_owned());
            }
        }
        Ok(Deviations { ids })
    }

    /// Whether `id` is an allowed difference.
    pub fn allows(&self, id: &str) -> bool {
        self.ids.contains(id)
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_list_parses_and_lists_value_not_fused() {
        let d = Deviations::parse(BUILTIN).expect("deviations.toml parses");
        assert!(d.allows("value-not-fused"));
        assert!(!d.allows("anything-else"));
        assert!(!d.is_empty());
    }

    #[test]
    fn a_row_needs_a_reason() {
        let bad = "[[deviation]]\nid = \"x\"\nsurface = \"focus\"\n";
        assert_eq!(
            Deviations::parse(bad).map(|d| d.len()),
            Err(DeviationsError::Missing {
                index: 0,
                field: "reason"
            })
        );
        assert!(matches!(
            Deviations::parse("[[deviation"),
            Err(DeviationsError::Syntax(_))
        ));
    }
}
