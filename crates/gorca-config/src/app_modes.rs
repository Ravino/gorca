// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-application web-mode defaults, as data (owner decision DP-3, 2026-10-19).
//!
//! The default for every web document, Electron included, is NVDA's: browse mode with automatic
//! focus mode on widgets. Some applications are better operated without the virtual buffer (a
//! code editor); that is a *row* in this table, not code in the reader. The table is the lowest
//! layer of what the planned profile stack (ADR-0016, M14a: typed layers with app, window and
//! site triggers) will fold: rows carry the same trigger shape (application name matchers and an
//! optional site host) and the same "most specific row wins, later layer wins" rule, so M14a's
//! user profiles and M18a's add-on manifests can supply further layers without a change of the
//! reader. The sticky focus mode is *not* a default of any row: it is the user's command
//! (`browse.toggle`) and is always available.

use std::sync::OnceLock;

/// What the virtual buffer does for a matching application.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum BrowseDefault {
    /// Browse mode, with automatic focus mode on widgets (NVDA's behaviour).
    Auto,
    /// No virtual buffer: the application is operated in focus mode and the reader follows its
    /// focus. Today the application is simply not treated as a web document host; user and
    /// add-on layers (M14a, M18a) can override the row.
    Off,
}

/// One row: who it applies to and what it sets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AppModeRow {
    /// Stable id, shown in diagnostics ("why is there no browse mode here").
    pub id: String,
    /// Application names (AT-SPI application name), compared case-insensitively and exactly.
    pub apps: Vec<String>,
    /// Document host (site trigger of M14a); `None` matches every document of the application.
    /// Not evaluated until documents carry their URL (`DocMeta.url`, PM-2).
    pub host: Option<String>,
    pub browse: BrowseDefault,
    /// Why, in one line.
    pub why: String,
}

/// The rows the product ships.
pub fn builtin_rows() -> &'static [AppModeRow] {
    static ROWS: OnceLock<Vec<AppModeRow>> = OnceLock::new();
    ROWS.get_or_init(|| {
        vec![AppModeRow {
            id: "vscode".to_owned(),
            apps: [
                "code",
                "visual studio code",
                "code - oss",
                "vscodium",
                "cursor",
            ]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
            host: None,
            browse: BrowseDefault::Off,
            why: "an editor built from text widgets and its own key bindings; NVDA disables the \
                  buffer for it (appModules/code.py)"
                .to_owned(),
        }]
    })
    .as_slice()
}

/// The row that decides for `app` (and `host`, when the document's site is known). `layers` are
/// ordered lowest first; a later layer wins, and inside one layer a row that names the host
/// beats one that does not.
pub fn resolve<'a>(
    layers: &[&'a [AppModeRow]],
    app: &str,
    host: Option<&str>,
) -> Option<&'a AppModeRow> {
    let mut best: Option<(&AppModeRow, (usize, bool))> = None;
    for (i, layer) in layers.iter().enumerate() {
        for row in *layer {
            if !row.apps.iter().any(|a| a.eq_ignore_ascii_case(app)) {
                continue;
            }
            let host_hit = match (&row.host, host) {
                (None, _) => false,
                (Some(h), Some(d)) if h.eq_ignore_ascii_case(d) => true,
                (Some(_), _) => continue,
            };
            let rank = (i, host_hit);
            if best.is_none_or(|(_, r)| rank >= r) {
                best = Some((row, rank));
            }
        }
    }
    best.map(|(r, _)| r)
}

/// The built-in decision for an application: what the buffer does there.
pub fn browse_default(app: &str) -> BrowseDefault {
    resolve(&[builtin_rows()], app, None).map_or(BrowseDefault::Auto, |r| r.browse)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, app: &str, host: Option<&str>, b: BrowseDefault) -> AppModeRow {
        AppModeRow {
            id: id.to_owned(),
            apps: vec![app.to_owned()],
            host: host.map(str::to_owned),
            browse: b,
            why: String::new(),
        }
    }

    #[test]
    fn every_application_is_browse_mode_unless_a_row_says_otherwise() {
        assert_eq!(browse_default("Firefox"), BrowseDefault::Auto);
        assert_eq!(browse_default("Signal"), BrowseDefault::Auto);
        assert_eq!(browse_default("chromium"), BrowseDefault::Auto);
    }

    #[test]
    fn vs_code_has_no_buffer_by_name_in_any_case() {
        assert_eq!(browse_default("Code"), BrowseDefault::Off);
        assert_eq!(browse_default("Visual Studio Code"), BrowseDefault::Off);
    }

    #[test]
    fn a_later_layer_overrides_and_a_host_row_beats_an_app_row() {
        let base = [row("a", "Signal", None, BrowseDefault::Off)];
        let user = [row("u", "signal", None, BrowseDefault::Auto)];
        let site = [
            row("s1", "chromium", None, BrowseDefault::Off),
            row("s2", "chromium", Some("mail.example"), BrowseDefault::Auto),
        ];
        assert_eq!(
            resolve(&[&base, &user], "Signal", None).map(|r| r.id.as_str()),
            Some("u")
        );
        assert_eq!(
            resolve(&[&site], "chromium", Some("mail.example")).map(|r| r.id.as_str()),
            Some("s2")
        );
        assert_eq!(
            resolve(&[&site], "chromium", Some("other.example")).map(|r| r.id.as_str()),
            Some("s1")
        );
    }
}
