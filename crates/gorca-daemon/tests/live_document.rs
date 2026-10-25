// SPDX-License-Identifier: GPL-3.0-or-later
//! Probe (ignored, needs a live session and `GORCA_LIVE_DOC=1`): the real document source reads the
//! showing Firefox document over a peer-to-peer connection (read-only: GetChildren, GetRole,
//! GetState, GetTree and friends, no events, no writes), its deltas become plugin rows the way the
//! document actor builds them, and the real first-party plugin runs over them. Prints what the
//! plugin sees of the first chat rows and what it returns.
#![allow(
    clippy::pedantic,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_core::plugin_port::{FactOp, PluginBridge, PluginService, Row, ScopeSpec, Tree};
use gorca_daemon::plugins::{PluginOptions, PluginRuntime, parse_pins};
use gorca_dbus::call_method;
use gorca_doc_source_api::{DocSource, DocSourceCtx, delta_channel, join_channel};
use gorca_doc_source_atspi::roles::{raw, state_bit};
use gorca_doc_source_atspi::source::SourceConfig;
use gorca_doc_source_atspi::{AppConn, AtspiDocSource, ConnectOpts, WebQuirks, wire};
use gorca_model::sem::SemRole;
use gorca_model::{DocDelta, DocId, SrcNodeId};
use gorca_rt::{CancellationToken, Clock, Deadline, mpsc};

const ACC: &str = "org.a11y.atspi.Accessible";

fn dl(ms: u64) -> Deadline {
    Deadline(Instant::now() + Duration::from_millis(ms))
}

async fn a11y_bus() -> zbus::Connection {
    let session = zbus::Connection::session().await.unwrap();
    let m = session
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .await
        .unwrap();
    let addr: String = m.body().deserialize().unwrap();
    zbus::connection::Builder::address(addr.as_str())
        .unwrap()
        .build()
        .await
        .unwrap()
}

async fn name_of(bus: &zbus::Connection, dest: &str, path: &str) -> String {
    let m = call_method(
        bus,
        dl(2000),
        dest,
        path,
        "org.freedesktop.DBus.Properties",
        "Get",
        &(ACC, "Name"),
    )
    .await;
    m.ok()
        .and_then(|m| {
            wire::parse_variant_string(m.body().data().bytes())
                .ok()
                .map(str::to_owned)
        })
        .unwrap_or_default()
}

async fn find_showing_doc(conn: &AppConn, root: &str) -> Option<String> {
    let mut queue = std::collections::VecDeque::from([(root.to_owned(), 0u32)]);
    let mut seen = 0;
    while let Some((p, depth)) = queue.pop_front() {
        seen += 1;
        if seen > 4000 {
            break;
        }
        let role = conn
            .call(conn.next_lane(), dl(2000), &p, ACC, "GetRole", &())
            .await
            .ok()
            .and_then(|m| wire::parse_u32(m.body().data().bytes()).ok());
        let Some(role) = role else { continue };
        if role == raw::DOCUMENT_WEB {
            let st = conn
                .call(conn.next_lane(), dl(2000), &p, ACC, "GetState", &())
                .await
                .ok()
                .and_then(|m| wire::parse_state(m.body().data().bytes()).ok())
                .unwrap_or(0);
            if st & (1u64 << state_bit::SHOWING) != 0 {
                return Some(p);
            }
            continue;
        }
        if depth < 8
            && let Ok(m) = conn
                .call(conn.next_lane(), dl(2000), &p, ACC, "GetChildren", &())
                .await
            && let Ok(kids) = wire::parse_children(m.body().data().bytes())
        {
            for (_, c) in kids {
                queue.push_back((c, depth + 1));
            }
        }
    }
    None
}

struct Flat(HashMap<u32, Vec<u32>>, HashMap<u32, u32>);

impl Tree for Flat {
    fn parent(&self, n: u32) -> Option<u32> {
        self.1.get(&n).copied()
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.get(&n).cloned().unwrap_or_default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs a live session"]
async fn live_firefox_document_through_the_real_source_and_plugin() {
    if std::env::var_os("GORCA_LIVE_DOC").is_none() {
        return;
    }
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_test_writer()
        .try_init();
    let bus = a11y_bus().await;
    let m = call_method(
        &bus,
        dl(3000),
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        ACC,
        "GetChildren",
        &(),
    )
    .await
    .unwrap();
    let apps = wire::parse_children(m.body().data().bytes()).unwrap();
    let mut ff = None;
    for (dest, path) in apps {
        if name_of(&bus, &dest, &path).await == "Firefox" {
            ff = Some((dest, path));
        }
    }
    let (dest, root) = ff.expect("a Firefox application");
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
    let opts = ConnectOpts::new(runtime.unwrap_or_default());
    let conn = Arc::new(AppConn::connect(&bus, &dest, &opts).await.unwrap());
    let doc = find_showing_doc(&conn, &root)
        .await
        .expect("a showing document");
    println!("document {doc}");

    let mut config = SourceConfig::browse(gorca_model::TextUnit::Utf16Bom, false);
    config.fetch.quirks = WebQuirks::GECKO;
    let (_ev_tx, ev_rx) = mpsc::channel(8);
    let (source, _metrics) = AtspiDocSource::new(Arc::clone(&conn), doc, config, ev_rx);
    let (sink, mut rx) = delta_channel(1 << 16);
    let (_req_tx, req_rx) = mpsc::channel(8);
    let (join, _jrx) = join_channel(16);
    let (_core_tx, core_events) = mpsc::channel(8);
    let shutdown = CancellationToken::new();
    let ctx = DocSourceCtx {
        doc: DocId(1),
        app: gorca_model::AppId {
            connector: gorca_model::ConnectorId(0),
            serial: 0,
        },
        sink,
        requests: req_rx,
        events: core_events,
        join,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let task = tokio::spawn(Box::new(source).attach(ctx));

    // Collect deltas until quiet.
    let mut batches: Vec<DocDelta> = Vec::new();
    let mut last = Instant::now();
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        match tokio::time::timeout(Duration::from_millis(500), rx.deltas.recv()).await {
            Ok(Some(d)) => {
                batches.push(d);
                last = Instant::now();
            }
            Ok(None) => break,
            Err(_) => {
                if !batches.is_empty() && last.elapsed() > Duration::from_secs(2) {
                    break;
                }
            }
        }
    }
    shutdown.cancel();
    let _ = task.await;
    println!("{} deltas", batches.len());

    // Segments are the doc actor's batches (up to Settled). The scope opens after the first one
    // (the URL hint is a batch of its own) and replays what is there, like `open_plugins`.
    let root_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let pins = parse_pins(&std::fs::read_to_string(root_dir.join("plugins/pins.txt")).unwrap());
    let rt = PluginRuntime::load_with_pins(
        &PluginOptions {
            first_party_dir: Some(
                std::env::var_os("GORCA_DIST")
                    .map_or_else(|| root_dir.join("plugins/dist"), PathBuf::from),
            ),
            scheduler: match std::env::var("GORCA_SCHED").as_deref() {
                Ok("sps") => gorca_daemon::plugins::SchedulerPolicy::Sps,
                _ => gorca_daemon::plugins::SchedulerPolicy::Labels,
            },
            ..PluginOptions::default()
        },
        &pins,
    );
    let scope = ScopeSpec {
        app: Arc::from("firefox-esr"),
        url: Some(Arc::from("https://web.telegram.org/a/")),
        engine: 1,
        verified: true,
        doc: true,
    };
    let runner = rt.open(&scope, Arc::new(|| {})).expect("scope");
    let mut b = PluginBridge::new(runner, rt.governor(), scope);

    let mut index: HashMap<SrcNodeId, u32> = HashMap::new();
    let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut parent: HashMap<u32, u32> = HashMap::new();
    let mut all_rows: HashMap<u32, Row> = HashMap::new();
    let mut ops: Vec<FactOp> = Vec::new();
    let mut segment = 0;
    let mut pending_rows: Vec<(u32, Row)> = Vec::new();
    let mut open = false;
    let alloc = |id: SrcNodeId, index: &mut HashMap<SrcNodeId, u32>| -> u32 {
        let n = u32::try_from(index.len()).unwrap();
        *index.entry(id).or_insert(n)
    };
    for d in &batches {
        match d {
            DocDelta::Upsert(nodes) => {
                for n in nodes {
                    let ix = alloc(n.id, &mut index);
                    let row = match &n.rec {
                        Some(rec) => Row {
                            role: rec.sem.role.code(),
                            states: 0,
                            name: rec.name.clone(),
                            text: n.text.clone(),
                            tag: (!rec.tag.is_empty()).then(|| Arc::from(rec.tag.as_str())),
                            class: rec.class.clone(),
                            aria_role: rec.aria_role.clone(),
                            dom_id: rec.dom_id.clone(),
                            secret: false,
                        },
                        None => Row {
                            role: SemRole::StaticText.code(),
                            name: n.text.clone(),
                            text: n.text.clone(),
                            ..Row::default()
                        },
                    };
                    all_rows.insert(ix, row.clone());
                    if open {
                        b.upsert(ix, row);
                    } else {
                        pending_rows.push((ix, row));
                    }
                }
            }
            DocDelta::Children {
                parent: p,
                children,
            } => {
                let pi = alloc(*p, &mut index);
                let list: Vec<u32> = children.iter().map(|c| alloc(*c, &mut index)).collect();
                for c in &list {
                    parent.insert(*c, pi);
                }
                if kids.insert(pi, list).is_some() && open {
                    b.touch(pi);
                }
            }
            DocDelta::Settled { .. } => {
                segment += 1;
                if !open {
                    open = true;
                    for (i, r) in std::mem::take(&mut pending_rows) {
                        b.upsert(i, r);
                    }
                }
                let tree = Flat(kids.clone(), parent.clone());
                b.begin_turn();
                b.background(&tree);
                ops.extend(b.take_ops());
            }
            _ => {}
        }
    }
    println!("{} segments, {} nodes", segment, all_rows.len());
    let tree = Flat(kids.clone(), parent.clone());
    let end = Instant::now() + Duration::from_secs(20);
    while Instant::now() < end && b.has_work() {
        b.begin_turn();
        b.background(&tree);
        ops.extend(b.take_ops());
        std::thread::sleep(Duration::from_millis(2));
    }
    let has = |r: &Row, c: &str| {
        r.class
            .as_deref()
            .is_some_and(|k| k.split_whitespace().any(|t| t == c))
    };
    let chat_rows = all_rows
        .values()
        .filter(|r| has(r, "ListItem-button"))
        .count();
    let names: usize = ops
        .iter()
        .filter(|o| {
            matches!(
                o,
                FactOp::Patch {
                    write: gorca_fuse::Write::Val(gorca_fuse::Val::Name(_)),
                    ..
                }
            )
        })
        .count();
    // Every title (`fullName`) outside a chat-list row, with its ancestors: the chat header.
    for (i, r) in &all_rows {
        if !has(r, "fullName") {
            continue;
        }
        let mut chain: Vec<String> = Vec::new();
        let mut cur = *i;
        let mut in_row = false;
        while let Some(p) = parent.get(&cur) {
            let pr = &all_rows[p];
            in_row |= has(pr, "ListItem-button");
            chain.push(format!(
                "{}.{}",
                pr.tag.as_deref().unwrap_or(""),
                pr.class
                    .as_deref()
                    .unwrap_or("")
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
            ));
            cur = *p;
        }
        if !in_row {
            println!(
                "TITLE node {i} role {} tag {:?} class {:?} name {:?} chain {}",
                r.role,
                r.tag.as_deref(),
                r.class.as_deref(),
                r.name
                    .as_deref()
                    .map(|n| n.chars().take(20).collect::<String>()),
                chain.join(" < ")
            );
        }
    }
    if let Some(out) = std::env::var_os("GORCA_DUMP_CHAT") {
        // The open chat: the `messages-layout` node, the header title, the message list and its
        // first messages (each with its subtree), anonymised like the chat-list fixture.
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut alias = |s: &str| -> String {
            let keep = s.trim().is_empty()
                || s.trim().chars().all(|c| {
                    c.is_ascii_digit() || matches!(c, ':' | '.' | ',' | ' ' | 'K' | 'M' | '?')
                });
            if keep {
                return s.to_owned();
            }
            let n = aliases.len();
            aliases
                .entry(s.to_owned())
                .or_insert_with(|| format!("Слово{n}"))
                .clone()
        };
        let layout = all_rows
            .iter()
            .filter(|(_, r)| has(r, "messages-layout"))
            .map(|(i, _)| *i)
            .min()
            .expect("a messages-layout node (open a chat first)");
        let mut order: Vec<u32> = vec![layout];
        let subtree = |root: u32, cap: usize, order: &mut Vec<u32>| {
            let mut q = std::collections::VecDeque::from([root]);
            let mut n = 0;
            while let Some(x) = q.pop_front() {
                if n >= cap {
                    break;
                }
                n += 1;
                if !order.contains(&x) {
                    order.push(x);
                }
                q.extend(kids.get(&x).into_iter().flatten().copied());
            }
        };
        let title = all_rows
            .iter()
            .filter(|(i, r)| {
                has(r, "fullName") && {
                    let mut c = **i;
                    let mut in_row = false;
                    while let Some(p) = parent.get(&c) {
                        in_row |= has(&all_rows[p], "ListItem-button");
                        c = *p;
                    }
                    !in_row
                }
            })
            .map(|(i, _)| *i)
            .min()
            .expect("a header title");
        subtree(title, 8, &mut order);
        let list = all_rows
            .iter()
            .filter(|(_, r)| has(r, "MessageList"))
            .map(|(i, _)| *i)
            .min()
            .expect("a MessageList");
        let mut chain = vec![list];
        while let Some(p) = parent.get(chain.last().unwrap()) {
            if *p == layout {
                break;
            }
            chain.push(*p);
        }
        for c in chain.iter().rev() {
            order.push(*c);
        }
        for k in kids.get(&list).into_iter().flatten().take(14) {
            subtree(*k, 30, &mut order);
        }
        let pos: HashMap<u32, usize> = order.iter().enumerate().map(|(k, i)| (*i, k + 1)).collect();
        let mut json = vec![serde_json::json!({
            "i": 0, "p": -1, "role": SemRole::Generic.code(), "tag": "div", "class": "", "name": "", "text": ""
        })];
        for i in &order {
            let r = &all_rows[i];
            let p = if *i == layout {
                0
            } else {
                parent
                    .get(i)
                    .and_then(|p| pos.get(p))
                    .map_or(0, |k| i64::try_from(*k).unwrap())
            };
            json.push(serde_json::json!({
                "i": pos[i], "p": p, "role": r.role,
                "tag": r.tag.as_deref().unwrap_or(""),
                "class": r.class.as_deref().unwrap_or(""),
                "name": r.name.as_deref().map(&mut alias).unwrap_or_default(),
                "text": r.text.as_deref().map(&mut alias).unwrap_or_default(),
            }));
        }
        std::fs::write(&out, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        println!("chat fixture written: {} nodes", json.len());
    }
    if let Some(out) = std::env::var_os("GORCA_DUMP_FIXTURE") {
        // A small, anonymised slice of what the real source delivered: three chat rows and two
        // messages with their subtrees. Texts become aliases; times, dates and counts are kept.
        let mut aliases: HashMap<String, String> = HashMap::new();
        let mut alias = |s: &str| -> String {
            let keep = s.trim().is_empty()
                || s.trim().chars().all(|c| {
                    c.is_ascii_digit() || matches!(c, ':' | '.' | ',' | ' ' | 'K' | 'M' | '?')
                })
                || s.trim() == ":";
            if keep {
                return s.to_owned();
            }
            let n = aliases.len();
            aliases
                .entry(s.to_owned())
                .or_insert_with(|| {
                    let pad = if s.starts_with(' ') { " " } else { "" };
                    format!("{pad}Слово{n}")
                })
                .clone()
        };
        let mut order: Vec<u32> = Vec::new();
        let pick = |class: &str, take: usize, cap: usize, order: &mut Vec<u32>| {
            let mut roots: Vec<u32> = all_rows
                .iter()
                .filter(|(_, r)| has(r, class))
                .map(|(i, _)| *i)
                .collect();
            roots.sort_unstable();
            for r in roots.into_iter().take(take) {
                let mut q = std::collections::VecDeque::from([r]);
                let mut n = 0;
                while let Some(x) = q.pop_front() {
                    if n >= cap {
                        break;
                    }
                    n += 1;
                    order.push(x);
                    q.extend(kids.get(&x).into_iter().flatten().copied());
                }
            }
        };
        pick("ListItem-button", 3, 40, &mut order);
        pick("Message", 2, 40, &mut order);
        let pos: HashMap<u32, usize> = order.iter().enumerate().map(|(k, i)| (*i, k + 1)).collect();
        let mut json = vec![serde_json::json!({
            "i": 0, "p": -1, "role": SemRole::Generic.code(), "tag": "div", "class": "", "name": "", "text": ""
        })];
        for i in &order {
            let r = &all_rows[i];
            let p = parent
                .get(i)
                .and_then(|p| pos.get(p))
                .map_or(0, |k| i64::try_from(*k).unwrap());
            json.push(serde_json::json!({
                "i": pos[i], "p": p, "role": r.role,
                "tag": r.tag.as_deref().unwrap_or(""),
                "class": r.class.as_deref().unwrap_or(""),
                "name": r.name.as_deref().map(&mut alias).unwrap_or_default(),
                "text": r.text.as_deref().map(&mut alias).unwrap_or_default(),
            }));
        }
        std::fs::write(&out, serde_json::to_string_pretty(&json).unwrap()).unwrap();
        println!("fixture written: {} nodes", json.len());
    }
    for o in ops.iter().take(4) {
        println!("{o:?}");
    }
    println!(
        "ListItem-button rows {chat_rows}, name facts {names}, ops {}, stats {:?}",
        ops.len(),
        b.stats()
    );
}
