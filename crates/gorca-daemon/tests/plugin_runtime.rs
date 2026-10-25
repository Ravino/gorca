// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin runtime end to end below the browser: the real first-party Telegram plugin
//! (`plugins/telegram-web`, a wasm32-wasip2 component) loaded through the pin list, matched to a
//! verified document scope, driven through the core's `PluginBridge`, and its facts checked.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{FactOp, PluginBridge, PluginService, Row, ScopeSpec, Tree, now_ns};
use gorca_daemon::plugins::{PluginOptions, PluginRuntime, parse_pins};
use gorca_model::sem::{Group, SemRole};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Build the first-party plugins once (the script is incremental).
fn dist() -> PathBuf {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let st = Command::new(root().join("tools/build-plugins.sh"))
            .status()
            .expect("run tools/build-plugins.sh");
        assert!(st.success(), "plugin build failed");
    });
    root().join("plugins/dist")
}

fn pins() -> Vec<gorca_plugin_host::package::Pin> {
    parse_pins(&std::fs::read_to_string(root().join("plugins/pins.txt")).unwrap())
}

fn runtime(extra: PluginOptions) -> Arc<PluginRuntime> {
    let opts = PluginOptions {
        first_party_dir: Some(dist()),
        ..extra
    };
    PluginRuntime::load_with_pins(&opts, &pins())
}

fn copy_dir(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        std::fs::copy(e.path(), to.join(e.file_name())).unwrap();
    }
}

fn scope(app: &str, url: &str, verified: bool) -> ScopeSpec {
    ScopeSpec {
        app: Arc::from(app),
        url: Some(Arc::from(url)),
        engine: 1,
        verified,
        doc: true,
    }
}

struct Flat(HashMap<u32, Vec<u32>>);

impl Tree for Flat {
    fn parent(&self, n: u32) -> Option<u32> {
        self.0.iter().find(|(_, c)| c.contains(&n)).map(|(p, _)| *p)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.get(&n).cloned().unwrap_or_default()
    }
}

/// A node of a fixture: (index, parent, tag, class, text).
type N = (u32, Option<u32>, &'static str, &'static str, &'static str);

fn rows(nodes: &[N]) -> (Vec<(u32, Row)>, Flat) {
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut out = Vec::new();
    for (i, p, tag, class, text) in nodes {
        if let Some(p) = p {
            kids.entry(*p).or_default().push(*i);
        }
        let leaf = !text.is_empty();
        out.push((
            *i,
            Row {
                role: if leaf {
                    SemRole::StaticText.code()
                } else {
                    SemRole::Generic.code()
                },
                name: leaf.then(|| Arc::from(*text)),
                tag: (!tag.is_empty()).then(|| Arc::from(*tag)),
                class: (!class.is_empty()).then(|| Arc::from(*class)),
                ..Row::default()
            },
        ));
    }
    (out, Flat(kids))
}

/// Web A: `div.ListItem-button` with initials, title, time, preview and badge.
const CHAT_A: &[N] = &[
    (0, None, "div", "", ""),
    (1, Some(0), "div", "ListItem-button", ""),
    (2, Some(1), "div", "status", ""),
    (3, Some(2), "div", "Avatar", ""),
    (4, Some(3), "", "", "AL"),
    (5, Some(1), "div", "info", ""),
    (6, Some(5), "div", "title", ""),
    (7, Some(6), "h3", "fullName", ""),
    (8, Some(7), "", "", "Alice"),
    (9, Some(5), "span", "time", ""),
    (10, Some(9), "", "", "15:00"),
    (11, Some(5), "div", "last-message", ""),
    (12, Some(11), "", "", "see you soon"),
    (13, Some(5), "div", "ChatBadge unread", ""),
    (14, Some(13), "", "", "1"),
];

/// Web K: `a.chatlist-chat` whose avatar label doubles the title.
const CHAT_K: &[N] = &[
    (0, None, "ul", "", ""),
    (1, Some(0), "a", "chatlist-chat chatlist-chat-bigger", ""),
    (2, Some(1), "div", "dialog-avatar", ""),
    (3, Some(2), "", "", "Alice"),
    (4, Some(1), "div", "user-caption", ""),
    (5, Some(4), "p", "dialog-title", ""),
    (6, Some(5), "span", "peer-title", ""),
    (7, Some(6), "", "", "Alice"),
    (8, Some(5), "span", "message-time", ""),
    (9, Some(8), "", "", "15:00"),
    (10, Some(4), "span", "user-last-message", ""),
    (11, Some(10), "", "", "see you soon"),
];

/// Web K in Chromium: the inline spans are gone, title, time and preview are bare text leaves.
const CHAT_K_CHROMIUM: &[N] = &[
    (0, None, "ul", "", ""),
    (
        1,
        Some(0),
        "a",
        "chatlist-chat chatlist-chat-bigger row-big",
        "",
    ),
    (2, Some(1), "div", "dialog-avatar", "Alice"),
    (3, Some(1), "", "", "Alice"),
    (4, Some(1), "", "", "15:00"),
    (5, Some(1), "", "", "see you soon"),
    (
        6,
        Some(1),
        "div",
        "dialog-subtitle-badge badge badge-24 unread",
        "",
    ),
    (7, Some(6), "", "", "1"),
];

const MESSAGE_A: &[N] = &[
    (0, None, "div", "", ""),
    (1, Some(0), "div", "Message message-list-item", ""),
    (2, Some(1), "div", "message-content", ""),
    (3, Some(2), "", "", "hello there"),
];

fn feed(b: &mut PluginBridge, nodes: &[N]) -> Flat {
    let (rs, tree) = rows(nodes);
    for (i, r) in rs {
        b.upsert(i, r);
    }
    tree
}

fn names(ops: &[FactOp]) -> Vec<(u32, String)> {
    ops.iter()
        .filter_map(|o| match o {
            FactOp::Patch {
                node,
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                ..
            } => Some((*node, n.to_string())),
            FactOp::Inferred {
                node,
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(n)),
                ..
            } => Some((*node, n.to_string())),
            _ => None,
        })
        .collect()
}

/// Run the bridge until it has operations or `limit` passes.
fn collect(b: &mut PluginBridge, tree: &Flat, limit: Duration) -> Vec<FactOp> {
    let end = Instant::now() + limit;
    let mut ops = Vec::new();
    while Instant::now() < end {
        b.begin_turn();
        b.background(tree);
        ops.extend(b.take_ops());
        if !ops.is_empty() && !b.has_work() {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    ops
}

fn open_bridge(rt: &Arc<PluginRuntime>, s: ScopeSpec) -> Option<PluginBridge> {
    let runner = rt.open(&s, Arc::new(|| {}))?;
    Some(PluginBridge::new(runner, rt.governor(), s))
}

#[test]
fn the_real_plugin_names_a_web_a_chat_row_without_initials_and_says_unread() {
    let rt = runtime(PluginOptions::default());
    assert!(rt.ids().contains(&"gorca.telegram-web".to_owned()));
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_A);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert_eq!(
        names(&ops),
        vec![(1, "Alice, 1 unread, see you soon, 15:00".to_owned())],
        "{ops:?}"
    );
    // First party: an S1 patch, plain (T0).
    assert!(matches!(ops[0], FactOp::Patch { .. }));
}

#[test]
fn web_k_in_chromium_is_named_from_its_bare_text_leaves() {
    let rt = runtime(PluginOptions::default());
    let mut b =
        open_bridge(&rt, scope("chromium", "https://web.telegram.org/k/", true)).expect("scope");
    let tree = feed(&mut b, CHAT_K_CHROMIUM);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert_eq!(
        names(&ops),
        vec![(1, "Alice, 1 unread, see you soon, 15:00".to_owned())],
        "{ops:?}"
    );
}

#[test]
fn web_k_name_is_not_doubled_and_web_a_messages_get_the_article_role() {
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("chromium", "https://web.telegram.org/k/#@x", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_K);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert_eq!(
        names(&ops),
        vec![(1, "Alice, see you soon, 15:00".to_owned())]
    );
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/", true),
    )
    .expect("scope");
    let tree = feed(&mut b, MESSAGE_A);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert!(
        ops.iter().any(|o| matches!(
            o,
            FactOp::Patch {
                node: 1,
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Role(SemRole::Article)),
                ..
            }
        )),
        "{ops:?}"
    );
}

#[test]
fn a_change_of_the_badge_rederives_once_and_an_unrelated_row_does_not() {
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_A);
    let _ = collect(&mut b, &tree, Duration::from_secs(5));
    let before = b.stats().background_jobs;
    // The badge text changes: the plugin read it, so one more derivation.
    b.begin_turn();
    b.upsert(
        14,
        Row {
            role: SemRole::StaticText.code(),
            name: Some(Arc::from("2")),
            ..Row::default()
        },
    );
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert!(
        names(&ops)
            .iter()
            .any(|(_, n)| n == "Alice, 2 unread, see you soon, 15:00"),
        "{ops:?}"
    );
    assert_eq!(b.stats().background_jobs, before + 1);
    // Nothing changed: no job.
    let jobs = b.stats().background_jobs;
    let _ = collect(&mut b, &tree, Duration::from_millis(30));
    assert_eq!(b.stats().background_jobs, jobs);
}

#[test]
fn an_unverified_identity_or_another_site_gets_no_plugin() {
    let rt = runtime(PluginOptions::default());
    // A self-reported name is not evidence: the URL-scoped first-party plugin is refused.
    assert!(
        open_bridge(
            &rt,
            scope("firefox-esr", "https://web.telegram.org/a/", false)
        )
        .is_none()
    );
    // The evil-suffix host and another site.
    assert!(
        open_bridge(
            &rt,
            scope("firefox-esr", "https://web.telegram.org.evil.com/a/", true)
        )
        .is_none()
    );
    assert!(open_bridge(&rt, scope("firefox-esr", "https://example.org/a/", true)).is_none());
    // An application that is not a listed browser.
    assert!(open_bridge(&rt, scope("gedit", "https://web.telegram.org/a/", true)).is_none());
    // A flatpak sandbox is named by its id and never verified.
    assert!(
        open_bridge(
            &rt,
            scope(
                "flatpak:org.mozilla.firefox",
                "https://web.telegram.org/a/",
                false
            )
        )
        .is_none()
    );
}

#[test]
fn a_tampered_first_party_package_is_refused_and_a_sideloaded_copy_is_hedged() {
    // Tamper: change one byte of the component in a copy of the dist directory.
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("gorca.telegram-web");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::copy(
        dist().join("gorca.telegram-web/manifest.toml"),
        pkg.join("manifest.toml"),
    )
    .unwrap();
    let mut wasm = std::fs::read(dist().join("gorca.telegram-web/plugin.wasm")).unwrap();
    let last = wasm.len() - 1;
    wasm[last] ^= 1;
    std::fs::write(pkg.join("plugin.wasm"), &wasm).unwrap();
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(tmp.path().to_path_buf()),
            ..PluginOptions::default()
        },
        &pins(),
    );
    assert!(
        rt.ids().is_empty(),
        "a hash mismatch is refused, never demoted"
    );

    // The same component sideloaded under another id is a third party: its patch becomes S4.
    let user = tempfile::tempdir().unwrap();
    let pkg = user.path().join("example.telegram");
    std::fs::create_dir_all(&pkg).unwrap();
    let m = std::fs::read_to_string(dist().join("gorca.telegram-web/manifest.toml"))
        .unwrap()
        .replace("gorca.telegram-web", "example.telegram");
    std::fs::write(pkg.join("manifest.toml"), m).unwrap();
    std::fs::copy(
        dist().join("gorca.telegram-web/plugin.wasm"),
        pkg.join("plugin.wasm"),
    )
    .unwrap();
    copy_dir(&dist().join("gorca.telegram-web/i18n"), &pkg.join("i18n"));
    // By default a sideloaded package is refused (no system-scopes yet): fail closed.
    let refused = PluginRuntime::load_with_pins(
        &PluginOptions {
            user_dir: Some(user.path().to_path_buf()),
            ..PluginOptions::default()
        },
        &pins(),
    );
    assert!(refused.ids().is_empty(), "third-party code must not load");
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            user_dir: Some(user.path().to_path_buf()),
            allow_sideloaded: true,
            ..PluginOptions::default()
        },
        &pins(),
    );
    assert_eq!(rt.ids(), vec!["example.telegram".to_owned()]);
    // Verified identity, but third party and URL scoped.
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_A);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert!(
        ops.iter().all(|o| !matches!(o, FactOp::Patch { .. })),
        "a third party never gets S1: {ops:?}"
    );
    assert!(
        ops.iter().any(|o| matches!(o, FactOp::Inferred { .. })),
        "{ops:?}"
    );
    // And an unverified identity gets no third party at all.
    assert!(
        open_bridge(
            &rt,
            scope("firefox-esr", "https://web.telegram.org/a/", false)
        )
        .is_none()
    );
}

#[test]
fn the_file_url_knob_adds_a_pattern_for_private_session_tests_only() {
    let rt = runtime(PluginOptions {
        url_extra: vec![(
            "gorca.telegram-web".to_owned(),
            "file:///x/tests/web/telegram/*".to_owned(),
        )],
        ..PluginOptions::default()
    });
    assert!(
        open_bridge(
            &rt,
            scope("firefox-esr", "file:///x/tests/web/telegram/a.html", true)
        )
        .is_some()
    );
    // The manifest alone never accepts a file URL.
    let rt = runtime(PluginOptions::default());
    assert!(
        open_bridge(
            &rt,
            scope("firefox-esr", "file:///x/tests/web/telegram/a.html", true)
        )
        .is_none()
    );
}

#[test]
fn a_focus_on_an_underived_row_waits_at_most_the_deadline_and_the_facts_follow() {
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_A);
    // Focus lands on the row before any background derivation ran.
    b.begin_turn();
    let t0 = now_ns();
    b.interactive(&[1, 0], &tree, t0);
    let took = Duration::from_nanos(now_ns() - t0);
    // The bounded wait: D = 0.3 ms plus the snapshot and send; generous slack for a loaded host.
    assert!(
        took < Duration::from_millis(5),
        "interactive turn took {took:?}"
    );
    let mut ops = b.take_ops();
    // Late facts arrive as refinements of later turns.
    if ops.is_empty() {
        ops = collect(&mut b, &tree, Duration::from_secs(5));
    }
    assert_eq!(names(&ops).len(), 1, "{ops:?}");
    let _ = Group::Name;
}

#[test]
fn a_key_down_arms_every_open_scope_and_a_closed_one_is_forgotten() {
    let rt = runtime(PluginOptions::default());
    assert!(rt.prewake().is_empty());
    let b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/", true),
    )
    .expect("scope");
    assert_eq!(rt.prewake().len(), 1);
    rt.prewake().key_down();
    drop(b);
    std::thread::sleep(Duration::from_millis(30));
    // The next key (past the rate limit) prunes the handle of the scope that is gone.
    rt.prewake().key_down();
    assert!(rt.prewake().is_empty());
}

#[test]
fn a_sideloaded_package_is_not_loaded_by_default() {
    let user = tempfile::tempdir().unwrap();
    let pkg = user.path().join("example.third");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(
        pkg.join("manifest.toml"),
        "[plugin]\nid = \"example.third\"\n[match]\napps = [\"firefox-esr\"]\n",
    )
    .unwrap();
    std::fs::write(pkg.join("plugin.wasm"), b"\0asm").unwrap();
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(dist()),
            user_dir: Some(user.path().to_path_buf()),
            ..PluginOptions::default()
        },
        &pins(),
    );
    // First party keeps working; the user directory contributes nothing.
    assert_eq!(rt.ids(), vec!["gorca.telegram-web".to_owned()]);
}
