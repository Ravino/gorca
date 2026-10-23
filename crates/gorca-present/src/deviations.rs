// SPDX-License-Identifier: GPL-3.0-or-later
//! The allowed-differences list (`deviations.toml`, ADR-0022 section 2.11).

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
    #[error("deviation {index}: `until` is not an ISO date")]
    BadDate { index: usize },
}

/// One allowed difference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub id: String,
    pub surface: String,
    /// ISO date (`YYYY-MM-DD`) after which the row is expired.
    pub until: String,
}

/// The differences allowed to remain, each with a reason, a surface and an expiry.
#[derive(Clone, Debug, Default)]
pub struct Deviations {
    rows: Vec<Row>,
}

fn is_iso_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
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

    /// Parse a list. Every `[[deviation]]` needs `id`, `surface`, `reason` and an ISO `until`.
    ///
    /// # Errors
    /// [`DeviationsError`] for invalid TOML or an incomplete row.
    pub fn parse(src: &str) -> Result<Deviations, DeviationsError> {
        let doc: DocumentMut = src
            .parse()
            .map_err(|e: toml_edit::TomlError| DeviationsError::Syntax(e.to_string()))?;
        let mut rows = Vec::new();
        if let Some(tables) = doc.get("deviation").and_then(|i| i.as_array_of_tables()) {
            for (index, row) in tables.iter().enumerate() {
                let get = |field: &'static str| {
                    row.get(field)
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .ok_or(DeviationsError::Missing { index, field })
                };
                let until = get("until")?;
                if !is_iso_date(until) {
                    return Err(DeviationsError::BadDate { index });
                }
                get("reason")?;
                rows.push(Row {
                    id: get("id")?.to_owned(),
                    surface: get("surface")?.to_owned(),
                    until: until.to_owned(),
                });
            }
        }
        Ok(Deviations { rows })
    }

    /// Whether `id` is an allowed difference on `surface`.
    pub fn allows(&self, id: &str, surface: &str) -> bool {
        self.rows.iter().any(|r| r.id == id && r.surface == surface)
    }

    /// Ids of the rows whose `until` is before `today` (`YYYY-MM-DD`).
    pub fn expired(&self, today: &str) -> Vec<&str> {
        self.rows
            .iter()
            .filter(|r| r.until.as_str() < today)
            .map(|r| r.id.as_str())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// Today's date (UTC) as `YYYY-MM-DD`, from the system clock (civil-from-days).
pub fn today_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = i64::try_from(secs / 86_400).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_checked_in_list_parses_and_is_keyed_by_surface() {
        let d = Deviations::parse(BUILTIN).expect("deviations.toml parses");
        assert!(d.allows("content-derived-name", "focus"));
        assert!(
            !d.allows("content-derived-name", "browse"),
            "the same id on another surface is not allowed"
        );
        assert!(!d.allows("anything-else", "focus"));
        assert!(!d.is_empty());
    }

    #[test]
    fn no_row_of_the_checked_in_list_is_expired() {
        let d = Deviations::parse(BUILTIN).expect("deviations.toml parses");
        let today = today_utc();
        assert_eq!(
            d.expired(&today),
            Vec::<&str>::new(),
            "close the gap or renew the row with a reason (today {today})"
        );
    }

    #[test]
    fn expiry_compares_iso_dates() {
        let d = Deviations::parse(BUILTIN).expect("parses");
        assert_eq!(d.expired("2026-12-16").len(), d.len());
        assert!(d.expired("2026-12-15").is_empty());
        assert_eq!(today_utc().len(), 10);
    }

    #[test]
    fn a_row_needs_a_reason_and_a_date() {
        let bad = "[[deviation]]\nid = \"x\"\nsurface = \"focus\"\nuntil = \"2026-01-01\"\n";
        assert_eq!(
            Deviations::parse(bad).map(|d| d.len()),
            Err(DeviationsError::Missing {
                index: 0,
                field: "reason"
            })
        );
        let no_date =
            "[[deviation]]\nid = \"x\"\nsurface = \"focus\"\nreason = \"r\"\nuntil = \"soon\"\n";
        assert_eq!(
            Deviations::parse(no_date).map(|d| d.len()),
            Err(DeviationsError::BadDate { index: 0 })
        );
        assert!(matches!(
            Deviations::parse("[[deviation"),
            Err(DeviationsError::Syntax(_))
        ));
    }
}
