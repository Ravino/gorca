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

/// Web A as read from the live Firefox ESR tree (2026-10-25): the row is a link, the badge has
/// only hashed CSS-module classes, text sits in run leaves, the preview has a sender and a colon.
const CHAT_A_LIVE: &[N] = &[
    (0, None, "div", "", ""),
    (1, Some(0), "a", "ListItem-button", ""),
    (
        2,
        Some(1),
        "div",
        "Avatar size-large peer-color-0 no-photo",
        "",
    ),
    (3, Some(2), "div", "inner", ""),
    (4, Some(3), "div", "letters", "Q"),
    (5, Some(1), "button", "fullName _8IVW0azL", ""),
    (6, Some(5), "", "", "Q148"),
    (7, Some(1), "div", "time", ""),
    (8, Some(7), "", "", "22:28"),
    (
        9,
        Some(1),
        "div",
        "last-message shared-canvas-container",
        "",
    ),
    (10, Some(9), "div", "sender-name", ""),
    (11, Some(10), "", "", "Алексей"),
    (12, Some(9), "div", "colon", ""),
    (13, Some(12), "", "", ":"),
    (14, Some(9), "div", "last-message-summary", ""),
    (15, Some(14), "", "", "see you soon"),
    (16, Some(1), "div", "NT-X2xr9 yf73Yld8 npoDLolY", ""),
    (17, Some(16), "", "", "529"),
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
fn the_live_web_a_row_with_a_hashed_badge_class_gets_its_unread_count() {
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#728451375", true),
    )
    .expect("scope");
    let tree = feed(&mut b, CHAT_A_LIVE);
    let ops = collect(&mut b, &tree, Duration::from_secs(5));
    assert_eq!(
        names(&ops),
        vec![(
            1,
            "Q148, 529 unread, Алексей: see you soon, 22:28".to_owned()
        )],
        "{ops:?}"
    );
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
fn web_k_name_is_not_doubled_and_web_a_messages_get_the_listitem_role() {
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
                write: gorca_fuse::Write::Val(gorca_fuse::Val::Role(SemRole::ListItem)),
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

/// Probe (ignored): feed a live document dump (`GORCA_LIVE_TREE=<json>`, a list of
/// `{i, p, role, name, cls, tag, nch}` in document order) through the real plugin and print what
/// comes back and every fault. The dump is made with an AT-SPI script (see the diagnosis report).
#[test]
#[ignore = "needs GORCA_LIVE_TREE"]
fn live_tree_probe() {
    let Some(path) = std::env::var_os("GORCA_LIVE_TREE") else {
        return;
    };
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let limit = std::env::var("GORCA_LIVE_LIMIT")
        .ok()
        .and_then(|l| l.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    let list: Vec<_> = v.as_array().unwrap().iter().take(limit).collect();
    let mut nodes: Vec<N> = Vec::new();
    let leak = |s: &str| -> &'static str { Box::leak(s.to_owned().into_boxed_str()) };
    for x in &list {
        let i = x["i"].as_u64().unwrap() as u32;
        let p = x["p"].as_i64().unwrap();
        let name = x["name"].as_str().unwrap_or("");
        nodes.push((
            i,
            (p >= 0).then_some(p as u32),
            leak(x["tag"].as_str().unwrap_or("")),
            leak(x["cls"].as_str().unwrap_or("")),
            leak(name),
        ));
    }
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let tree = feed(&mut b, &nodes);
    let ops = collect(&mut b, &tree, Duration::from_secs(20));
    for (n, s) in names(&ops) {
        println!("name {n}: {s}");
    }
    let mut per: std::collections::BTreeMap<u32, usize> = std::collections::BTreeMap::new();
    for (n, _) in names(&ops) {
        *per.entry(n).or_default() += 1;
    }
    println!("ops per node {per:?}");
    println!("ops {} stats {:?}", ops.len(), b.stats());
}

/// A busy Web A chat list: 90 rows with long Cyrillic previews, derived in background batches.
/// One call must never be asked for more than its grant can pay (Watchdog) nor for more fact
/// text than a call may return (Malformed); every row gets its name, once.
#[test]
fn a_long_busy_chat_list_derives_without_faults_and_without_duplicate_facts() {
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let long: &'static str = Box::leak(
        "Привет, это очень длинное сообщение о ничём. "
            .repeat(14)
            .into_boxed_str(),
    );
    let mut nodes: Vec<N> = vec![(0, None, "div", "", "")];
    let rows_n = 90u32;
    for r in 0..rows_n {
        let base = 1 + r * 8;
        nodes.push((base, Some(0), "a", "ListItem-button", ""));
        nodes.push((base + 1, Some(base), "button", "fullName", ""));
        nodes.push((
            base + 2,
            Some(base + 1),
            "",
            "",
            Box::leak(format!("Чат номер {r}").into_boxed_str()),
        ));
        nodes.push((base + 3, Some(base), "div", "time", ""));
        nodes.push((base + 4, Some(base + 3), "", "", "22:28"));
        nodes.push((base + 5, Some(base), "div", "last-message", ""));
        nodes.push((base + 6, Some(base + 5), "", "", long));
        nodes.push((base + 7, Some(base), "div", "NT-X2xr9 yf73Yld8", ""));
    }
    let tree = feed(&mut b, &nodes);
    let ops = collect(&mut b, &tree, Duration::from_secs(20));
    let got = names(&ops);
    let mut per: HashMap<u32, usize> = HashMap::new();
    for (n, _) in &got {
        *per.entry(*n).or_default() += 1;
    }
    assert_eq!(
        per.len(),
        rows_n as usize,
        "every row is named: {:?}",
        b.stats()
    );
    assert!(per.values().all(|c| *c == 1), "one fact per row: {per:?}");
}

/// The rows the real document source delivered for the live Telegram Web A in Firefox ESR
/// (2026-10-26, text anonymised; made by `live_document.rs`, which reads the real browser): three
/// chat rows (`a.ListItem-button` with role link, a `h3.fullName` button, hashed badge classes,
/// text runs as separate leaves) and two messages. A fixture of the wrong panel cannot pass here:
/// the plugin must name every chat row and give every message the article role.
#[test]
fn the_rows_the_real_source_delivered_for_live_web_a_are_named_and_the_messages_are_list_items() {
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/web/telegram-a-live.json")).unwrap(),
    )
    .unwrap();
    let list = v.as_array().unwrap();
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let arc = |s: &str| (!s.is_empty()).then(|| Arc::<str>::from(s));
    for x in list {
        let i = x["i"].as_u64().unwrap() as u32;
        let p = x["p"].as_i64().unwrap();
        if p >= 0 {
            kids.entry(p as u32).or_default().push(i);
        }
        b.upsert(
            i,
            Row {
                role: x["role"].as_u64().unwrap() as u16,
                name: arc(x["name"].as_str().unwrap()),
                text: arc(x["text"].as_str().unwrap()),
                tag: arc(x["tag"].as_str().unwrap()),
                class: arc(x["class"].as_str().unwrap()),
                ..Row::default()
            },
        );
    }
    let tree = Flat(kids);
    let ops = collect(&mut b, &tree, Duration::from_secs(10));
    let got = names(&ops);
    let chat_rows: Vec<u32> = list
        .iter()
        .filter(|x| {
            x["class"]
                .as_str()
                .unwrap()
                .split_whitespace()
                .any(|t| t == "ListItem-button")
        })
        .map(|x| x["i"].as_u64().unwrap() as u32)
        .collect();
    assert_eq!(chat_rows.len(), 3);
    for r in &chat_rows {
        let n = got
            .iter()
            .find(|(n, _)| n == r)
            .unwrap_or_else(|| panic!("chat row {r} has no name: {ops:?}"));
        assert!(
            n.1.contains(", "),
            "a computed name, not the raw one: {}",
            n.1
        );
    }
    // The first row has the hashed-class badge "9K".
    assert!(
        got.iter()
            .any(|(n, s)| *n == chat_rows[0] && s.contains("9K unread")),
        "{got:?}"
    );
    let articles = ops
        .iter()
        .filter(|o| {
            matches!(
                o,
                FactOp::Patch {
                    write: gorca_fuse::Write::Val(gorca_fuse::Val::Role(SemRole::ListItem)),
                    ..
                }
            )
        })
        .count();
    assert_eq!(articles, 2, "{ops:?}");
}

/// The open chat of Web A as the real document source delivered it (Firefox ESR, 2026-10-26, text
/// anonymised; `live_document.rs` with `GORCA_DUMP_CHAT`): `div.messages-layout` holds the header
/// title (`h3.fullName`, role button) and the `MessageList`, whose direct children are the
/// `sticky-date` separators and the `Message` items. The message list is a list, each message a
/// list item (outgoing ones described), the title a level-1 heading, each date separator a
/// level-2 heading, and a synthetic unread divider is one too.
#[test]
fn the_open_chat_gets_a_message_list_headings_and_described_own_messages() {
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/web/telegram-a-open-chat.json")).unwrap(),
    )
    .unwrap();
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let arc = |s: &str| (!s.is_empty()).then(|| Arc::<str>::from(s));
    let mut class_of: HashMap<u32, String> = HashMap::new();
    let mut list_node = 0;
    let mut area_node = 0;
    for x in v.as_array().unwrap() {
        let i = x["i"].as_u64().unwrap() as u32;
        let p = x["p"].as_i64().unwrap();
        if p >= 0 {
            kids.entry(p as u32).or_default().push(i);
        }
        let class = x["class"].as_str().unwrap();
        if class.split_whitespace().any(|t| t == "MessageList") {
            list_node = i;
            area_node = p as u32;
        }
        class_of.insert(i, class.to_owned());
        b.upsert(
            i,
            Row {
                role: x["role"].as_u64().unwrap() as u16,
                name: arc(x["name"].as_str().unwrap()),
                text: arc(x["text"].as_str().unwrap()),
                tag: arc(x["tag"].as_str().unwrap()),
                class: arc(class),
                ..Row::default()
            },
        );
    }
    // A divider the dump did not have (no unread messages in that chat): a child of the list.
    let divider = 100_000;
    kids.entry(list_node).or_default().push(divider);
    class_of.insert(divider, "unread-divider".to_owned());
    b.upsert(
        divider,
        Row {
            role: SemRole::Generic.code(),
            tag: arc("div"),
            class: arc("unread-divider"),
            ..Row::default()
        },
    );
    let ops = collect(&mut b, &Flat(kids), Duration::from_secs(10));
    let with_class = |c: &str| -> Vec<u32> {
        let mut v: Vec<u32> = class_of
            .iter()
            .filter(|(_, k)| k.split_whitespace().any(|t| t == c))
            .map(|(i, _)| *i)
            .collect();
        v.sort_unstable();
        v
    };
    let patches = |n: u32, pick: fn(&gorca_fuse::Val) -> Option<String>| -> Vec<String> {
        ops.iter()
            .filter_map(|o| match o {
                FactOp::Patch {
                    node,
                    write: gorca_fuse::Write::Val(v),
                    ..
                } if *node == n => pick(v),
                _ => None,
            })
            .collect()
    };
    let role = |v: &gorca_fuse::Val| match v {
        gorca_fuse::Val::Role(r) => Some(format!("{r:?}")),
        _ => None,
    };
    let level = |v: &gorca_fuse::Val| match v {
        gorca_fuse::Val::Level(l) => Some(l.to_string()),
        _ => None,
    };
    let desc = |v: &gorca_fuse::Val| match v {
        gorca_fuse::Val::Description(d) => Some(d.to_string()),
        _ => None,
    };
    let name = |v: &gorca_fuse::Val| match v {
        gorca_fuse::Val::Name(d) => Some(d.to_string()),
        _ => None,
    };
    assert_eq!(patches(list_node, role), vec!["List"], "{ops:?}");
    assert!(
        patches(list_node, name).iter().any(|n| !n.is_empty()),
        "the list is named from the catalog: {ops:?}"
    );
    let messages = with_class("Message");
    assert!(messages.len() >= 5, "{}", messages.len());
    let mut own = 0;
    for m in &messages {
        assert_eq!(patches(*m, role), vec!["ListItem"], "message {m}");
        let is_own = class_of[m].split_whitespace().any(|t| t == "own");
        let d = patches(*m, desc);
        assert_eq!(d.len(), usize::from(is_own), "message {m}: {d:?}");
        own += usize::from(is_own);
    }
    assert!(own >= 1, "the dump has outgoing messages");
    // The title is the only `fullName` outside a chat-list row in this document.
    let header = with_class("fullName");
    assert_eq!(header.len(), 1);
    assert_eq!(patches(header[0], role), vec!["Heading"]);
    assert_eq!(patches(header[0], level), vec!["1"]);
    for d in with_class("sticky-date") {
        assert_eq!(patches(d, role), vec!["Heading"], "date separator {d}");
        assert_eq!(patches(d, level), vec!["2"]);
    }
    // Landmarks, by structure: the chat list, the message area (the list's parent) and the
    // composer, each with a catalog name.
    for (what, n, r) in [
        ("chat list", with_class("chat-list")[0], "List"),
        ("message area", area_node, "Region"),
        ("composer", with_class("Composer")[0], "Region"),
    ] {
        assert_eq!(patches(n, role), vec![r], "{what}");
        assert!(
            patches(n, name).iter().any(|s| !s.is_empty()),
            "{what} is named: {ops:?}"
        );
    }
    assert_eq!(patches(divider, role), vec!["Heading"]);
    assert_eq!(patches(divider, level), vec!["2"]);
}

/// The chat list of Web A as the real source delivered it (2026-10-26, anonymised;
/// `GORCA_DUMP_LIST`): `chat-list` is a list named "Chat list" inside a navigation landmark (its
/// parent), each `div.ListItem.chat-item-clickable` (the wrapper whose siblings are the rows) is a
/// list item, and the link inside keeps its computed name and its role (Enter still opens the chat).
#[test]
fn the_chat_list_is_a_list_of_list_items_inside_the_chats_landmark() {
    let v: serde_json::Value = serde_json::from_slice(
        &std::fs::read(root().join("tests/fixtures/web/telegram-a-chat-list.json")).unwrap(),
    )
    .unwrap();
    let rt = runtime(PluginOptions::default());
    let mut b = open_bridge(
        &rt,
        scope("firefox-esr", "https://web.telegram.org/a/#1", true),
    )
    .expect("scope");
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let arc = |s: &str| (!s.is_empty()).then(|| Arc::<str>::from(s));
    let mut class_of: HashMap<u32, String> = HashMap::new();
    let mut parent_of: HashMap<u32, u32> = HashMap::new();
    for x in v.as_array().unwrap() {
        let i = x["i"].as_u64().unwrap() as u32;
        let p = x["p"].as_i64().unwrap();
        if p >= 0 {
            kids.entry(p as u32).or_default().push(i);
            parent_of.insert(i, p as u32);
        }
        let class = x["class"].as_str().unwrap();
        class_of.insert(i, class.to_owned());
        b.upsert(
            i,
            Row {
                role: x["role"].as_u64().unwrap() as u16,
                name: arc(x["name"].as_str().unwrap()),
                text: arc(x["text"].as_str().unwrap()),
                tag: arc(x["tag"].as_str().unwrap()),
                class: arc(class),
                ..Row::default()
            },
        );
    }
    let ops = collect(&mut b, &Flat(kids), Duration::from_secs(10));
    let with_class = |c: &str| -> Vec<u32> {
        let mut v: Vec<u32> = class_of
            .iter()
            .filter(|(_, k)| k.split_whitespace().any(|t| t == c))
            .map(|(i, _)| *i)
            .collect();
        v.sort_unstable();
        v
    };
    let roles = |n: u32| -> Vec<String> {
        ops.iter()
            .filter_map(|o| match o {
                FactOp::Patch {
                    node,
                    write: gorca_fuse::Write::Val(gorca_fuse::Val::Role(r)),
                    ..
                } if *node == n => Some(format!("{r:?}")),
                _ => None,
            })
            .collect()
    };
    let list = with_class("chat-list");
    assert_eq!(list.len(), 1);
    assert_eq!(roles(list[0]), vec!["List"], "{ops:?}");
    assert_eq!(roles(parent_of[&list[0]]), vec!["Navigation"], "{ops:?}");
    let named = names(&ops);
    assert!(named.iter().any(|(n, s)| *n == list[0] && !s.is_empty()));
    assert!(
        named
            .iter()
            .any(|(n, s)| *n == parent_of[&list[0]] && !s.is_empty())
    );
    // The rows whose wrapper Gecko exposes are list items; a bare link is left alone (a link
    // that is also a list item would stop being operable: activation and focus mode follow the
    // native role).
    let wrappers = with_class("chat-item-clickable");
    assert!(!wrappers.is_empty());
    for r in &wrappers {
        assert_eq!(roles(*r), vec!["ListItem"], "chat row {r}");
    }
    let links = with_class("ListItem-button");
    assert_eq!(links.len(), 3);
    for l in links {
        assert!(
            named.iter().any(|(n, _)| *n == l),
            "link {l} keeps its computed name"
        );
        assert!(roles(l).is_empty(), "link {l} keeps its role and behaviour");
    }
}
