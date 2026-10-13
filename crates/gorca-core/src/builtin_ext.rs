//! Built-in app profiles: the minimal in-tree stand-in for per-app extension hooks until the
//! WASM extension host is wired (`gorca-ext-api`). The core never branches on an application
//! name anywhere else; it asks [`AppProfile::for_app`], which matches on [`AppInfo`] the same
//! way an extension manifest would.

use gorca_model::AppInfo;

/// Per-application behaviour switches chosen from [`AppInfo`] when the app appears.
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
}

impl AppProfile {
    pub fn for_app(info: &AppInfo) -> Self {
        Self {
            // Only the connector-verified Shell: a self-reported name grants nothing.
            speak_selection: info.verified_shell,
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
    fn a_spoofed_shell_name_gets_no_profile() {
        assert!(!AppProfile::for_app(&info("gnome-shell", false)).speak_selection);
        assert!(!AppProfile::for_app(&info("GNOME-Shell", false)).speak_selection);
    }
}
