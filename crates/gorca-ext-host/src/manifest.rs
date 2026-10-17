use bitflags::bitflags;
use gorca_model::{AppInfo, ConnectorKind};

use crate::glob::glob_match;

bitflags! {
    /// Host capabilities an extension declares. Anything undeclared is denied.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct Capabilities: u8 {
        const SPEAK            = 1 << 0;
        const FETCH_PREFETCH   = 1 << 1;
        const FETCH_BACKGROUND = 1 << 2;
        const SETTINGS         = 1 << 3;
    }
}

bitflags! {
    /// Hooks an extension exports.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct HookSet: u8 {
        const ON_EVENT  = 1 << 0;
        const PRESENT   = 1 << 1;
        const NAV_UNITS = 1 << 2;
        const COMMAND   = 1 << 3;
        const POLICY    = 1 << 4;
        const TERMINAL  = 1 << 5;
    }
}

/// An empty list matches anything.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Matcher {
    pub app_name: Vec<String>,
    pub toolkit: Vec<String>,
    pub connector: Vec<String>,
    pub document_role: Vec<String>,
    /// Only an application the connector verified as the protected Shell (`AppInfo::verified_shell`).
    /// A self-reported name proves nothing; an extension for the Shell asks for this, not for a
    /// name (the core's own `AppProfile` does the same).
    pub verified_shell: bool,
}

fn any(globs: &[String], text: &str) -> bool {
    globs.is_empty() || globs.iter().any(|g| glob_match(g, text))
}

fn connector_name(k: ConnectorKind) -> &'static str {
    match k {
        ConnectorKind::Atspi => "atspi",
        ConnectorKind::Newton => "newton",
        ConnectorKind::Fake => "fake",
    }
}

impl Matcher {
    pub fn matches(&self, app: &AppInfo, document_role: Option<&str>) -> bool {
        (!self.verified_shell || app.verified_shell)
            && any(&self.app_name, &app.app_name)
            && any(&self.toolkit, &app.toolkit)
            && any(&self.connector, connector_name(app.connector_kind))
            && (self.document_role.is_empty()
                || document_role.is_some_and(|r| any(&self.document_role, r)))
    }
}

/// Order when several match: app-specific, then toolkit-specific, then connector-specific.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum Specificity {
    App,
    Toolkit,
    Connector,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Manifest {
    pub name: String,
    pub matcher: Matcher,
    pub hooks: HookSet,
    /// Commands the extension declares, with their default chords.
    pub keymap: Vec<KeymapEntry>,
    pub capabilities: Capabilities,
    /// How the extension runs. `Native` is for in-tree first-party extensions only and loses hang
    /// protection (ADR-0008 3.3); the manifest must say so.
    pub backend: ExtBackend,
    /// The component file, relative to the manifest (`Wasm` only).
    pub module: Option<String>,
}

/// How an extension executes (ADR-0008 section 3.3).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ExtBackend {
    /// A `wasm32-wasip2` component under wasmtime: fuel/epoch limit, 32 MiB, trap becomes reset.
    #[default]
    Wasm,
    /// Linked in-process: `catch_unwind` and a post-hoc deadline check; cannot pre-empt a hang.
    Native,
}

/// One command of an extension, as the manifest declares it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct KeymapEntry {
    /// Short name; the registry name is `ext.<extension>.<command>`.
    pub command: String,
    /// The id the extension's `command` hook receives.
    pub local_id: u32,
    pub help: String,
    /// Where the command may be bound; the default chord goes to the first one.
    pub contexts: Vec<gorca_config::ContextName>,
    /// Default chord (`gorca+shift+f9`), bound unless the preset or the user mentions it.
    pub chord: Option<String>,
    pub max_press: u8,
    pub interrupts: bool,
}

impl Manifest {
    pub fn specificity(&self) -> Specificity {
        if !self.matcher.app_name.is_empty() {
            Specificity::App
        } else if !self.matcher.toolkit.is_empty() {
            Specificity::Toolkit
        } else {
            Specificity::Connector
        }
    }
}

/// The manifests that match `app`, most specific first; ties keep input order.
pub fn order_matching<'a>(
    manifests: &'a [Manifest],
    app: &AppInfo,
    document_role: Option<&str>,
) -> Vec<&'a Manifest> {
    let mut m: Vec<&Manifest> = manifests
        .iter()
        .filter(|m| m.matcher.matches(app, document_role))
        .collect();
    m.sort_by_key(|m| m.specificity());
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn app(name: &str, toolkit: &str) -> AppInfo {
        AppInfo {
            toolkit: Arc::from(toolkit),
            app_name: Arc::from(name),
            pid: None,
            connector_kind: ConnectorKind::Atspi,
            verified_shell: false,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            page_events_settle_ms: 0,
            root: None,
        }
    }

    fn manifest(name: &str, m: Matcher) -> Manifest {
        Manifest {
            name: name.to_owned(),
            matcher: m,
            hooks: HookSet::PRESENT,
            keymap: vec![],
            capabilities: Capabilities::empty(),
            backend: ExtBackend::Wasm,
            module: None,
        }
    }

    #[test]
    fn empty_matcher_matches_everything_but_document_role_needs_a_role() {
        assert!(Matcher::default().matches(&app("x", "gtk"), None));
        let m = Matcher {
            document_role: vec!["web*".into()],
            ..Matcher::default()
        };
        assert!(!m.matches(&app("x", "gtk"), None));
        assert!(m.matches(&app("x", "gtk"), Some("web-document")));
    }

    #[test]
    fn verified_shell_matches_only_the_verified_shell_never_a_name() {
        let m = Matcher {
            verified_shell: true,
            ..Matcher::default()
        };
        let mut shell = app("gnome-shell", "clutter");
        assert!(
            !m.matches(&shell, None),
            "a self-reported name is not enough"
        );
        shell.verified_shell = true;
        assert!(m.matches(&shell, None));
        assert!(!m.matches(&app("anything", "gtk"), None));
    }

    #[test]
    fn order_is_app_then_toolkit_then_connector() {
        let ms = vec![
            manifest(
                "by-connector",
                Matcher {
                    connector: vec!["atspi".into()],
                    ..Matcher::default()
                },
            ),
            manifest(
                "by-toolkit",
                Matcher {
                    toolkit: vec!["gtk*".into()],
                    ..Matcher::default()
                },
            ),
            manifest(
                "by-app",
                Matcher {
                    app_name: vec!["gnome-terminal*".into()],
                    ..Matcher::default()
                },
            ),
            manifest(
                "other-app",
                Matcher {
                    app_name: vec!["firefox".into()],
                    ..Matcher::default()
                },
            ),
        ];
        let got = order_matching(&ms, &app("gnome-terminal-server", "gtk4"), None);
        let names: Vec<_> = got.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["by-app", "by-toolkit", "by-connector"]);
    }
}
