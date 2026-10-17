// SPDX-License-Identifier: GPL-3.0-or-later
//! Built-in app profiles: the minimal in-tree stand-in for per-app extension hooks until the
//! WASM extension host is wired (`gorca-ext-api`). The core never branches on an application
//! name anywhere else; it asks [`AppProfile::for_app`], which matches on [`AppInfo`] the same
//! way an extension manifest would.

use gorca_model::{AppInfo, Role, Segment, StateSet, TextUnit};

/// When an application policy is asked.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PolicyMoment {
    /// A node took focus and is about to be presented.
    Focus,
    /// The value of a node that does not hold focus changed (the Shell's OSD level bar).
    Value,
    /// A window was activated and is about to be announced (`Silence` skips the title).
    Activate,
    /// A node became visible (`showing`): does it read like a notice? (`Notice` says yes.)
    Showing,
}

/// What a policy sees of a node: plain data, nothing that lets it call back.
#[derive(Copy, Clone, Debug)]
pub struct PolicyFacts<'a> {
    pub moment: PolicyMoment,
    pub role: Role,
    pub states: StateSet,
    pub name: Option<&'a str>,
    pub description: Option<&'a str>,
    /// `(current, min, max)` of a range value.
    pub range: Option<(f64, f64, f64)>,
}

/// What a policy decides.
#[derive(Clone, Debug, PartialEq)]
pub enum PolicyVerdict {
    /// Present the node as for any application.
    Default,
    Silence,
    /// Replace the value in the core's presentation.
    Value(String),
    /// Say this instead.
    Say(Vec<Segment>),
    /// Read the node as a notice when it appears (title, role word, text).
    Notice,
}

/// The per-application presentation policy of ADR-0008 tier W3 (an extension), as the core sees
/// it: facts in, verdict out. The daemon wires the native extension (`ext-gnome-shell`) behind
/// it today; the WASM host of M5 implements the same trait. It must be quick and must not block:
/// the Reader calls it inline.
pub trait AppPolicy: Send + Sync + std::fmt::Debug {
    fn decide(&self, facts: &PolicyFacts<'_>) -> PolicyVerdict;
}

/// Per-application behaviour switches chosen from [`AppInfo`] when the app appears.
// Independent switches, each chosen from the connector's report, not one state.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AppProfile {
    /// `object:state-changed:selected(1)` on an item is a focus-equivalent move and is spoken.
    ///
    /// GNOME Shell's Alt+Tab switcher moves a `selected` pseudo-class between items; St
    /// implements neither the Selection interface nor key focus on the items, so the state
    /// change is the only signal there is (`refs/gnome-shell/src/st/st-widget.c`
    /// `check_pseudo_class`; Orca does the same in `scripts/apps/gnome-shell/script.py`
    /// `_on_selected_changed`). Toolkits that emit `selected` next to real focus (GTK lists,
    /// tab strips) must not turn on this switch: the focus event already speaks and the
    /// selection is background noise.
    pub speak_selection: bool,
    /// The unit the application counts text offsets in, from the connector's capability
    /// report, never from a name. The native text layer follows scalars and Gecko's UTF-16
    /// units (`TextUnit::window_follows`) and leaves byte offsets to their own layer.
    pub text_unit: TextUnit,
    /// Quirk data from the connector: see `AppInfo::text_insert_slack`.
    pub text_insert_slack: u32,
    /// `state-changed:showing(1)` on a node the mirror does not know may be a notification banner
    /// or an OSD: the Reader reads it (M4b). The Shell creates those actors on demand, and St
    /// sets `showing` when they are mapped; no other signal says a banner appeared. Taken from
    /// the verified identity like `speak_selection`; other applications report notices as
    /// announcements, or as nodes already in the mirror (which are always read).
    pub watch_showing: bool,
    /// The application's presentation goes through the installed [`AppPolicy`]. Taken from the
    /// verified identity, standing in for the extension manifest matcher of M5.
    pub policy: bool,
}

impl AppProfile {
    pub fn for_app(info: &AppInfo) -> Self {
        Self {
            // Only the connector-verified Shell: a self-reported name grants nothing.
            speak_selection: info.verified_shell,
            text_unit: info.text_unit,
            text_insert_slack: info.text_insert_slack,
            watch_showing: info.verified_shell,
            policy: info.verified_shell,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::ConnectorKind;
    use std::sync::Arc;

    fn info(name: &str, verified_shell: bool) -> AppInfo {
        AppInfo {
            toolkit: Arc::from("clutter"),
            app_name: Arc::from(name),
            pid: None,
            connector_kind: ConnectorKind::Atspi,
            verified_shell,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            root: None,
        }
    }

    #[test]
    fn only_the_verified_shell_speaks_selection() {
        assert!(AppProfile::for_app(&info("gnome-shell", true)).speak_selection);
        assert!(!AppProfile::for_app(&info("nautilus", false)).speak_selection);
        assert!(!AppProfile::default().speak_selection);
    }

    #[test]
    fn only_a_unit_the_window_cannot_follow_turns_native_text_off() {
        let mut i = info("anything", false);
        assert!(AppProfile::for_app(&i).text_unit.window_follows());
        i.text_unit = TextUnit::Utf16Bom;
        assert!(AppProfile::for_app(&i).text_unit.window_follows());
        i.text_unit = TextUnit::Utf8;
        assert!(!AppProfile::for_app(&i).text_unit.window_follows());
        assert!(AppProfile::default().text_unit.window_follows());
    }

    #[test]
    fn only_the_verified_shell_watches_showing() {
        assert!(AppProfile::for_app(&info("gnome-shell", true)).watch_showing);
        assert!(!AppProfile::for_app(&info("gnome-shell", false)).watch_showing);
        assert!(!AppProfile::default().watch_showing);
    }

    #[test]
    fn a_spoofed_shell_name_gets_no_profile() {
        assert!(!AppProfile::for_app(&info("gnome-shell", false)).speak_selection);
        assert!(!AppProfile::for_app(&info("GNOME-Shell", false)).speak_selection);
    }
}
