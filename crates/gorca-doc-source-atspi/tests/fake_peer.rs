// SPDX-License-Identifier: GPL-3.0-or-later
//! End-to-end tests of the source against an in-process fake AT-SPI peer (a zbus peer-to-peer
//! server that answers `Accessible` and `Collection.GetTree` over a socketpair), so the block
//! fetch, the event-driven updates and the identity ladder run in `make check` without a browser.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::used_underscore_binding,
    clippy::unused_async,
    clippy::needless_range_loop,
    clippy::implicit_clone,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gorca_doc_source_api::{DocSource, DocSourceCtx, delta_channel, join_channel, web_attrs};
use gorca_doc_source_atspi::roles::raw;
use gorca_doc_source_atspi::source::SourceConfig;
use gorca_doc_source_atspi::{
    AppConn, AtspiDocSource, AtspiEvent, AttrInterner, EventKind, FetchOpts, PathKey, PathTable,
    Split, StatePolicy, TextPolicy, fetch_document,
};
use gorca_model::{DocDelta, DocId, Role, SrcNodeId};
use gorca_rt::{CancellationToken, Clock, mpsc};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

// ---------------------------------------------------------------------------------------------
// The fake application

#[derive(Clone, Debug)]
struct FNode {
    role: u32,
    state: u64,
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<u64>,
    /// The hypertext (`Text.GetText`).
    text: String,
    /// Answer `GetRole` only after this long (a hung page).
    role_delay: Duration,
}

#[derive(Debug, Default)]
struct Tree {
    nodes: HashMap<u64, FNode>,
    next: u64,
    /// Objects `Action.DoAction` ran on, in order.
    acted: Vec<u64>,
}

type Shared = Arc<Mutex<Tree>>;

fn path_of(id: u64) -> String {
    format!("/org/a11y/atspi/accessible/{id}")
}

fn opath(id: u64) -> OwnedObjectPath {
    OwnedObjectPath::try_from(path_of(id)).unwrap()
}

struct Acc {
    tree: Shared,
    id: u64,
}

#[zbus::interface(name = "org.a11y.atspi.Accessible")]
impl Acc {
    async fn get_role(&self) -> u32 {
        let (role, delay) = {
            let t = self.tree.lock().unwrap();
            let n = &t.nodes[&self.id];
            (n.role, n.role_delay)
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        role
    }

    fn get_state(&self) -> Vec<u32> {
        let s = self.tree.lock().unwrap().nodes[&self.id].state;
        vec![(s & 0xffff_ffff) as u32, (s >> 32) as u32]
    }

    fn get_children(&self) -> Vec<(String, OwnedObjectPath)> {
        let t = self.tree.lock().unwrap();
        t.nodes[&self.id]
            .children
            .iter()
            .map(|c| (":1.1".to_owned(), opath(*c)))
            .collect()
    }

    fn get_attributes(&self) -> HashMap<String, String> {
        self.tree.lock().unwrap().nodes[&self.id]
            .attrs
            .iter()
            .cloned()
            .collect()
    }

    #[zbus(property)]
    fn name(&self) -> String {
        self.tree.lock().unwrap().nodes[&self.id].name.clone()
    }

    #[zbus(property)]
    fn parent(&self) -> (String, OwnedObjectPath) {
        let t = self.tree.lock().unwrap();
        for (pid, n) in &t.nodes {
            if n.children.contains(&self.id) {
                return (":1.1".to_owned(), opath(*pid));
            }
        }
        (
            ":1.1".to_owned(),
            OwnedObjectPath::try_from("/org/a11y/atspi/null").unwrap(),
        )
    }

    #[zbus(property)]
    fn child_count(&self) -> i32 {
        i32::try_from(self.tree.lock().unwrap().nodes[&self.id].children.len()).unwrap()
    }
}

struct Act {
    tree: Shared,
    id: u64,
}

#[zbus::interface(name = "org.a11y.atspi.Action")]
impl Act {
    fn do_action(&self, _index: i32) -> bool {
        self.tree.lock().unwrap().acted.push(self.id);
        true
    }
}

struct Txt {
    tree: Shared,
    id: u64,
}

#[zbus::interface(name = "org.a11y.atspi.Text")]
impl Txt {
    fn get_text(&self, start: i32, end: i32) -> String {
        let t = self.tree.lock().unwrap();
        let chars: Vec<char> = t.nodes[&self.id].text.chars().collect();
        let s = usize::try_from(start).unwrap_or(0).min(chars.len());
        let e = usize::try_from(end).unwrap_or(chars.len()).min(chars.len());
        chars[s..e.max(s)].iter().collect()
    }
}

struct Coll {
    tree: Shared,
    id: u64,
}

type Rule = (
    Vec<i32>,
    i32,
    HashMap<String, String>,
    i32,
    Vec<i32>,
    i32,
    Vec<String>,
    i32,
    bool,
);
type TreeRow = ((String, OwnedObjectPath), HashMap<String, OwnedValue>);

#[zbus::interface(name = "org.a11y.atspi.Collection")]
impl Coll {
    fn get_tree(&self, _rule: Rule, _props: Vec<String>) -> Vec<TreeRow> {
        let t = self.tree.lock().unwrap();
        let mut out = Vec::new();
        let mut stack = vec![self.id];
        while let Some(id) = stack.pop() {
            let n = &t.nodes[&id];
            let mut m: HashMap<String, OwnedValue> = HashMap::new();
            m.insert(
                "Name".into(),
                Value::from(n.name.clone()).try_into().unwrap(),
            );
            m.insert(
                "ChildCount".into(),
                Value::from(i32::try_from(n.children.len()).unwrap())
                    .try_into()
                    .unwrap(),
            );
            let attrs: HashMap<String, String> = n.attrs.iter().cloned().collect();
            m.insert("Attributes".into(), Value::from(attrs).try_into().unwrap());
            out.push(((":1.1".to_owned(), opath(id)), m));
            for c in n.children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }
}

struct Fake {
    tree: Shared,
    servers: Vec<zbus::Connection>,
}

impl Fake {
    /// `lanes` client connections, each to its own server over a socketpair, sharing one tree.
    async fn start(lanes: usize) -> (Self, AppConn) {
        let tree: Shared = Arc::new(Mutex::new(Tree {
            nodes: HashMap::new(),
            next: 1,
            acted: Vec::new(),
        }));
        let mut servers = Vec::new();
        let mut clients = Vec::new();
        for _ in 0..lanes {
            let (a, b) = tokio::net::UnixStream::pair().unwrap();
            let guid = zbus::Guid::generate();
            let s = zbus::connection::Builder::unix_stream(a)
                .server(guid)
                .unwrap()
                .p2p()
                .build();
            let c = zbus::connection::Builder::unix_stream(b).p2p().build();
            let (s, c) = tokio::try_join!(s, c).unwrap();
            servers.push(s);
            clients.push(c);
        }
        (Self { tree, servers }, AppConn::from_p2p(clients))
    }

    async fn register(&self, id: u64) {
        let p = path_of(id);
        for s in &self.servers {
            s.object_server()
                .at(
                    p.as_str(),
                    Acc {
                        tree: Arc::clone(&self.tree),
                        id,
                    },
                )
                .await
                .unwrap();
            s.object_server()
                .at(
                    p.as_str(),
                    Coll {
                        tree: Arc::clone(&self.tree),
                        id,
                    },
                )
                .await
                .unwrap();
            s.object_server()
                .at(
                    p.as_str(),
                    Txt {
                        tree: Arc::clone(&self.tree),
                        id,
                    },
                )
                .await
                .unwrap();
            s.object_server()
                .at(
                    p.as_str(),
                    Act {
                        tree: Arc::clone(&self.tree),
                        id,
                    },
                )
                .await
                .unwrap();
        }
    }

    async fn unregister(&self, id: u64) {
        let p = path_of(id);
        for s in &self.servers {
            let _ = s.object_server().remove::<Acc, _>(p.as_str()).await;
            let _ = s.object_server().remove::<Coll, _>(p.as_str()).await;
            let _ = s.object_server().remove::<Act, _>(p.as_str()).await;
            let _ = s.object_server().remove::<Txt, _>(p.as_str()).await;
        }
    }

    /// Add a node; returns its id. The caller links it into a parent with [`Fake::link`].
    async fn node(&self, role: u32, name: &str, tag: &str, dom_id: Option<&str>) -> u64 {
        let id = {
            let mut t = self.tree.lock().unwrap();
            let id = t.next;
            t.next += 1;
            let mut attrs = vec![("tag".to_owned(), tag.to_owned())];
            if let Some(d) = dom_id {
                attrs.push(("id".to_owned(), d.to_owned()));
            }
            t.nodes.insert(
                id,
                FNode {
                    role,
                    state: (1 << 25) | (1 << 30) | (1 << 8),
                    name: name.to_owned(),
                    attrs,
                    children: Vec::new(),
                    text: String::new(),
                    role_delay: Duration::ZERO,
                },
            );
            id
        };
        self.register(id).await;
        id
    }

    fn link(&self, parent: u64, child: u64) {
        self.tree
            .lock()
            .unwrap()
            .nodes
            .get_mut(&parent)
            .unwrap()
            .children
            .push(child);
    }

    fn set_children(&self, parent: u64, children: Vec<u64>) {
        self.tree
            .lock()
            .unwrap()
            .nodes
            .get_mut(&parent)
            .unwrap()
            .children = children;
    }

    fn set_text(&self, id: u64, text: &str) {
        let mut t = self.tree.lock().unwrap();
        text.clone_into(&mut t.nodes.get_mut(&id).unwrap().text);
    }

    fn rename(&self, id: u64, name: &str) {
        self.tree
            .lock()
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap()
            .name
            .clear();
        self.tree
            .lock()
            .unwrap()
            .nodes
            .get_mut(&id)
            .unwrap()
            .name
            .push_str(name);
    }

    /// doc(1) -> ul -> n x li(name "item i", id "item-i")
    async fn list(&self, n: usize) -> (u64, u64, Vec<u64>) {
        let doc = self.node(raw::DOCUMENT_WEB, "doc", "body", None).await;
        let ul = self.node(raw::LIST, "", "ul", None).await;
        self.link(doc, ul);
        let mut lis = Vec::new();
        for i in 0..n {
            let li = self
                .node(
                    raw::LIST_ITEM,
                    &format!("item {i}"),
                    "li",
                    Some(&format!("item-{i}")),
                )
                .await;
            self.link(ul, li);
            lis.push(li);
        }
        (doc, ul, lis)
    }
}

fn key(id: u64) -> PathKey {
    PathKey::parse(&path_of(id)).unwrap()
}

fn fast_opts(split: Split) -> FetchOpts {
    FetchOpts {
        split,
        window: 8,
        call_timeout: Duration::from_millis(500),
        budget: Duration::from_secs(5),
        warm: false,
        ..FetchOpts::default()
    }
}

// ---------------------------------------------------------------------------------------------
// Block fetch

#[tokio::test]
async fn block_fetch_builds_the_tree_with_roles_states_and_attributes() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, _ul, lis) = fake.list(40).await;
    let conn = Arc::new(conn);
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let d = fetch_document(
        &conn,
        &interner,
        &mut paths,
        &path_of(doc),
        &fast_opts(Split::Whole),
    )
    .await
    .unwrap();
    assert_eq!(d.block.node_count(), 42);
    assert_eq!(d.block.nodes[0].role, Role::DocumentWeb);
    assert_eq!(d.block.nodes[1].role, Role::List);
    assert_eq!(d.block.nodes[2].role, Role::ListItem);
    assert_eq!(d.block.nodes[2].name.as_deref(), Some("item 0"));
    let tag = d.block.nodes[2]
        .attrs
        .iter()
        .find(|(k, _)| *k == web_attrs::TAG)
        .map(|(_, v)| v.to_string());
    assert_eq!(tag.as_deref(), Some("li"));
    let id = d.block.nodes[2]
        .attrs
        .iter()
        .find(|(k, _)| *k == web_attrs::DOM_ID)
        .map(|(_, v)| v.to_string());
    assert_eq!(id.as_deref(), Some("item-0"));
    // List items are "stateful": their state was fetched; the list's was defaulted.
    assert!(
        d.block.nodes[2]
            .states
            .contains(gorca_model::StateSet::SHOWING)
    );
    // Children lists are complete and ordered.
    let ul_children = &d
        .block
        .children
        .iter()
        .find(|(p, _)| p.0 == key(1 + 1).0)
        .unwrap()
        .1;
    assert_eq!(ul_children.len(), 40);
    assert_eq!(ul_children[0].0, key(lis[0]).0);
    assert!(d.timings.get_tree > Duration::ZERO);
    assert_eq!(d.timings.failed_calls, 0);
}

#[tokio::test]
async fn frontier_split_equals_whole_fetch() {
    let (fake, conn) = Fake::start(4).await;
    // doc -> 6 sections -> 5 paragraphs each
    let doc = fake.node(raw::DOCUMENT_WEB, "doc", "body", None).await;
    for s in 0..6 {
        let sec = fake
            .node(raw::SECTION, &format!("s{s}"), "section", None)
            .await;
        fake.link(doc, sec);
        for p in 0..5 {
            let para = fake
                .node(raw::PARAGRAPH, &format!("p{s}.{p}"), "p", None)
                .await;
            fake.link(sec, para);
        }
    }
    let conn = Arc::new(conn);
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let whole = fetch_document(
        &conn,
        &interner,
        &mut paths,
        &path_of(doc),
        &fast_opts(Split::Whole),
    )
    .await
    .unwrap();
    let front = fetch_document(
        &conn,
        &interner,
        &mut paths,
        &path_of(doc),
        &fast_opts(Split::Frontier {
            min_subtrees: 4,
            max_depth: 3,
        }),
    )
    .await
    .unwrap();
    let sig = |d: &gorca_doc_source_atspi::FetchedDoc| -> Vec<(u64, Role, Option<String>, usize)> {
        d.block
            .nodes
            .iter()
            .map(|n| {
                (
                    n.id.0,
                    n.role,
                    n.name.as_deref().map(str::to_owned),
                    n.attrs.len(),
                )
            })
            .collect()
    };
    assert_eq!(sig(&whole), sig(&front));
    assert_eq!(whole.block.children.len(), front.block.children.len());
}

#[tokio::test]
async fn a_hung_node_cannot_stall_the_fetch_past_its_deadline() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, _ul, lis) = fake.list(20).await;
    // One list item never answers GetRole in time.
    fake.tree
        .lock()
        .unwrap()
        .nodes
        .get_mut(&lis[7])
        .unwrap()
        .role_delay = Duration::from_secs(30);
    let conn = Arc::new(conn);
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let mut o = fast_opts(Split::Whole);
    // Long enough that a healthy call is never timed out by a loaded host (the test counts
    // failed calls exactly); the hung one takes the full timeout either way.
    o.call_timeout = Duration::from_millis(1500);
    let t = Instant::now();
    let d = fetch_document(&conn, &interner, &mut paths, &path_of(doc), &o)
        .await
        .unwrap();
    // The hung call ends at its 1.5 s timeout instead of its 30 s delay: the fetch is not
    // stalled past the deadline.
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "took {:?}",
        t.elapsed()
    );
    assert_eq!(d.timings.failed_calls, 1, "exactly the hung call failed");
    // The node is still in the tree, with an unknown role.
    assert_eq!(d.block.node_count(), 22);
    assert_eq!(d.block.nodes[2 + 7].role, Role::Unknown);
}

#[tokio::test]
async fn text_tier_is_optional_and_states_can_be_skipped() {
    let (fake, conn) = Fake::start(1).await;
    let (doc, _ul, _) = fake.list(5).await;
    let conn = Arc::new(conn);
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let mut o = fast_opts(Split::Whole);
    o.states = StatePolicy::None;
    o.text = TextPolicy::None;
    let d = fetch_document(&conn, &interner, &mut paths, &path_of(doc), &o)
        .await
        .unwrap();
    assert_eq!(
        d.block.completeness,
        gorca_doc_source_api::Completeness::Roles
    );
    assert_eq!(d.timings.state_calls, 0);
}

// ---------------------------------------------------------------------------------------------
// Live source: events -> deltas

struct Harness {
    fake: Fake,
    ev_tx: mpsc::Sender<AtspiEvent>,
    rx: gorca_doc_source_api::DeltaReceiver,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), gorca_doc_source_api::SourceError>>,
    metrics: gorca_doc_source_atspi::SourceMetrics,
    /// Kept alive: a closed request channel means "detach" to the source.
    _req_tx: mpsc::Sender<gorca_doc_source_api::DocRequest>,
}

async fn start_source_with(
    fake: Fake,
    conn: AppConn,
    doc: u64,
    use_dom_id: bool,
    first_nodes: usize,
) -> Harness {
    let (ev_tx, ev_rx) = mpsc::channel(1024);
    let cfg = SourceConfig {
        fetch: fast_opts(Split::Whole),
        settle: gorca_doc_source_atspi::SettlePolicy {
            quiet: Duration::from_millis(15),
            max_wait: Duration::from_millis(60),
            ..gorca_doc_source_atspi::SettlePolicy::default()
        },
        use_dom_id,
        first_nodes,
        ..SourceConfig::default()
    };
    let (source, metrics) = AtspiDocSource::new(Arc::new(conn), path_of(doc), cfg, ev_rx);
    let (sink, rx) = delta_channel(1 << 14);
    let (req_tx, req_rx) = mpsc::channel(8);
    let (join, _j) = join_channel(8);
    let shutdown = CancellationToken::new();
    let (_core_tx, core_events) = mpsc::channel(8);
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
    Harness {
        fake,
        ev_tx,
        rx,
        shutdown,
        task,
        metrics,
        _req_tx: req_tx,
    }
}

async fn start_source(fake: Fake, conn: AppConn, doc: u64, use_dom_id: bool) -> Harness {
    // The default block size is larger than these toy pages: they arrive as one block.
    start_source_with(fake, conn, doc, use_dom_id, 400).await
}

/// A tiny mirror: id -> (role, name, children), applying the documented delta semantics.
#[derive(Default)]
struct Mini {
    nodes: HashMap<SrcNodeId, (Role, Option<String>, Vec<SrcNodeId>)>,
    root: Option<SrcNodeId>,
    removed: Vec<SrcNodeId>,
}

impl Mini {
    fn apply(&mut self, d: DocDelta) {
        match d {
            DocDelta::Reset { root, .. } => {
                self.nodes.clear();
                self.root = Some(root);
            }
            DocDelta::Upsert(ns) => {
                for n in ns {
                    let e = self
                        .nodes
                        .entry(n.id)
                        .or_insert((Role::Unknown, None, Vec::new()));
                    e.0 = n.role;
                    e.1 = n.name.as_deref().map(str::to_owned);
                }
            }
            DocDelta::Children { parent, children } => {
                let old = self
                    .nodes
                    .get(&parent)
                    .map(|n| n.2.clone())
                    .unwrap_or_default();
                for gone in old.iter().filter(|o| !children.contains(o)) {
                    self.drop(*gone);
                }
                if let Some(p) = self.nodes.get_mut(&parent) {
                    p.2 = children.to_vec();
                }
            }
            DocDelta::Removed(ids) => {
                for i in ids {
                    self.removed.push(i);
                    self.drop(i);
                }
            }
            DocDelta::Text { .. } | DocDelta::Settled { .. } | DocDelta::Hint(_) => {}
        }
    }

    fn drop(&mut self, id: SrcNodeId) {
        let mut st = vec![id];
        while let Some(n) = st.pop() {
            if let Some(x) = self.nodes.remove(&n) {
                st.extend(x.2);
            }
        }
    }

    fn names(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut st: Vec<SrcNodeId> = self.root.into_iter().collect();
        while let Some(id) = st.pop() {
            let Some(n) = self.nodes.get(&id) else {
                continue;
            };
            out.push(n.1.clone().unwrap_or_default());
            st.extend(n.2.iter().rev());
        }
        out
    }

    fn id_of_name(&self, name: &str) -> Option<SrcNodeId> {
        self.nodes
            .iter()
            .find(|(_, n)| n.1.as_deref() == Some(name))
            .map(|(i, _)| *i)
    }
}

async fn pump(h: &mut Harness, mini: &mut Mini, until: impl Fn(&Mini) -> bool) {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        while let Ok(d) = h.rx.deltas.try_recv() {
            mini.apply(d);
        }
        if until(mini) {
            return;
        }
        assert!(
            Instant::now() < end,
            "timed out waiting for the mirror; names = {:?}",
            mini.names()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn ev(path: u64, kind: EventKind) -> AtspiEvent {
    AtspiEvent {
        path: key(path),
        kind,
        at: Instant::now(),
    }
}

#[tokio::test]
async fn initial_build_then_children_changed_adds_and_removes() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(10).await;
    let mut h = start_source(fake, conn, doc, true).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 12).await;
    let id3 = mini.id_of_name("item 3").unwrap();

    // The page appends two items and removes item 0.
    let a = h
        .fake
        .node(raw::LIST_ITEM, "item 10", "li", Some("item-10"))
        .await;
    let b = h
        .fake
        .node(raw::LIST_ITEM, "item 11", "li", Some("item-11"))
        .await;
    let mut kids = lis[1..].to_vec();
    kids.push(a);
    kids.push(b);
    h.fake.set_children(ul, kids);
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildAdded {
                index: 10,
                child: Some(key(a)),
            },
        ))
        .await
        .unwrap();
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildAdded {
                index: 11,
                child: Some(key(b)),
            },
        ))
        .await
        .unwrap();
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildRemoved {
                index: 0,
                child: Some(key(lis[0])),
            },
        ))
        .await
        .unwrap();
    pump(&mut h, &mut mini, |m| {
        m.names().contains(&"item 11".to_owned()) && !m.names().contains(&"item 0".to_owned())
    })
    .await;
    assert_eq!(mini.names().len(), 13);
    assert_eq!(
        mini.id_of_name("item 3"),
        Some(id3),
        "untouched siblings keep their id"
    );
    let ups = h.metrics.updates();
    assert_eq!(ups.len(), 1, "one burst, one update");
    assert_eq!(ups[0].parents_refreshed, 1);
    assert_eq!(
        ups[0].nodes_fetched, 2,
        "only the two new subtrees were fetched"
    );
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn recreated_items_keep_their_logical_id_in_any_order() {
    // Firefox moves a DOM node by destroying its accessible and creating a new one.
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(6).await;
    let mut h = start_source(fake, conn, doc, true).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 8).await;
    let before: Vec<SrcNodeId> = (0..6)
        .map(|i| mini.id_of_name(&format!("item {i}")).unwrap())
        .collect();

    // Reverse: every item gets a new object at a new path, in reverse order.
    let mut fresh = Vec::new();
    for i in (0..6).rev() {
        fresh.push(
            h.fake
                .node(
                    raw::LIST_ITEM,
                    &format!("item {i}"),
                    "li",
                    Some(&format!("item-{i}")),
                )
                .await,
        );
    }
    for old in &lis {
        h.fake.unregister(*old).await;
    }
    h.fake.set_children(ul, fresh.clone());
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildRemoved {
                index: 0,
                child: None,
            },
        ))
        .await
        .unwrap();
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildAdded {
                index: -1,
                child: None,
            },
        ))
        .await
        .unwrap();
    pump(&mut h, &mut mini, |m| {
        m.names().last().is_some_and(|n| n == "item 0")
    })
    .await;
    for i in 0..6 {
        assert_eq!(
            mini.id_of_name(&format!("item {i}")),
            Some(before[i]),
            "item {i} must keep its logical id"
        );
    }
    assert!(
        mini.removed.is_empty(),
        "nothing was removed, only re-bound: {:?}",
        mini.removed
    );
    let rec = &h.metrics.updates()[0];
    assert_eq!(rec.stats.by_dom_id, 6, "DOM ids carry the match");
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn without_dom_ids_the_name_tag_rung_still_matches_moves() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(6).await;
    let mut h = start_source(fake, conn, doc, false).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 8).await;
    let before: Vec<SrcNodeId> = (0..6)
        .map(|i| mini.id_of_name(&format!("item {i}")).unwrap())
        .collect();
    let mut fresh = Vec::new();
    for i in [3usize, 1, 5, 0, 4, 2] {
        fresh.push(
            h.fake
                .node(
                    raw::LIST_ITEM,
                    &format!("item {i}"),
                    "li",
                    Some(&format!("item-{i}")),
                )
                .await,
        );
    }
    for old in &lis {
        h.fake.unregister(*old).await;
    }
    h.fake.set_children(ul, fresh);
    h.ev_tx
        .send(ev(
            ul,
            EventKind::ChildAdded {
                index: -1,
                child: None,
            },
        ))
        .await
        .unwrap();
    pump(&mut h, &mut mini, |m| {
        m.names().get(2).is_some_and(|n| n == "item 3")
    })
    .await;
    for i in 0..6 {
        assert_eq!(mini.id_of_name(&format!("item {i}")), Some(before[i]));
    }
    let rec = &h.metrics.updates()[0];
    assert_eq!(rec.stats.by_dom_id, 0);
    assert_eq!(rec.stats.by_full_key, 6);
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn name_edits_keep_ids_but_a_permutation_in_place_moves_them() {
    // Chromium reuses the object at a path for other content. Edits keep the id; a swap of two
    // siblings' content (React with index keys) makes the ids follow the content.
    let (fake, conn) = Fake::start(2).await;
    let (doc, _ul, lis) = fake.list(4).await;
    let mut h = start_source(fake, conn, doc, false).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 6).await;
    let id0 = mini.id_of_name("item 0").unwrap();
    let id1 = mini.id_of_name("item 1").unwrap();

    // Edit: item 2 becomes "item 2 (edited)".
    h.fake.rename(lis[2], "item 2 (edited)");
    h.ev_tx
        .send(ev(lis[2], EventKind::NameChanged))
        .await
        .unwrap();
    pump(&mut h, &mut mini, |m| {
        m.names().contains(&"item 2 (edited)".to_owned())
    })
    .await;
    assert!(mini.id_of_name("item 2 (edited)").is_some());
    assert!(!h.metrics.updates()[0].rewrite);

    // Swap content of item 0 and item 1 in place.
    h.fake.rename(lis[0], "item 1");
    h.fake.rename(lis[1], "item 0");
    h.ev_tx
        .send(ev(lis[0], EventKind::NameChanged))
        .await
        .unwrap();
    h.ev_tx
        .send(ev(lis[1], EventKind::NameChanged))
        .await
        .unwrap();
    pump(&mut h, &mut mini, |m| {
        m.names().get(2).is_some_and(|n| n == "item 1")
    })
    .await;
    assert_eq!(
        mini.id_of_name("item 0"),
        Some(id0),
        "id follows the content"
    );
    assert_eq!(mini.id_of_name("item 1"), Some(id1));
    assert!(h.metrics.updates().iter().any(|u| u.rewrite));
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn event_storm_beyond_the_threshold_resyncs_the_document() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, _ul, _lis) = fake.list(30).await;
    let mut h = start_source(fake, conn, doc, true).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 32).await;
    let before: Vec<SrcNodeId> = (0..30)
        .map(|i| mini.id_of_name(&format!("item {i}")).unwrap())
        .collect();
    // 500 distinct dirty nodes > resync_threshold (400): the source refetches everything.
    for i in 0..500u64 {
        h.ev_tx
            .send(ev(10_000 + i, EventKind::NameChanged))
            .await
            .unwrap();
    }
    // Unknown paths are skipped, but the count alone triggers the overload path.
    let end = Instant::now() + Duration::from_secs(5);
    while h.metrics.updates().is_empty() && Instant::now() < end {
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let ups = h.metrics.updates();
    assert!(
        ups[0].resync,
        "overload must degrade to a document resync: {:?}",
        ups[0]
    );
    pump(&mut h, &mut mini, |m| m.names().len() == 32).await;
    for i in 0..30 {
        assert_eq!(
            mini.id_of_name(&format!("item {i}")),
            Some(before[i]),
            "a resync keeps logical ids"
        );
    }
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn revalidate_on_read_finds_a_silent_reorder() {
    // Chromium reorders a keyed list without any structure event (paths stay, order changes).
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(5).await;
    let mut h = start_source(fake, conn, doc, true).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 7).await;
    let ids: Vec<SrcNodeId> = lis.iter().map(|l| SrcNodeId(key(*l).0)).collect();

    let mut rev = lis.clone();
    rev.reverse();
    h.fake.set_children(ul, rev);
    let (tx, rx) = gorca_rt::oneshot::channel();
    h._req_tx
        .send(gorca_doc_source_api::DocRequest::Revalidate {
            parent: SrcNodeId(key(ul).0),
            deadline: gorca_rt::Deadline(Instant::now() + Duration::from_secs(2)),
            reply: tx,
        })
        .await
        .unwrap();
    let got = rx.await.unwrap().unwrap();
    let want: Vec<SrcNodeId> = ids.iter().rev().copied().collect();
    assert_eq!(got.to_vec(), want, "the reply carries the new order");
    pump(&mut h, &mut mini, |m| {
        m.names().get(2).is_some_and(|n| n == "item 4")
    })
    .await;
    assert!(mini.removed.is_empty());
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

#[tokio::test]
async fn a_defunct_document_root_detaches_the_source() {
    // Navigation replaces the document accessible; the old root reports defunct.
    let (fake, conn) = Fake::start(1).await;
    let (doc, _ul, _lis) = fake.list(3).await;
    let mut h = start_source(fake, conn, doc, true).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 5).await;
    h.ev_tx.send(ev(doc, EventKind::Defunct)).await.unwrap();
    let r = tokio::time::timeout(Duration::from_secs(2), h.task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(r, Err(gorca_doc_source_api::SourceError::Detached));
}

#[tokio::test]
async fn progressive_build_delivers_the_top_of_the_page_first() {
    let (fake, conn) = Fake::start(2).await;
    // doc -> 6 sections -> 5 paragraphs each (37 nodes)
    let doc = fake.node(raw::DOCUMENT_WEB, "doc", "body", None).await;
    for s in 0..6 {
        let sec = fake
            .node(raw::SECTION, &format!("s{s}"), "section", None)
            .await;
        fake.link(doc, sec);
        for p in 0..5 {
            let para = fake
                .node(raw::PARAGRAPH, &format!("p{s}.{p}"), "p", None)
                .await;
            fake.link(sec, para);
        }
    }
    let mut h = start_source_with(fake, conn, doc, true, 8).await;
    let mut mini = Mini::default();
    pump(&mut h, &mut mini, |m| m.names().len() == 37).await;
    let (first_at, first_nodes) = h.metrics.first_block().expect("first block recorded");
    assert!(
        first_nodes < 37,
        "the first block must not be the whole page: {first_nodes}"
    );
    assert!(
        first_nodes >= 7,
        "root plus at least one section: {first_nodes}"
    );
    assert!(first_at < Duration::from_secs(2));
    let (build, total) = h.metrics.build().expect("build recorded");
    assert_eq!(total, 37);
    assert!(build.total >= first_at);
    assert_eq!(mini.names()[0], "doc");
    assert_eq!(mini.names()[1], "s0");
    h.shutdown.cancel();
    h.task.await.unwrap().unwrap();
}

// ---------------------------------------------------------------------------------------------
// Browse configuration: text tier, run leaves, Settled markers, routed events, activity

use gorca_doc_source_api::{Activity, DocEvent, DocRequest};
use gorca_model::{NodeEvent, NodeKey};

struct Browse {
    fake: Fake,
    rx: gorca_doc_source_api::DeltaReceiver,
    events: mpsc::Sender<DocEvent>,
    requests: mpsc::Sender<DocRequest>,
    shutdown: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), gorca_doc_source_api::SourceError>>,
}

fn app_id() -> gorca_model::AppId {
    gorca_model::AppId {
        connector: gorca_model::ConnectorId(0),
        serial: 7,
    }
}

fn node_key(id: u64) -> NodeKey {
    NodeKey {
        app: app_id(),
        local: gorca_dbus::drain::local_id(&path_of(id)),
    }
}

async fn start_browse(fake: Fake, conn: AppConn, doc: u64) -> Browse {
    let (_unused_tx, ev_rx) = mpsc::channel(8);
    let mut cfg = SourceConfig::browse(gorca_model::TextUnit::Utf16Bom, false);
    cfg.fetch = FetchOpts {
        text: TextPolicy::All,
        ..fast_opts(Split::Whole)
    };
    cfg.settle = gorca_doc_source_atspi::SettlePolicy {
        quiet: Duration::from_millis(15),
        max_wait: Duration::from_millis(60),
        ..gorca_doc_source_atspi::SettlePolicy::default()
    };
    let (source, _metrics) = AtspiDocSource::new(Arc::new(conn), path_of(doc), cfg, ev_rx);
    let (sink, rx) = delta_channel(1 << 14);
    let (req_tx, req_rx) = mpsc::channel(8);
    let (join, _j) = join_channel(8);
    let (events, core_events) = mpsc::channel(64);
    let shutdown = CancellationToken::new();
    let ctx = DocSourceCtx {
        doc: DocId(1),
        app: app_id(),
        sink,
        requests: req_rx,
        events: core_events,
        join,
        shutdown: shutdown.clone(),
        clock: Clock::System,
    };
    let task = tokio::spawn(Box::new(source).attach(ctx));
    Browse {
        fake,
        rx,
        events,
        requests: req_tx,
        shutdown,
        task,
    }
}

/// Drain deltas until a `Settled` arrives; returns everything before it.
async fn until_settled(b: &mut Browse) -> Vec<DocDelta> {
    let end = Instant::now() + Duration::from_secs(5);
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(50), b.rx.deltas.recv()).await {
            Ok(Some(DocDelta::Settled { .. })) => return out,
            Ok(Some(d)) => out.push(d),
            Ok(None) => panic!("sink closed"),
            Err(_) => assert!(Instant::now() < end, "no Settled; got {} deltas", out.len()),
        }
    }
}

fn upserts_of(ds: &[DocDelta]) -> Vec<gorca_model::SrcNode> {
    ds.iter()
        .filter_map(|d| match d {
            DocDelta::Upsert(v) => Some(v.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

fn text_changed(id: u64, seq: u64) -> DocEvent {
    DocEvent {
        ev: NodeEvent::TextChanged {
            node: node_key(id),
            delta: gorca_model::TextDelta::Replaced { text: None },
        },
        seq,
        at: Instant::now(),
    }
}

#[tokio::test]
async fn browse_build_splits_hypertext_into_runs_and_ends_with_settled() {
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    let link = fake.node(raw::LINK, "Home", "a", None).await;
    fake.link(doc, p);
    fake.link(p, link);
    fake.set_text(p, "Hello \u{FFFC} world");
    fake.set_text(link, "Home");
    let mut b = start_browse(fake, conn, doc).await;
    let batch = until_settled(&mut b).await;
    let ups = upserts_of(&batch);
    let runs: Vec<_> = ups
        .iter()
        .filter(|n| gorca_doc_source_atspi::runs::is_run(n.id))
        .collect();
    let texts: Vec<_> = runs.iter().filter_map(|n| n.text.as_deref()).collect();
    assert!(texts.contains(&"Hello "), "{texts:?}");
    assert!(texts.contains(&" world"), "{texts:?}");
    assert!(texts.contains(&"Home"), "{texts:?}");
    assert!(runs.iter().all(|n| n.local.is_none()));
    let real = ups.iter().find(|n| n.role == Role::Paragraph).unwrap();
    assert_eq!(real.local, Some(node_key(p).local));
    assert!(real.text.is_none(), "container text is out of the node");
    let list = batch
        .iter()
        .find_map(|d| match d {
            DocDelta::Children { parent, children } if parent.0 == key(p).0 => {
                Some(children.to_vec())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(list.len(), 3);
    assert!(gorca_doc_source_atspi::runs::is_run(list[0]));
    assert_eq!(list[1].0, key(link).0);
    assert!(gorca_doc_source_atspi::runs::is_run(list[2]));
    assert!(matches!(batch.first(), Some(DocDelta::Reset { .. })));
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn routed_core_events_refresh_the_document_and_settle() {
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    fake.link(doc, p);
    fake.set_text(p, "one");
    let mut b = start_browse(fake, conn, doc).await;
    let _ = until_settled(&mut b).await;

    // The page edits the paragraph's text.
    b.fake.set_text(p, "two");
    b.events.send(text_changed(p, 1)).await.unwrap();
    let batch = until_settled(&mut b).await;
    let ups = upserts_of(&batch);
    assert!(
        ups.iter().any(|n| n.text.as_deref() == Some("two")),
        "the edited run is re-sent in place"
    );

    // An event for a node the document never saw is ignored: nothing arrives.
    b.events
        .send(DocEvent {
            ev: NodeEvent::ChildrenChanged {
                node: node_key(9999),
            },
            seq: 2,
            at: Instant::now(),
        })
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(200), b.rx.deltas.recv())
            .await
            .is_err()
    );
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn a_dormant_source_marks_only_and_a_wake_rebuilds_with_ids_kept() {
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    fake.link(doc, p);
    fake.set_text(p, "one");
    let mut b = start_browse(fake, conn, doc).await;
    let first = until_settled(&mut b).await;
    let run_before = upserts_of(&first)
        .into_iter()
        .find(|n| gorca_doc_source_atspi::runs::is_run(n.id))
        .map(|n| n.id)
        .unwrap();

    b.requests
        .send(DocRequest::SetActivity(Activity::Dormant))
        .await
        .unwrap();
    b.fake.set_text(p, "two");
    b.events.send(text_changed(p, 1)).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(250), b.rx.deltas.recv())
            .await
            .is_err(),
        "a dormant document does no IPC and pushes nothing"
    );
    b.requests
        .send(DocRequest::SetActivity(Activity::Awake))
        .await
        .unwrap();
    let batch = until_settled(&mut b).await;
    let ups = upserts_of(&batch);
    let run_after = ups
        .iter()
        .find(|n| n.text.as_deref() == Some("two"))
        .map(|n| n.id);
    assert_eq!(run_after, Some(run_before), "the run keeps its id");

    // An explicit rebuild settles even when nothing changed.
    b.requests.send(DocRequest::Rebuild).await.unwrap();
    let _ = until_settled(&mut b).await;
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn the_parent_walk_finds_the_topmost_document_and_a_search_finds_any() {
    use gorca_doc_source_atspi::factory::{any_document, top_document};
    let (fake, conn) = Fake::start(1).await;
    // page(doc) -> section -> iframe(doc) -> p ; and a chrome-like sibling without a document
    let app = fake.node(raw::APPLICATION, "app", "app", None).await;
    let page = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let sec = fake.node(raw::SECTION, "", "section", None).await;
    let inner = fake.node(raw::DOCUMENT_WEB, "inner", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    let chrome = fake.node(raw::PUSH_BUTTON, "tab", "button", None).await;
    fake.link(app, page);
    fake.link(app, chrome);
    fake.link(page, sec);
    fake.link(sec, inner);
    fake.link(inner, p);
    assert_eq!(
        top_document(&conn, &path_of(p)).await,
        Some(path_of(page)),
        "a nested document belongs to the top-level one"
    );
    assert_eq!(top_document(&conn, &path_of(chrome)).await, None);
    // The search starts at the well-known root path, which the fake does not serve: build a
    // fake whose node 0 is not special, so only check that a miss is a clean `None`.
    assert_eq!(any_document(&conn).await, None);
}

// ---------------------------------------------------------------------------------------------
// The off-screen frame wake (`EngineQuirks::wake_offscreen_frames`), M9 review fixes M-1, M-2.

mod frame_wake {
    use super::*;
    use gorca_doc_source_atspi::source::EngineQuirks;

    /// A page whose scroll offset the fake Component interface moves and measures: a node at
    /// page position `top` has its screen y at `top - offset`.
    #[derive(Default)]
    struct Page {
        offset: i32,
        /// (path id, member, time) of every scroll call.
        log: Vec<(u64, &'static str, Instant)>,
    }
    type Shared = Arc<Mutex<Page>>;

    struct Comp {
        page: Shared,
        id: u64,
        top: i32,
    }

    #[zbus::interface(name = "org.a11y.atspi.Component")]
    impl Comp {
        fn scroll_to(&self, _kind: u32) -> bool {
            let mut p = self.page.lock().unwrap();
            p.log.push((self.id, "ScrollTo", Instant::now()));
            p.offset = self.top;
            true
        }

        fn scroll_to_point(&self, _coord: u32, _x: i32, y: i32) -> bool {
            let mut p = self.page.lock().unwrap();
            p.log.push((self.id, "ScrollToPoint", Instant::now()));
            p.offset = self.top - y;
            true
        }

        fn get_extents(&self, _coord: u32) -> (i32, i32, i32, i32) {
            let p = self.page.lock().unwrap();
            (10, self.top - p.offset, 200, 20)
        }
    }

    async fn comp(fake: &Fake, page: &Shared, id: u64, top: i32) {
        for s in &fake.servers {
            s.object_server()
                .at(
                    path_of(id).as_str(),
                    Comp {
                        page: Arc::clone(page),
                        id,
                        top,
                    },
                )
                .await
                .unwrap();
        }
    }

    fn not_showing(fake: &Fake, id: u64) {
        fake.tree.lock().unwrap().nodes.get_mut(&id).unwrap().state &= !(1 << 25);
    }

    async fn start_blink(fake: Fake, conn: AppConn, doc: u64) -> Browse {
        let (_unused_tx, ev_rx) = mpsc::channel(8);
        let mut cfg = SourceConfig::browse(gorca_model::TextUnit::Scalar, true);
        cfg.quirks = EngineQuirks::blink(true);
        cfg.fetch = FetchOpts {
            text: TextPolicy::All,
            ..fast_opts(Split::Whole)
        };
        let (source, _metrics) = AtspiDocSource::new(Arc::new(conn), path_of(doc), cfg, ev_rx);
        let (sink, rx) = delta_channel(1 << 14);
        let (req_tx, req_rx) = mpsc::channel(8);
        let (join, _j) = join_channel(8);
        let (events, core_events) = mpsc::channel(64);
        let shutdown = CancellationToken::new();
        let ctx = DocSourceCtx {
            doc: DocId(1),
            app: app_id(),
            sink,
            requests: req_rx,
            events: core_events,
            join,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let task = tokio::spawn(Box::new(source).attach(ctx));
        Browse {
            fake,
            rx,
            events,
            requests: req_tx,
            shutdown,
            task,
        }
    }

    fn members(page: &Shared) -> Vec<(u64, &'static str)> {
        page.lock()
            .unwrap()
            .log
            .iter()
            .map(|(i, m, _)| (*i, *m))
            .collect()
    }

    /// An article whose first screen has no link, button or list item (only a heading and a
    /// paragraph) and an embedded cross-site frame further down: the frame is scrolled into
    /// view, and the page is back at exactly the original offset (here a page that was
    /// scrolled 130 px when the document was built).
    #[tokio::test]
    async fn frame_wake_restores_the_exact_scroll_offset_on_a_text_only_page() {
        let (fake, conn) = Fake::start(2).await;
        let page: Shared = Arc::default();
        page.lock().unwrap().offset = 130;
        let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
        let h = fake.node(raw::HEADING, "Title", "h1", None).await;
        let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
        let fr = fake.node(28, "Video", "iframe", None).await;
        fake.link(doc, h);
        fake.link(doc, p);
        fake.link(doc, fr);
        fake.set_text(h, "Title");
        fake.set_text(p, "Some text.");
        not_showing(&fake, fr);
        comp(&fake, &page, h, 150).await;
        comp(&fake, &page, p, 400).await;
        comp(&fake, &page, fr, 5000).await;
        let mut b = start_blink(fake, conn, doc).await;
        let _ = until_settled(&mut b).await;
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let calls = members(&page);
        let offset = page.lock().unwrap().offset;
        b.shutdown.cancel();
        let _ = b.task.await;
        assert!(
            calls.contains(&(fr, "ScrollTo")),
            "the frame was scrolled into view: {calls:?}"
        );
        assert!(
            calls.iter().any(|(_, m)| *m == "ScrollToPoint"),
            "the page was scrolled back: {calls:?}"
        );
        assert_eq!(
            offset, 130,
            "the original scroll offset is restored exactly: {calls:?}"
        );
    }

    /// The user scrolls while the frame wakes: the viewport is theirs, nothing is restored.
    #[tokio::test]
    async fn frame_wake_never_overrides_a_scroll_the_user_made_meanwhile() {
        let (fake, conn) = Fake::start(2).await;
        let page: Shared = Arc::default();
        let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
        let h = fake.node(raw::HEADING, "Title", "h1", None).await;
        let fr = fake.node(28, "Video", "iframe", None).await;
        fake.link(doc, h);
        fake.link(doc, fr);
        fake.set_text(h, "Title");
        not_showing(&fake, fr);
        comp(&fake, &page, h, 50).await;
        comp(&fake, &page, fr, 5000).await;
        let mut b = start_blink(fake, conn, doc).await;
        let _ = until_settled(&mut b).await;
        // Wait for the wake's scroll, then scroll like a user would.
        let t0 = Instant::now();
        while !members(&page).contains(&(fr, "ScrollTo")) {
            assert!(
                t0.elapsed() < Duration::from_secs(5),
                "the frame was never scrolled"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        page.lock().unwrap().offset = 777;
        tokio::time::sleep(Duration::from_millis(2000)).await;
        let calls = members(&page);
        let offset = page.lock().unwrap().offset;
        b.shutdown.cancel();
        let _ = b.task.await;
        assert_eq!(offset, 777, "the user's scroll position stays: {calls:?}");
        assert!(
            !calls.iter().any(|(_, m)| *m == "ScrollToPoint"),
            "no restore after a user scroll: {calls:?}"
        );
    }

    /// A page with nothing in view to measure the position against is left alone.
    #[tokio::test]
    async fn frame_wake_does_nothing_when_no_node_is_in_view() {
        let (fake, conn) = Fake::start(2).await;
        let page: Shared = Arc::default();
        let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
        let h = fake.node(raw::HEADING, "Title", "h1", None).await;
        let fr = fake.node(28, "Video", "iframe", None).await;
        fake.link(doc, h);
        fake.link(doc, fr);
        not_showing(&fake, h);
        not_showing(&fake, fr);
        comp(&fake, &page, h, 50).await;
        comp(&fake, &page, fr, 5000).await;
        let mut b = start_blink(fake, conn, doc).await;
        let _ = until_settled(&mut b).await;
        tokio::time::sleep(Duration::from_millis(800)).await;
        let calls = members(&page);
        b.shutdown.cancel();
        let _ = b.task.await;
        assert!(
            calls.is_empty(),
            "no scrolling without a way back: {calls:?}"
        );
    }

    /// The document closes (navigation) right after its build: the frame wake stops with it.
    #[tokio::test]
    async fn frame_wake_stops_with_the_source() {
        let (fake, conn) = Fake::start(2).await;
        let page: Shared = Arc::default();
        let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
        let h = fake.node(raw::HEADING, "Title", "h1", None).await;
        fake.link(doc, h);
        comp(&fake, &page, h, 50).await;
        for i in 0..4 {
            let fr = fake.node(28, &format!("Ad {i}"), "iframe", None).await;
            fake.link(doc, fr);
            not_showing(&fake, fr);
            comp(&fake, &page, fr, 3000 + 100 * i).await;
        }
        let mut b = start_blink(fake, conn, doc).await;
        let _ = until_settled(&mut b).await;
        b.shutdown.cancel();
        let _ = b.task.await;
        let closed = Instant::now();
        tokio::time::sleep(Duration::from_millis(3500)).await;
        let after: Vec<u64> = page
            .lock()
            .unwrap()
            .log
            .iter()
            .filter(|(_, _, t)| *t > closed)
            .map(|(i, _, _)| *i)
            .collect();
        assert!(
            after.is_empty(),
            "frames of a closed document were still scrolled into view: {after:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// Actions on nodes the page replaced

/// Ask the source for a click on `node` and return its answer.
async fn click(b: &Browse, node: SrcNodeId) -> Result<(), gorca_doc_source_api::SourceError> {
    let (reply, rx) = gorca_rt::oneshot::channel();
    b.requests
        .send(DocRequest::Action {
            node,
            act: gorca_model::DocAction::Click,
            deadline: gorca_rt::Deadline(Instant::now() + Duration::from_secs(2)),
            reply,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .expect("the source answers")
        .expect("reply sent")
}

async fn browse_list() -> (Browse, u64, Vec<u64>, Vec<SrcNodeId>) {
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(3).await;
    let mut b = start_browse(fake, conn, doc).await;
    let first = until_settled(&mut b).await;
    let ups = upserts_of(&first);
    let ids = (0..3)
        .map(|i| {
            ups.iter()
                .find(|n| n.name.as_deref() == Some(format!("item {i}").as_str()))
                .map(|n| n.id)
                .expect("list item mirrored")
        })
        .collect();
    (b, ul, lis, ids)
}

#[tokio::test]
async fn a_click_on_a_live_node_does_one_action_on_that_object() {
    let (b, _ul, lis, ids) = browse_list().await;
    click(&b, ids[1]).await.unwrap();
    assert_eq!(b.fake.tree.lock().unwrap().acted, vec![lis[1]]);
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn a_click_on_a_replaced_node_follows_its_identity_to_the_live_object() {
    let (b, ul, lis, ids) = browse_list().await;
    // The page re-rendered the row: a brand-new object, same role, name and DOM id; no event has
    // reached the source yet.
    let fresh = b
        .fake
        .node(raw::LIST_ITEM, "item 1", "li", Some("item-1"))
        .await;
    b.fake.set_children(ul, vec![lis[0], fresh, lis[2]]);
    b.fake.unregister(lis[1]).await;
    click(&b, ids[1]).await.unwrap();
    assert_eq!(
        b.fake.tree.lock().unwrap().acted,
        vec![fresh],
        "the action reaches the new object, once"
    );
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn a_click_never_lands_on_another_element_when_the_node_is_gone() {
    // Replaced by an item with other content in the same slot: not the same item.
    let (b, ul, lis, ids) = browse_list().await;
    let other = b
        .fake
        .node(raw::LIST_ITEM, "item 1 (3 new messages)", "li", None)
        .await;
    b.fake.set_children(ul, vec![lis[0], other, lis[2]]);
    b.fake.unregister(lis[1]).await;
    let r = click(&b, ids[1]).await;
    assert!(
        matches!(r, Err(gorca_doc_source_api::SourceError::Gone)),
        "{r:?}"
    );
    assert!(b.fake.tree.lock().unwrap().acted.is_empty());
    b.shutdown.cancel();
    let _ = b.task.await;

    // Removed outright.
    let (b, ul, lis, ids) = browse_list().await;
    b.fake.set_children(ul, vec![lis[0], lis[2]]);
    b.fake.unregister(lis[1]).await;
    let r = click(&b, ids[1]).await;
    assert!(
        matches!(r, Err(gorca_doc_source_api::SourceError::Gone)),
        "{r:?}"
    );
    assert!(b.fake.tree.lock().unwrap().acted.is_empty());
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn duplicate_names_are_ambiguous_so_the_click_is_refused_not_shifted() {
    let (fake, conn) = Fake::start(2).await;
    let (doc, ul, lis) = fake.list(3).await;
    for li in &lis {
        fake.rename(*li, "Reply");
    }
    let mut b = start_browse(fake, conn, doc).await;
    let first = until_settled(&mut b).await;
    let ids: Vec<SrcNodeId> = upserts_of(&first)
        .iter()
        .filter(|n| n.name.as_deref() == Some("Reply"))
        .map(|n| n.id)
        .collect();
    assert_eq!(ids.len(), 3);
    // The page drops the first reply and re-renders the other two as new objects without DOM
    // ids. The user is on the second reply, which is now the first of the two new ones; the
    // identity ladder pairs duplicates in sibling order, so it cannot be told from the third.
    let f1 = b.fake.node(raw::LIST_ITEM, "Reply", "li", None).await;
    let f2 = b.fake.node(raw::LIST_ITEM, "Reply", "li", None).await;
    b.fake.set_children(ul, vec![f1, f2]);
    for li in &lis {
        b.fake.unregister(*li).await;
    }
    let r = click(&b, ids[1]).await;
    assert!(
        matches!(r, Err(gorca_doc_source_api::SourceError::Gone)),
        "{r:?}"
    );
    assert!(b.fake.tree.lock().unwrap().acted.is_empty());
    b.shutdown.cancel();
    let _ = b.task.await;
}

// ---------------------------------------------------------------------------------------------
// CHR-0: act only on what was spoken, and repairs that do not block the request loop

#[tokio::test]
async fn a_click_on_an_object_that_no_longer_says_what_was_spoken_is_refused() {
    let (b, _ul, lis, ids) = browse_list().await;
    // The page re-used the object for another row: same object, new name, no event yet.
    b.fake.rename(lis[1], "item 1 (draft)");
    let r = click(&b, ids[1]).await;
    assert!(
        matches!(&r, Err(gorca_doc_source_api::SourceError::Displaced(Some(n))) if &**n == "item 1 (draft)"),
        "{r:?}"
    );
    assert!(b.fake.tree.lock().unwrap().acted.is_empty());
    // An untouched sibling still clicks.
    click(&b, ids[2]).await.unwrap();
    assert_eq!(b.fake.tree.lock().unwrap().acted, vec![lis[2]]);
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn text_requests_are_served_while_a_repair_fetches() {
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    fake.link(doc, p);
    fake.set_text(p, "one");
    let mut b = start_browse(fake, conn, doc).await;
    let first = until_settled(&mut b).await;
    let para = upserts_of(&first)
        .into_iter()
        .find(|n| n.role == Role::Paragraph)
        .map(|n| n.id)
        .expect("paragraph mirrored");
    // The repair role pass now takes 400 ms.
    b.fake
        .tree
        .lock()
        .unwrap()
        .nodes
        .get_mut(&p)
        .unwrap()
        .role_delay = Duration::from_millis(400);
    let (reply, repaired) = gorca_rt::oneshot::channel();
    b.requests
        .send(DocRequest::Repair {
            cheap_check: false,
            reply,
        })
        .await
        .unwrap();
    let t = Instant::now();
    let (reply, text) = gorca_rt::oneshot::channel();
    b.requests
        .send(DocRequest::Text {
            node: para,
            range: 0..3,
            deadline: gorca_rt::Deadline(Instant::now() + Duration::from_secs(2)),
            reply,
        })
        .await
        .unwrap();
    let got = tokio::time::timeout(Duration::from_secs(3), text)
        .await
        .expect("the text answers")
        .expect("reply sent");
    assert_eq!(got.as_deref(), Ok("one"));
    assert!(
        t.elapsed() < Duration::from_millis(300),
        "the text request waited for the repair: {:?}",
        t.elapsed()
    );
    // The repair itself still ends, with a report and a Settled marker.
    let report = tokio::time::timeout(Duration::from_secs(5), repaired)
        .await
        .expect("the repair answers")
        .expect("reply sent")
        .expect("repair ok");
    assert!(report.took >= Duration::from_millis(350), "{report:?}");
    assert!(!report.failed);
    let _ = until_settled(&mut b).await;
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn growth_without_events_stays_invisible_until_a_repair_finds_it() {
    // Chromium sends no children-changed:add while a page loads and nothing when the load ends:
    // the tree grows under a known node and the source is told nothing (step 0, CHR-0).
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let main = fake.node(raw::SECTION, "App", "main", None).await;
    fake.link(doc, main);
    let mut b = start_browse(fake, conn, doc).await;
    let _ = until_settled(&mut b).await;
    for i in 0..40 {
        let p = b
            .fake
            .node(raw::PARAGRAPH, "", "p", Some(&format!("p{i}")))
            .await;
        b.fake.set_text(p, &format!("paragraph {i}"));
        b.fake.link(main, p);
    }
    assert!(
        tokio::time::timeout(Duration::from_millis(400), b.rx.deltas.recv())
            .await
            .is_err(),
        "no event, so nothing is pushed"
    );
    let (reply, report) = gorca_rt::oneshot::channel();
    b.requests
        .send(DocRequest::Repair {
            cheap_check: false,
            reply,
        })
        .await
        .unwrap();
    let pushed = until_settled(&mut b).await;
    let report = report.await.unwrap().unwrap();
    assert!(report.nodes >= 42, "{report:?}");
    assert!(!report.failed);
    let paragraphs = upserts_of(&pushed)
        .iter()
        .filter(|n| n.role == Role::Paragraph)
        .count();
    assert_eq!(paragraphs, 40, "every grown paragraph reaches the mirror");
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn a_heavy_subtree_is_expanded_by_the_next_fetch_so_no_get_tree_covers_it_whole() {
    // CHR-0 B-3: a GetTree on a big subtree blocks the browser's main thread for its whole
    // length. The first fetch finds the heavy root; the next one splits it with GetChildren.
    let (fake, conn) = Fake::start(3).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "doc", "body", None).await;
    let big = fake.node(raw::SECTION, "big", "section", None).await;
    let small = fake.node(raw::SECTION, "small", "section", None).await;
    fake.link(doc, big);
    fake.link(doc, small);
    for i in 0..60 {
        let p = fake.node(raw::PARAGRAPH, &format!("p{i}"), "p", None).await;
        fake.link(big, p);
    }
    let sp = fake.node(raw::PARAGRAPH, "only", "p", None).await;
    fake.link(small, sp);
    let conn = Arc::new(conn);
    let interner = Arc::new(AttrInterner::default());
    let mut paths = PathTable::default();
    let mut opts = fast_opts(Split::Frontier {
        min_subtrees: 2,
        max_depth: 1,
    });
    opts.heavy_nodes = 30;
    let first = fetch_document(&conn, &interner, &mut paths, &path_of(doc), &opts)
        .await
        .unwrap();
    assert_eq!(first.timings.heavy_roots, vec![path_of(big)]);
    assert!(first.timings.heaviest_get_tree_nodes >= 61);

    opts.heavy = Arc::new(first.timings.heavy_roots.iter().cloned().collect());
    let second = fetch_document(&conn, &interner, &mut paths, &path_of(doc), &opts)
        .await
        .unwrap();
    assert!(
        second.timings.heaviest_get_tree_nodes <= 2,
        "the heavy section was fetched in pieces: {} nodes in the biggest call",
        second.timings.heaviest_get_tree_nodes
    );
    assert!(second.timings.heavy_roots.is_empty());
    // The same document either way.
    assert_eq!(first.block.nodes.len(), second.block.nodes.len());
}

#[tokio::test]
async fn incremental_updates_are_applied_while_a_repair_runs_and_survive_it() {
    // Review F-1: a repair used to freeze every event-driven update for its whole length.
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let p = fake.node(raw::PARAGRAPH, "", "p", None).await;
    fake.link(doc, p);
    fake.set_text(p, "one");
    let mut b = start_browse(fake, conn, doc).await;
    let _ = until_settled(&mut b).await;
    // The repair's role pass takes about 450 ms.
    b.fake
        .tree
        .lock()
        .unwrap()
        .nodes
        .get_mut(&p)
        .unwrap()
        .role_delay = Duration::from_millis(450);
    let (reply, repaired) = gorca_rt::oneshot::channel();
    b.requests
        .send(DocRequest::Repair {
            cheap_check: false,
            reply,
        })
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    // The page adds a paragraph and says so (an ordinary incremental update).
    let q = b.fake.node(raw::PARAGRAPH, "", "p", Some("q")).await;
    b.fake.set_text(q, "fresh");
    b.fake.link(doc, q);
    let t = Instant::now();
    b.events
        .send(DocEvent {
            ev: NodeEvent::ChildrenChanged {
                node: node_key(doc),
            },
            seq: 1,
            at: Instant::now(),
        })
        .await
        .unwrap();
    let mut fresh_id = None;
    let mut seen_after = None;
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end && fresh_id.is_none() {
        if let Ok(Some(DocDelta::Upsert(v))) =
            tokio::time::timeout(Duration::from_millis(20), b.rx.deltas.recv()).await
            && let Some(n) = v.iter().find(|n| n.text.as_deref() == Some("fresh"))
        {
            fresh_id = Some(n.id);
            seen_after = Some(t.elapsed());
        }
    }
    let seen_after = seen_after.expect("the incremental add reached the sink");
    assert!(
        seen_after < Duration::from_millis(250),
        "the update waited behind the repair: {seen_after:?}"
    );
    // The repair ends (its snapshot may predate the add) and the add is still there afterwards.
    let report = tokio::time::timeout(Duration::from_secs(5), repaired)
        .await
        .expect("the repair answers")
        .expect("reply sent")
        .expect("repair ok");
    assert!(!report.failed);
    let mut deltas = Vec::new();
    let end = Instant::now() + Duration::from_millis(1200);
    while Instant::now() < end {
        if let Ok(Some(d)) =
            tokio::time::timeout(Duration::from_millis(50), b.rx.deltas.recv()).await
        {
            deltas.push(d);
        }
    }
    let id = fresh_id.unwrap();
    let removed = deltas
        .iter()
        .any(|d| matches!(d, DocDelta::Removed(v) if v.contains(&id)));
    assert!(!removed, "the repair removed the node the update added");
    let last_children = deltas.iter().rev().find_map(|d| match d {
        DocDelta::Children { children, .. } if children.contains(&id) => Some(true),
        DocDelta::Children { .. } => Some(false),
        DocDelta::Reset { .. }
        | DocDelta::Upsert(_)
        | DocDelta::Removed(_)
        | DocDelta::Text { .. }
        | DocDelta::Hint(_)
        | DocDelta::Settled { .. } => None,
    });
    assert_ne!(last_children, Some(false), "a later list dropped the node");
    b.shutdown.cancel();
    let _ = b.task.await;
}

#[tokio::test]
async fn a_guessed_repair_is_answered_by_the_cheap_look_when_nothing_changed() {
    let (fake, conn) = Fake::start(2).await;
    let doc = fake.node(raw::DOCUMENT_WEB, "page", "body", None).await;
    let main = fake.node(raw::SECTION, "App", "main", None).await;
    fake.link(doc, main);
    let mut b = start_browse(fake, conn, doc).await;
    let _ = until_settled(&mut b).await;
    let ask = |b: &Browse, cheap: bool| {
        let (reply, rx) = gorca_rt::oneshot::channel();
        let tx = b.requests.clone();
        async move {
            tx.send(DocRequest::Repair {
                cheap_check: cheap,
                reply,
            })
            .await
            .unwrap();
            tokio::time::timeout(Duration::from_secs(5), rx)
                .await
                .expect("answer")
                .expect("sent")
                .expect("ok")
        }
    };
    // Nothing changed: answered without a refetch (no nodes fetched, no deltas pushed).
    let r = ask(&b, true).await;
    assert_eq!((r.nodes, r.deltas), (0, 0), "{r:?}");
    assert!(
        tokio::time::timeout(Duration::from_millis(150), b.rx.deltas.recv())
            .await
            .is_err()
    );
    // The empty main grows without an event: the look sees the child count and refetches.
    for i in 0..5 {
        let p = b
            .fake
            .node(raw::PARAGRAPH, "", "p", Some(&format!("g{i}")))
            .await;
        b.fake.set_text(p, "grown");
        b.fake.link(main, p);
    }
    let r = ask(&b, true).await;
    assert!(r.nodes >= 7, "the look missed the growth: {r:?}");
    let pushed = until_settled(&mut b).await;
    assert_eq!(
        upserts_of(&pushed)
            .iter()
            .filter(|n| n.role == Role::Paragraph)
            .count(),
        5
    );
    b.shutdown.cancel();
    let _ = b.task.await;
}
