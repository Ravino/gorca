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
    }
}

/// An empty list matches anything.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Matcher {
    pub app_name: Vec<String>,
    pub toolkit: Vec<String>,
    pub connector: Vec<String>,
    pub document_role: Vec<String>,
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
        any(&self.app_name, &app.app_name)
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
    /// `(key description, command id)` pairs. TBD: syntax.
    pub keymap: Vec<(String, u32)>,
    pub capabilities: Capabilities,
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
