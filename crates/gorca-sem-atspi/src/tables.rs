// SPDX-License-Identifier: GPL-3.0-or-later
//! The AT-SPI source's per-profile data tables, embedded as text (M22a-B5).
//!
//! This crate only carries the data; `gorca_fuse::quirk` parses and validates it (closed action
//! set, size caps) and the connector or doc source builds the `Profile` and `QuirkSet` from it.
//! Engine identity (which tables belong to Gecko, Blink, GTK4) lives here and in the
//! `quirks/profiles.toml` selector, never in the normaliser.

/// One source profile's tables.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Bundle {
    /// Profile name as the selector reports it.
    pub name: &'static str,
    /// `[[rule]]` document for `QuirkSet::parse`.
    pub quirks: &'static str,
    /// `[pi]` document for `parse_pi`.
    pub pi: &'static str,
    /// `[[row]]` document for `parse_inexpressible`.
    pub inexpressible: &'static str,
}

/// The profile used when no selector row matches.
pub const DEFAULT_PROFILE: &str = "default";

/// The source-profile selector document for `ProfileSelector::parse`.
pub const SELECTOR: &str = include_str!("../quirks/profiles.toml");

const PI_DEFAULT: &str = include_str!("../pi/default.toml");

/// Every shipped profile, the default first.
pub const BUNDLES: [Bundle; 4] = [
    Bundle {
        name: DEFAULT_PROFILE,
        quirks: include_str!("../quirks/default.toml"),
        pi: PI_DEFAULT,
        inexpressible: include_str!("../inexpressible/default.toml"),
    },
    Bundle {
        name: "gecko",
        quirks: include_str!("../quirks/gecko.toml"),
        pi: PI_DEFAULT,
        inexpressible: include_str!("../inexpressible/gecko.toml"),
    },
    Bundle {
        name: "blink",
        quirks: include_str!("../quirks/blink.toml"),
        pi: PI_DEFAULT,
        inexpressible: include_str!("../inexpressible/blink.toml"),
    },
    Bundle {
        name: "gtk4",
        quirks: include_str!("../quirks/gtk4.toml"),
        pi: PI_DEFAULT,
        inexpressible: include_str!("../inexpressible/gtk4.toml"),
    },
];

/// The tables of a profile by name.
pub fn bundle(name: &str) -> Option<&'static Bundle> {
    BUNDLES.iter().find(|b| b.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_the_default_exists() {
        for (i, b) in BUNDLES.iter().enumerate() {
            assert!(
                BUNDLES[i + 1..].iter().all(|o| o.name != b.name),
                "{}",
                b.name
            );
        }
        assert!(bundle(DEFAULT_PROFILE).is_some());
        assert!(bundle("nope").is_none());
    }
}
