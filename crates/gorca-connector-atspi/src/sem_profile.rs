// SPDX-License-Identifier: GPL-3.0-or-later
//! The source profile of an application (M22a-C1b): which `gorca-sem-atspi` table bundle (quirk
//! rows, source priority, inexpressible groups) the core's `FUSE` runs with for this app. The
//! choice is data (`quirks/profiles.toml`) over what the connector knows; the result travels in
//! `AppInfo::sem_tables` as plain text, so the core never names a toolkit or this crate.

use std::sync::OnceLock;

use gorca_fuse::{AppFacts, ProfileSelector};
use gorca_model::{SemTables, WebEngine};
use gorca_sem_atspi::tables::{DEFAULT_PROFILE, SELECTOR, bundle};

fn selector() -> Option<&'static ProfileSelector> {
    static SEL: OnceLock<Option<ProfileSelector>> = OnceLock::new();
    SEL.get_or_init(|| match ProfileSelector::parse(SELECTOR) {
        Ok(s) => Some(s),
        Err(e) => {
            // The table is embedded and tested; a failure here is a build defect, and the
            // default profile is the safe answer.
            tracing::error!(error = %e, "source-profile selector does not parse");
            None
        }
    })
    .as_ref()
}

/// The tables for an application, by its toolkit attribute, the web engine the connector detected
/// and its name (names are only the selector's fallback rows). The default bundle when nothing
/// matches; `None` only if the embedded default is missing (it is not).
pub fn tables_for(toolkit: &str, app_name: &str, engine: WebEngine) -> Option<SemTables> {
    let facts = AppFacts {
        app_name,
        toolkit: Some(toolkit).filter(|t| !t.is_empty()),
        web_engine: match engine {
            WebEngine::None => None,
            WebEngine::Gecko => Some("gecko"),
            WebEngine::Blink => Some("blink"),
        },
    };
    let name = selector()
        .and_then(|s| s.select(&facts))
        .unwrap_or(DEFAULT_PROFILE);
    let b = bundle(name).or_else(|| bundle(DEFAULT_PROFILE))?;
    Some(SemTables {
        name: b.name,
        quirks: b.quirks,
        pi: b.pi,
        inexpressible: b.inexpressible,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(t: &str, a: &str, e: WebEngine) -> &'static str {
        tables_for(t, a, e).map_or("none", |s| s.name)
    }

    #[test]
    fn engine_and_toolkit_pick_the_bundle_names_are_the_fallback() {
        assert_eq!(name("Gecko", "firefox", WebEngine::Gecko), "gecko");
        assert_eq!(name("Chromium", "chrome", WebEngine::Blink), "blink");
        assert_eq!(name("GTK", "gnome-calculator", WebEngine::None), "gtk4");
        assert_eq!(name("", "firefox-esr", WebEngine::None), "gecko");
        assert_eq!(name("", "orca", WebEngine::None), "default");
        assert_eq!(name("unknown", "xyz", WebEngine::None), "default");
    }

    #[test]
    fn every_shipped_bundle_is_a_valid_set_of_tables() {
        for (t, a, e) in [
            ("", "x", WebEngine::Gecko),
            ("", "x", WebEngine::Blink),
            ("GTK", "x", WebEngine::None),
            ("", "x", WebEngine::None),
        ] {
            let s = tables_for(t, a, e);
            assert!(
                s.is_some_and(|s| gorca_fuse::QuirkSet::parse(s.quirks).is_ok()
                    && gorca_fuse::parse_pi(s.pi).is_ok()
                    && gorca_fuse::parse_inexpressible(s.inexpressible).is_ok())
            );
        }
    }
}
