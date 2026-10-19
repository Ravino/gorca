// SPDX-License-Identifier: GPL-3.0-or-later
//! Triggers, matchers and the facts they match (ADR-0016 section 2.2).
//!
//! Identity facts only: an application's name, executable and desktop id; a window's title and
//! kind; a focus context; a document's host and path. Never toolkit heuristics. The facts are
//! cached by the caller (TR-1); matching here is a few string comparisons and allocates nothing.

use std::fmt::Write as _;

use toml_edit::Item;

use crate::diag::{Diag, DiagMsg};
use crate::glob::{Glob, GlobError};

/// PF-6: globs per matcher (all three lists together).
pub const MAX_GLOBS_PER_MATCHER: usize = 16;

/// Focus contexts a `Context` trigger can name.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum FocusContext {
    Terminal,
    Editable,
    Document,
    BrowseMode,
    FocusMode,
    Menu,
    Table,
    Dialog,
}

/// Number of [`FocusContext`] values (size of the TR-2 context table).
pub const N_CONTEXT: usize = 8;

impl FocusContext {
    pub const ALL: [FocusContext; N_CONTEXT] = [
        FocusContext::Terminal,
        FocusContext::Editable,
        FocusContext::Document,
        FocusContext::BrowseMode,
        FocusContext::FocusMode,
        FocusContext::Menu,
        FocusContext::Table,
        FocusContext::Dialog,
    ];

    pub fn index(self) -> usize {
        match self {
            FocusContext::Terminal => 0,
            FocusContext::Editable => 1,
            FocusContext::Document => 2,
            FocusContext::BrowseMode => 3,
            FocusContext::FocusMode => 4,
            FocusContext::Menu => 5,
            FocusContext::Table => 6,
            FocusContext::Dialog => 7,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            FocusContext::Terminal => "terminal",
            FocusContext::Editable => "editable",
            FocusContext::Document => "document",
            FocusContext::BrowseMode => "browse-mode",
            FocusContext::FocusMode => "focus-mode",
            FocusContext::Menu => "menu",
            FocusContext::Table => "table",
            FocusContext::Dialog => "dialog",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim().to_ascii_lowercase().replace('_', "-");
        FocusContext::ALL.into_iter().find(|c| c.name() == s)
    }

    /// `Context{BrowseMode|FocusMode}` triggers may never set `ext.*` keys (NS-3).
    pub fn is_mode(self) -> bool {
        match self {
            FocusContext::BrowseMode | FocusContext::FocusMode => true,
            FocusContext::Terminal
            | FocusContext::Editable
            | FocusContext::Document
            | FocusContext::Menu
            | FocusContext::Table
            | FocusContext::Dialog => false,
        }
    }
}

/// The contexts that hold at the focus, as a bit set.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct ContextSet(u16);

impl ContextSet {
    pub const EMPTY: ContextSet = ContextSet(0);

    #[must_use]
    pub fn with(self, c: FocusContext) -> Self {
        ContextSet(self.0 | (1 << c.index()))
    }

    pub fn contains(self, c: FocusContext) -> bool {
        self.0 & (1 << c.index()) != 0
    }

    pub fn iter(self) -> impl Iterator<Item = FocusContext> {
        FocusContext::ALL
            .into_iter()
            .filter(move |c| self.contains(*c))
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum WindowKind {
    Normal,
    Dialog,
    Popup,
}

impl WindowKind {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "normal" => Some(WindowKind::Normal),
            "dialog" => Some(WindowKind::Dialog),
            "popup" => Some(WindowKind::Popup),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            WindowKind::Normal => "normal",
            WindowKind::Dialog => "dialog",
            WindowKind::Popup => "popup",
        }
    }
}

/// The trigger kinds, lowest specificity first (PM-1: `Site > Window > App > Context`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum TriggerKind {
    Context,
    App,
    Window,
    Site,
}

impl TriggerKind {
    pub fn name(self) -> &'static str {
        match self {
            TriggerKind::Context => "context",
            TriggerKind::App => "app",
            TriggerKind::Window => "window",
            TriggerKind::Site => "site",
        }
    }
}

/// How specific a match is: kind first, then the literal prefix of the best matching glob.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Specificity {
    pub kind: Option<TriggerKind>,
    pub prefix: u32,
}

/// An application's cached identity (TR-1).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AppFacts<'a> {
    pub name: &'a str,
    /// Executable path; `None` until the async `/proc` read lands (then no `exe` match).
    pub exe: Option<&'a str>,
    pub desktop_id: Option<&'a str>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct WindowFacts<'a> {
    pub title: &'a str,
    pub kind: WindowKind,
}

/// Host and path of the focused document, never the whole URL (PC-5).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SiteFacts<'a> {
    pub host: &'a str,
    pub path: Option<&'a str>,
}

/// Everything a trigger may look at, for one focus.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FocusFacts<'a> {
    pub app: Option<AppFacts<'a>>,
    pub window: Option<WindowFacts<'a>>,
    pub contexts: ContextSet,
    pub site: Option<SiteFacts<'a>>,
}

/// Identity patterns of an application: every non-empty list needs one matching glob.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppMatcher {
    pub name: Vec<Glob>,
    /// Matched against the whole executable path and against its file name.
    pub exe: Vec<Glob>,
    pub desktop_id: Vec<Glob>,
}

impl AppMatcher {
    /// No pattern at all: it names no application.
    pub fn is_empty(&self) -> bool {
        self.name.is_empty() && self.exe.is_empty() && self.desktop_id.is_empty()
    }

    pub fn glob_count(&self) -> usize {
        self.name.len() + self.exe.len() + self.desktop_id.len()
    }

    /// All patterns, whichever list.
    pub fn globs(&self) -> impl Iterator<Item = &Glob> {
        self.name
            .iter()
            .chain(self.exe.iter())
            .chain(self.desktop_id.iter())
    }

    /// `Some(prefix)` (the longest literal prefix among the matching globs) when `app` matches.
    /// An empty matcher matches nothing; absent `exe` or `desktop_id` facts never match.
    pub fn matches(&self, app: &AppFacts<'_>) -> Option<u32> {
        if self.is_empty() {
            return None;
        }
        let mut best = 0u32;
        let mut hit = |globs: &[Glob], text: Option<&str>, also: Option<&str>| -> bool {
            if globs.is_empty() {
                return true;
            }
            let mut found = false;
            for g in globs {
                let m = text.is_some_and(|t| g.matches(t)) || also.is_some_and(|t| g.matches(t));
                if m {
                    found = true;
                    best = best.max(u32::try_from(g.literal_prefix_len()).unwrap_or(u32::MAX));
                }
            }
            found
        };
        let exe_base = app.exe.and_then(|p| p.rsplit('/').next());
        (hit(&self.name, Some(app.name), None)
            && hit(&self.exe, app.exe, exe_base)
            && hit(&self.desktop_id, app.desktop_id, None))
        .then_some(best)
    }
}

/// A condition under which a profile applies.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Trigger {
    App {
        app: AppMatcher,
    },
    /// An empty `app` means any application.
    Window {
        app: AppMatcher,
        title: Glob,
        kind: Option<WindowKind>,
    },
    Context {
        ctx: FocusContext,
    },
    /// Documents only. An empty `app` means any application.
    Site {
        app: AppMatcher,
        host: Glob,
        path: Option<Glob>,
    },
}

impl Trigger {
    pub fn kind(&self) -> TriggerKind {
        match self {
            Trigger::App { .. } => TriggerKind::App,
            Trigger::Window { .. } => TriggerKind::Window,
            Trigger::Context { .. } => TriggerKind::Context,
            Trigger::Site { .. } => TriggerKind::Site,
        }
    }

    /// The application matcher, if the trigger has one.
    pub fn app(&self) -> Option<&AppMatcher> {
        match self {
            Trigger::App { app } | Trigger::Window { app, .. } | Trigger::Site { app, .. } => {
                Some(app)
            }
            Trigger::Context { .. } => None,
        }
    }

    /// Evaluate against `facts`; `Some(specificity)` on a match.
    pub fn matches(&self, facts: &FocusFacts<'_>) -> Option<Specificity> {
        match self {
            Trigger::App { app } => {
                let p = app.matches(facts.app.as_ref()?)?;
                Some(Specificity {
                    kind: Some(TriggerKind::App),
                    prefix: p,
                })
            }
            Trigger::Window { app, title, kind } => {
                let w = facts.window.as_ref()?;
                let ap = if app.is_empty() {
                    0
                } else {
                    app.matches(facts.app.as_ref()?)?
                };
                if kind.is_some_and(|k| k != w.kind) || !title.matches(w.title) {
                    return None;
                }
                Some(Specificity {
                    kind: Some(TriggerKind::Window),
                    prefix: ap + u32::try_from(title.literal_prefix_len()).unwrap_or(0),
                })
            }
            Trigger::Context { ctx } => facts.contexts.contains(*ctx).then_some(Specificity {
                kind: Some(TriggerKind::Context),
                prefix: 0,
            }),
            Trigger::Site { app, host, path } => {
                let s = facts.site.as_ref()?;
                let ap = if app.is_empty() {
                    0
                } else {
                    app.matches(facts.app.as_ref()?)?
                };
                if !host.matches(s.host) {
                    return None;
                }
                let mut p = ap + u32::try_from(host.literal_prefix_len()).unwrap_or(0);
                if let Some(pg) = path {
                    if !pg.matches(s.path.unwrap_or("")) {
                        return None;
                    }
                    p += u32::try_from(pg.literal_prefix_len()).unwrap_or(0);
                }
                Some(Specificity {
                    kind: Some(TriggerKind::Site),
                    prefix: p,
                })
            }
        }
    }

    /// A stable text for duplicate detection (same canonical text = same trigger).
    pub fn canonical(&self) -> String {
        fn list(out: &mut String, tag: &str, g: &[Glob]) {
            if g.is_empty() {
                return;
            }
            let mut v: Vec<String> = g.iter().map(|x| x.as_str().to_ascii_lowercase()).collect();
            v.sort();
            out.push_str(tag);
            out.push('=');
            out.push_str(&v.join("|"));
            out.push(';');
        }
        fn matcher(out: &mut String, m: &AppMatcher) {
            list(out, "name", &m.name);
            list(out, "exe", &m.exe);
            list(out, "desktop", &m.desktop_id);
        }
        let mut s = String::from(self.kind().name());
        s.push(':');
        match self {
            Trigger::App { app } => matcher(&mut s, app),
            Trigger::Window { app, title, kind } => {
                matcher(&mut s, app);
                let _ = write!(s, "title={};", title.as_str().to_ascii_lowercase());
                if let Some(k) = kind {
                    s.push_str(k.name());
                }
            }
            Trigger::Context { ctx } => s.push_str(ctx.name()),
            Trigger::Site { app, host, path } => {
                matcher(&mut s, app);
                let _ = write!(s, "host={};", host.as_str().to_ascii_lowercase());
                if let Some(p) = path {
                    let _ = write!(s, "path={}", p.as_str().to_ascii_lowercase());
                }
            }
        }
        s
    }
}

fn matcher_inline(m: &AppMatcher) -> toml_edit::InlineTable {
    let mut t = toml_edit::InlineTable::new();
    let arr = |g: &[Glob]| -> toml_edit::Array { g.iter().map(Glob::as_str).collect() };
    if !m.name.is_empty() {
        t.insert("name", toml_edit::Value::Array(arr(&m.name)));
    }
    if !m.exe.is_empty() {
        t.insert("exe", toml_edit::Value::Array(arr(&m.exe)));
    }
    if !m.desktop_id.is_empty() {
        t.insert("desktop_id", toml_edit::Value::Array(arr(&m.desktop_id)));
    }
    t
}

impl Trigger {
    /// The trigger as a `[[profile.trigger]]` table (the inverse of the parser).
    pub fn to_table(&self) -> toml_edit::Table {
        use toml_edit::{InlineTable, Value, value};
        let mut t = toml_edit::Table::new();
        match self {
            Trigger::App { app } => {
                t["app"] = value(matcher_inline(app));
            }
            Trigger::Window { app, title, kind } => {
                let mut w = InlineTable::new();
                if !app.is_empty() {
                    w.insert("app", Value::InlineTable(matcher_inline(app)));
                }
                w.insert("title", title.as_str().into());
                if let Some(k) = kind {
                    w.insert("kind", k.name().into());
                }
                t["window"] = value(w);
            }
            Trigger::Context { ctx } => {
                t["context"] = value(ctx.name());
            }
            Trigger::Site { app, host, path } => {
                let mut w = InlineTable::new();
                if !app.is_empty() {
                    w.insert("app", Value::InlineTable(matcher_inline(app)));
                }
                w.insert("host", host.as_str().into());
                if let Some(p) = path {
                    w.insert("path", p.as_str().into());
                }
                t["site"] = value(w);
            }
        }
        t
    }
}

fn span(item: &Item) -> Option<(u32, u32)> {
    item.span().map(|r| {
        (
            u32::try_from(r.start).unwrap_or(u32::MAX),
            u32::try_from(r.end).unwrap_or(u32::MAX),
        )
    })
}

fn bad(path: &str, item: &Item, what: &'static str, why: impl Into<String>, diags: &mut Vec<Diag>) {
    diags.push(Diag::error(
        path,
        span(item),
        DiagMsg::ProfileInvalid {
            what,
            why: why.into(),
        },
    ));
}

fn glob_of(text: &str, path: &str, item: &Item, diags: &mut Vec<Diag>) -> Option<Glob> {
    match Glob::new(text) {
        Ok(g) => Some(g),
        Err(e @ GlobError::TooLong { .. }) => {
            bad(path, item, "pattern", e.to_string(), diags);
            None
        }
    }
}

/// A string or a list of strings, each compiled as a glob.
fn globs_of(item: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Vec<Glob>> {
    let mut out = Vec::new();
    if let Some(s) = item.as_str() {
        out.push(glob_of(s, path, item, diags)?);
    } else if let Some(arr) = item.as_array() {
        for e in arr {
            let Some(s) = e.as_str() else {
                bad(path, item, "pattern list", "entries must be strings", diags);
                return None;
            };
            out.push(glob_of(s, path, item, diags)?);
        }
    } else {
        bad(
            path,
            item,
            "pattern list",
            "expected a string or a list of strings",
            diags,
        );
        return None;
    }
    Some(out)
}

/// `{ name = [..], exe = [..], desktop_id = [..] }`.
pub(crate) fn parse_matcher(item: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<AppMatcher> {
    let Some(t) = item.as_table_like() else {
        bad(path, item, "application matcher", "expected a table", diags);
        return None;
    };
    let mut m = AppMatcher::default();
    for (k, v) in t.iter() {
        let kp = format!("{path}.{k}");
        let slot = match k {
            "name" => &mut m.name,
            "exe" => &mut m.exe,
            "desktop_id" => &mut m.desktop_id,
            _ => {
                bad(
                    &kp,
                    v,
                    "application matcher",
                    "unknown key (name, exe or desktop_id)",
                    diags,
                );
                return None;
            }
        };
        *slot = globs_of(v, &kp, diags)?;
    }
    if m.glob_count() > MAX_GLOBS_PER_MATCHER {
        diags.push(Diag::error(
            path,
            span(item),
            DiagMsg::ProfileLimit {
                what: "patterns in one matcher",
                limit: MAX_GLOBS_PER_MATCHER,
            },
        ));
        return None;
    }
    Some(m)
}

/// One trigger table: exactly one of `app`, `window`, `context`, `site`.
pub(crate) fn parse_trigger(item: &Item, path: &str, diags: &mut Vec<Diag>) -> Option<Trigger> {
    let Some(t) = item.as_table_like() else {
        bad(path, item, "trigger", "expected a table", diags);
        return None;
    };
    if t.len() != 1 {
        bad(
            path,
            item,
            "trigger",
            "exactly one of app, window, context or site",
            diags,
        );
        return None;
    }
    let (k, v) = t.iter().next()?;
    let kp = format!("{path}.{k}");
    match k {
        "app" => {
            let app = parse_matcher(v, &kp, diags)?;
            if app.is_empty() {
                bad(
                    &kp,
                    v,
                    "trigger",
                    "an app trigger needs at least one pattern",
                    diags,
                );
                return None;
            }
            Some(Trigger::App { app })
        }
        "context" => {
            let Some(c) = v.as_str().and_then(FocusContext::parse) else {
                bad(&kp, v, "trigger", "unknown context", diags);
                return None;
            };
            Some(Trigger::Context { ctx: c })
        }
        "window" => {
            let Some(w) = v.as_table_like() else {
                bad(&kp, v, "trigger", "expected a table", diags);
                return None;
            };
            let mut app = AppMatcher::default();
            let mut title = None;
            let mut kind = None;
            for (wk, wv) in w.iter() {
                let wp = format!("{kp}.{wk}");
                match wk {
                    "app" => app = parse_matcher(wv, &wp, diags)?,
                    "title" => title = Some(glob_of(wv.as_str().unwrap_or(""), &wp, wv, diags)?),
                    "kind" => {
                        let Some(kd) = wv.as_str().and_then(WindowKind::parse) else {
                            bad(&wp, wv, "trigger", "kind is normal, dialog or popup", diags);
                            return None;
                        };
                        kind = Some(kd);
                    }
                    _ => {
                        bad(
                            &wp,
                            wv,
                            "trigger",
                            "unknown key (app, title or kind)",
                            diags,
                        );
                        return None;
                    }
                }
            }
            let Some(title) = title else {
                bad(&kp, v, "trigger", "a window trigger needs a title", diags);
                return None;
            };
            Some(Trigger::Window { app, title, kind })
        }
        "site" => {
            let Some(w) = v.as_table_like() else {
                bad(&kp, v, "trigger", "expected a table", diags);
                return None;
            };
            let mut app = AppMatcher::default();
            let mut host = None;
            let mut path_glob = None;
            for (wk, wv) in w.iter() {
                let wp = format!("{kp}.{wk}");
                match wk {
                    "app" => app = parse_matcher(wv, &wp, diags)?,
                    "host" => host = Some(glob_of(wv.as_str().unwrap_or(""), &wp, wv, diags)?),
                    "path" => path_glob = Some(glob_of(wv.as_str().unwrap_or(""), &wp, wv, diags)?),
                    _ => {
                        bad(&wp, wv, "trigger", "unknown key (app, host or path)", diags);
                        return None;
                    }
                }
            }
            let Some(host) = host else {
                bad(&kp, v, "trigger", "a site trigger needs a host", diags);
                return None;
            };
            Some(Trigger::Site {
                app,
                host,
                path: path_glob,
            })
        }
        _ => {
            bad(&kp, v, "trigger", "unknown trigger kind", diags);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use toml_edit::DocumentMut;

    fn trig(text: &str) -> (Option<Trigger>, Vec<Diag>) {
        let doc: DocumentMut = format!("t = {text}").parse().unwrap();
        let mut d = Vec::new();
        let t = parse_trigger(&doc["t"], "t", &mut d);
        (t, d)
    }

    fn facts(name: &str) -> FocusFacts<'_> {
        FocusFacts {
            app: Some(AppFacts {
                name,
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        }
    }

    #[test]
    fn app_trigger_matches_names_case_insensitively() {
        let (t, d) = trig(r#"{ app = { name = ["firefox*", "Chromium"] } }"#);
        assert!(d.is_empty(), "{d:?}");
        let t = t.unwrap();
        assert!(t.matches(&facts("Firefox-ESR")).is_some());
        assert!(t.matches(&facts("chromium")).is_some());
        assert!(t.matches(&facts("signal")).is_none());
        assert!(t.matches(&FocusFacts::default()).is_none());
    }

    #[test]
    fn exe_and_desktop_id_need_their_facts_and_all_lists_must_match() {
        let (t, _) = trig(r#"{ app = { name = ["fire*"], exe = ["firefox"] } }"#);
        let t = t.unwrap();
        let mut f = facts("firefox");
        assert!(t.matches(&f).is_none(), "exe absent: no match");
        f.app = Some(AppFacts {
            name: "firefox",
            exe: Some("/usr/lib/firefox/firefox"),
            desktop_id: None,
        });
        assert!(t.matches(&f).is_some(), "base name of the exe");
        f.app = Some(AppFacts {
            name: "other",
            exe: Some("/usr/lib/firefox/firefox"),
            desktop_id: None,
        });
        assert!(t.matches(&f).is_none(), "name list must match too");
        let (t, _) = trig(r#"{ app = { desktop_id = ["org.gnome.*"] } }"#);
        let t = t.unwrap();
        assert!(t.matches(&facts("x")).is_none());
        let mut f = facts("x");
        f.app = Some(AppFacts {
            name: "x",
            exe: None,
            desktop_id: Some("org.gnome.Terminal.desktop"),
        });
        assert!(t.matches(&f).is_some());
    }

    #[test]
    fn window_site_and_context_triggers() {
        let (w, d) =
            trig(r#"{ window = { app = { name = ["gedit"] }, title = "*.md", kind = "dialog" } }"#);
        assert!(d.is_empty(), "{d:?}");
        let w = w.unwrap();
        let mut f = facts("gedit");
        f.window = Some(WindowFacts {
            title: "a.md",
            kind: WindowKind::Dialog,
        });
        assert!(w.matches(&f).is_some());
        f.window = Some(WindowFacts {
            title: "a.md",
            kind: WindowKind::Normal,
        });
        assert!(w.matches(&f).is_none());
        let (s, d) = trig(
            r#"{ site = { app = { name = ["firefox*"] }, host = "web.telegram.org", path = "/k/*" } }"#,
        );
        assert!(d.is_empty(), "{d:?}");
        let s = s.unwrap();
        let mut f = facts("Firefox");
        f.site = Some(SiteFacts {
            host: "WEB.telegram.org",
            path: Some("/k/#1"),
        });
        let sp = s.matches(&f).unwrap();
        assert_eq!(sp.kind, Some(TriggerKind::Site));
        f.site = Some(SiteFacts {
            host: "web.telegram.org",
            path: Some("/z/"),
        });
        assert!(s.matches(&f).is_none());
        let (c, _) = trig(r#"{ context = "browse-mode" }"#);
        let c = c.unwrap();
        let mut f = FocusFacts::default();
        assert!(c.matches(&f).is_none());
        f.contexts = ContextSet::EMPTY.with(FocusContext::BrowseMode);
        assert!(c.matches(&f).is_some());
    }

    #[test]
    fn malformed_triggers_are_diagnosed() {
        for text in [
            r"{ app = { name = [] } }",
            r#"{ app = { nope = ["x"] } }"#,
            r#"{ context = "moon" }"#,
            r#"{ window = { title = "x", kind = "huge" } }"#,
            r#"{ site = { app = { name = ["x"] } } }"#,
            r#"{ app = { name = ["a"] }, context = "menu" }"#,
            r"{ other = 1 }",
            "3",
        ] {
            let (t, d) = trig(text);
            assert!(t.is_none(), "{text}");
            assert!(!d.is_empty(), "{text}");
        }
    }

    #[test]
    fn matcher_limits() {
        let many: Vec<String> = (0..17).map(|i| format!("\"app{i}\"")).collect();
        let (t, d) = trig(&format!("{{ app = {{ name = [{}] }} }}", many.join(",")));
        assert!(t.is_none());
        assert!(
            d.iter()
                .any(|x| matches!(x.msg, DiagMsg::ProfileLimit { .. }))
        );
        let long = "a".repeat(300);
        let (t, d) = trig(&format!("{{ app = {{ name = [\"{long}\"] }} }}"));
        assert!(t.is_none());
        assert!(!d.is_empty());
    }

    #[test]
    fn specificity_orders_kinds_then_prefix() {
        let a = Specificity {
            kind: Some(TriggerKind::App),
            prefix: 9,
        };
        let w = Specificity {
            kind: Some(TriggerKind::Window),
            prefix: 0,
        };
        assert!(w > a);
        assert!(
            Specificity {
                kind: Some(TriggerKind::App),
                prefix: 5
            } > Specificity {
                kind: Some(TriggerKind::App),
                prefix: 2
            }
        );
        assert!(a > Specificity::default());
    }
}
