// SPDX-License-Identifier: GPL-3.0-or-later
//! The `GORCA_CPU_DISABLE` / `--cpu-disable` list (ADR-0027 section 6.1).

use crate::feature::{Feature, FeatureSet};

/// Features switched off for diagnosis or portable-path testing.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DisableList {
    /// `all` or `portable`: every dispatched kernel runs its portable function.
    pub all: bool,
    pub features: FeatureSet,
    /// Names that are not features gorca knows (typos). Reported once, otherwise ignored.
    pub unknown: Vec<String>,
}

impl DisableList {
    /// Parse a comma or space separated list. Empty input disables nothing.
    #[must_use]
    pub fn parse(list: &str) -> DisableList {
        let mut d = DisableList::default();
        for tok in list.split(|c: char| c == ',' || c.is_whitespace()) {
            let tok = tok.trim();
            if tok.is_empty() {
                continue;
            }
            if tok.eq_ignore_ascii_case("all") || tok.eq_ignore_ascii_case("portable") {
                d.all = true;
            } else if let Some(f) = Feature::parse(tok) {
                d.features.insert(f);
            } else if !d.unknown.iter().any(|u| u == tok) {
                d.unknown.push(tok.to_owned());
            }
        }
        d
    }

    /// Nothing is disabled and nothing was misspelled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.all && self.features.is_empty() && self.unknown.is_empty()
    }

    /// `detected` minus what this list switches off.
    #[must_use]
    pub fn apply(&self, detected: FeatureSet) -> FeatureSet {
        if self.all {
            FeatureSet::EMPTY
        } else {
            detected.difference(self.features)
        }
    }

    /// Canonical text (`all`, or the sorted feature names, plus unknown names), for the report.
    #[must_use]
    pub fn describe(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if self.all {
            parts.push("all".into());
        }
        parts.extend(self.features.iter().map(|f| f.name().to_owned()));
        parts.extend(self.unknown.iter().map(|u| format!("{u}?")));
        if parts.is_empty() {
            "none".into()
        } else {
            parts.join(",")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lists() {
        assert!(DisableList::parse("").is_empty());
        let d = DisableList::parse("avx2, BMI2 sse4.2");
        assert_eq!(
            d.features,
            FeatureSet::of(&[Feature::Avx2, Feature::Bmi2, Feature::Sse42])
        );
        assert!(!d.all);
        assert!(DisableList::parse("all").all);
        assert!(DisableList::parse("Portable").all);
    }

    #[test]
    fn unknown_names_are_collected_once() {
        let d = DisableList::parse("avx2,avx3,avx3");
        assert_eq!(d.unknown, ["avx3"]);
        assert_eq!(d.describe(), "avx2,avx3?");
        assert!(!d.is_empty());
    }

    #[test]
    fn apply_removes_features() {
        let have = FeatureSet::of(&[Feature::Avx2, Feature::Bmi2]);
        assert_eq!(
            DisableList::parse("avx2").apply(have),
            FeatureSet::of(&[Feature::Bmi2])
        );
        assert_eq!(DisableList::parse("all").apply(have), FeatureSet::EMPTY);
    }
}
