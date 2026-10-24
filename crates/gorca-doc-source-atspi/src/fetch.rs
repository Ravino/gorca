// SPDX-License-Identifier: GPL-3.0-or-later
//! Block fetch of one document: `Collection.GetTree` for structure, names and attributes, then
//! pipelined `GetRole` / `GetState` / `GetText` passes over the same P2P pool.
//!
//! Strategy (ADR-0003 AS-1, AS-2; web source-layer report 2.1/2.2):
//!
//! 1. `GetTree(rule, [Name, ChildCount, Attributes])` returns the whole subtree as a flat DFS
//!    list without role or state. The server is the browser main thread, so one huge call also
//!    blocks the page. [`Split::Frontier`] expands the top of the tree with `GetChildren` and
//!    issues one `GetTree` per frontier node across the lanes: bounded main-thread slices, and the
//!    client-side decode runs on several cores.
//! 2. Role, state and text are methods, not properties, so they are separate passes with a
//!    window of in-flight calls per lane. The state pass can be limited to roles whose state
//!    matters for browsing ([`StatePolicy::Stateful`]); the rest get a documented default until a
//!    revalidation says otherwise.
//! 3. If the DFS list does not reconstruct into the expected tree (a node over
//!    `ATSPI_MAX_CHILDREN`, a `MANAGES_DESCENDANTS` node that was not descended) the call fails
//!    with [`FetchError::Shape`] and [`fallback_walk`] builds the block from `GetChildren`.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gorca_dbus::DbusError;
use gorca_doc_source_api::{Completeness, TreeBlock, web_attrs};
use gorca_model::{AttrKey, Role, SrcNode, SrcNodeId};
use gorca_rt::Deadline;
use smallvec::SmallVec;

use crate::p2p::AppConn;
use crate::roles;
use crate::wire::{self, AttrInterner, PathKey, RawNode, WireError};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COLLECTION: &str = "org.a11y.atspi.Collection";
const TEXT: &str = "org.a11y.atspi.Text";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error(transparent)]
    Dbus(#[from] DbusError),
    #[error(transparent)]
    Wire(#[from] WireError),
    #[error("tree shape not reconstructable: {0}")]
    Shape(&'static str),
    #[error("fetch deadline exceeded after {0:?}")]
    Deadline(Duration),
    #[error("root has no path key")]
    BadRoot,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Split {
    /// One `GetTree` on the root.
    Whole,
    /// Expand the top of the tree until at least this many subtrees exist, then one `GetTree`
    /// per subtree.
    Frontier { min_subtrees: usize, max_depth: u32 },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum StatePolicy {
    None,
    /// `GetState` only for roles where it matters ([`roles::is_stateful`]).
    Stateful,
    All,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TextPolicy {
    None,
    /// `Text.GetText(0, -1)` for every node whose role can carry text.
    All,
}

#[derive(Clone, Debug)]
pub struct FetchOpts {
    /// In-flight calls per lane.
    pub window: usize,
    pub split: Split,
    pub states: StatePolicy,
    pub text: TextPolicy,
    /// Whole-fetch deadline (Background class by default, section 6.4).
    pub budget: Duration,
    /// Per-call deadline inside a pass.
    pub call_timeout: Duration,
    /// Replies larger than this decode on the blocking pool.
    pub offload_bytes: usize,
    /// Activate Firefox's lazy cache domains before the first fetch (AS-3). Cheap once warm.
    pub warm: bool,
    /// Engine rows that normalise what a node says (M8b, ADR-0008).
    pub quirks: crate::extras::WebQuirks,
    /// Frontier leaves whose `GetTree` was heavy in an earlier fetch (more than [`HEAVY_NODES`]
    /// nodes): the browser's main thread is blocked for the whole call, so the next fetch
    /// expands them with `GetChildren` first (CHR-0 B-3). Empty on a first build.
    pub heavy: Arc<std::collections::HashSet<String>>,
    /// A `GetTree` reply of more nodes than this makes its root heavy (the product uses
    /// [`HEAVY_NODES`]).
    pub heavy_nodes: usize,
}

/// A `GetTree` reply of more nodes than this is a long slice of the browser's main thread
/// (about 10 microseconds per node on Chromium): its root is expanded next time.
pub const HEAVY_NODES: usize = 1500;
/// Extra frontier levels a heavy leaf may be expanded by.
const HEAVY_EXTRA_DEPTH: u32 = 6;

impl Default for FetchOpts {
    fn default() -> Self {
        Self {
            window: 32,
            split: Split::Frontier {
                min_subtrees: 8,
                max_depth: 3,
            },
            states: StatePolicy::Stateful,
            text: TextPolicy::None,
            budget: Duration::from_secs(20),
            call_timeout: Duration::from_secs(5),
            offload_bytes: 256 * 1024,
            warm: true,
            quirks: crate::extras::WebQuirks::NONE,
            heavy: Arc::default(),
            heavy_nodes: HEAVY_NODES,
        }
    }
}

/// Wall-clock cost of each stage, for the B-6 metrics.
#[derive(Clone, Debug, Default)]
pub struct FetchTimings {
    /// Heaviest single `GetTree` round trip of this fetch and its reply size: an upper bound of the
    /// time the browser's main thread was busy for one call (jank, WM-9).
    pub heaviest_get_tree: Duration,
    pub heaviest_get_tree_nodes: u64,
    /// Roots of the `GetTree` calls that returned more than [`HEAVY_NODES`] nodes.
    pub heavy_roots: Vec<String>,
    /// Cache-domain warming (first fetch of an app only).
    pub warm: Duration,
    pub frontier: Duration,
    pub get_tree: Duration,
    pub roles: Duration,
    pub states: Duration,
    pub text: Duration,
    pub assemble: Duration,
    pub total: Duration,
    pub calls: u64,
    pub tree_bytes: u64,
    pub state_calls: u64,
    pub text_calls: u64,
    pub failed_calls: u64,
}

/// Source-side bookkeeping for non-numeric paths.
#[derive(Clone, Debug, Default)]
pub struct PathTable {
    other: HashMap<u64, Box<str>>,
    by_str: HashMap<Box<str>, u64>,
    next: u64,
    /// `aria-errormessage` target -> the invalid fields that show its text. A change of the
    /// target's text re-reads those fields (M8b). Bounded by `MAX_ERROR_TARGETS`.
    error_targets: HashMap<PathKey, SmallVec<[PathKey; 2]>>,
}

/// Remembered error message targets (a long-lived SPA must not grow without bound).
const MAX_ERROR_TARGETS: usize = 1024;

impl PathTable {
    /// A copy a background fetch interns into while the live table keeps serving requests. Its
    /// side-table keys start far above the live ones so the two never hand out the same key.
    #[must_use]
    pub fn fork(&self) -> PathTable {
        let mut t = self.clone();
        t.next = self.next + (1 << 24);
        t
    }

    /// Take over what a forked table learned (non-numeric paths only; numeric paths need no
    /// table). A path the live table interned meanwhile under another key keeps that key.
    pub fn absorb(&mut self, other: PathTable) {
        for (k, p) in other.other {
            // The fork's key always resolves (the fetched nodes carry it); the live table's own
            // key for the same string stays the one `intern` hands out.
            if let std::collections::hash_map::Entry::Vacant(v) = self.by_str.entry(p.clone()) {
                v.insert(k);
            }
            self.other.insert(k, p);
        }
        // What the refetch learned about `aria-errormessage` targets (review F-2).
        for (target, fields) in other.error_targets {
            for f in fields {
                self.note_error_target(target, f);
            }
        }
        self.next = self.next.max(other.next);
    }

    pub fn intern(&mut self, path: &str) -> PathKey {
        if let Some(k) = PathKey::parse(path) {
            return k;
        }
        if let Some(k) = self.by_str.get(path) {
            return PathKey(*k);
        }
        self.next += 1;
        let k = wire::OTHER_PATH_BIT | self.next;
        self.other.insert(k, path.into());
        self.by_str.insert(path.into(), k);
        PathKey(k)
    }

    /// Remember that `field` shows the text of `target`.
    pub fn note_error_target(&mut self, target: PathKey, field: PathKey) {
        if self.error_targets.len() >= MAX_ERROR_TARGETS
            && !self.error_targets.contains_key(&target)
        {
            self.error_targets.clear();
        }
        let v = self.error_targets.entry(target).or_default();
        if !v.contains(&field) {
            v.push(field);
        }
    }

    /// The invalid fields that show the text of `target`.
    pub fn error_fields(&self, target: PathKey) -> &[PathKey] {
        self.error_targets.get(&target).map_or(&[], |v| &v[..])
    }

    pub fn path_of(&self, k: PathKey) -> Option<String> {
        k.to_path()
            .or_else(|| self.other.get(&k.0).map(std::string::ToString::to_string))
    }
}

/// A fetched document: the neutral block plus the transport facts later stages need.
#[derive(Debug)]
pub struct FetchedDoc {
    pub block: TreeBlock,
    /// Parallel to `block.nodes`.
    pub keys: Vec<PathKey>,
    pub raw_roles: Vec<u32>,
    pub child_counts: Vec<i32>,
    /// Indices into `block.nodes` where each independent tree starts (one for a document).
    pub roots: Vec<usize>,
    pub timings: FetchTimings,
    pub incomplete: bool,
}

impl FetchedDoc {
    /// A forest with no trees (nothing to add).
    pub fn empty() -> Self {
        Self {
            block: TreeBlock {
                root: SrcNodeId(0),
                completeness: Completeness::Roles,
                nodes: Vec::new(),
                children: Vec::new(),
            },
            keys: Vec::new(),
            raw_roles: Vec::new(),
            child_counts: Vec::new(),
            roots: Vec::new(),
            timings: FetchTimings::default(),
            incomplete: false,
        }
    }
}

/// Fetch the subtree under `root_path` of the app behind `conn`.
pub async fn fetch_document(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    root_path: &str,
    opts: &FetchOpts,
) -> Result<FetchedDoc, FetchError> {
    let t_start = Instant::now();
    let deadline = Deadline(t_start + opts.budget);
    let mut tm = FetchTimings::default();
    conn.reset_tree_stats();
    if opts.warm && !conn.is_warm() {
        let w =
            crate::warm::warm_domains(conn, interner, root_path, Duration::from_millis(2000)).await;
        tm.warm = w.waited;
        tm.calls += u64::from(w.probes);
    }
    let (nodes, child_counts) =
        fetch_structure(conn, interner, paths, root_path, opts, deadline, &mut tm).await?;
    let starts = vec![0usize];
    let mut doc = complete(
        conn,
        interner,
        paths,
        nodes,
        child_counts,
        starts,
        opts,
        deadline,
        tm,
        t_start,
    )
    .await?;
    let (call, nodes) = conn.tree_stats();
    doc.timings.heaviest_get_tree = call;
    doc.timings.heaviest_get_tree_nodes = nodes;
    Ok(doc)
}

/// Fetch several independent subtrees (for example the children a page just added) in one go:
/// one `GetTree` per root spread over the lanes, then shared role/state/text passes.
pub async fn fetch_forest(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    roots: &[String],
    opts: &FetchOpts,
) -> Result<FetchedDoc, FetchError> {
    let t_start = Instant::now();
    let deadline = Deadline(t_start + opts.budget);
    let mut tm = FetchTimings::default();
    let t = Instant::now();
    let by_root = trees_of(conn, interner, roots, opts, deadline, &mut tm).await?;
    tm.get_tree = t.elapsed();
    let mut nodes = Vec::new();
    let mut starts = Vec::new();
    for r in by_root.into_iter().flatten() {
        let mut r = r;
        fix_paths(&mut r.nodes, paths);
        starts.push(nodes.len());
        nodes.extend(r.nodes);
    }
    let counts = nodes.iter().map(|n| n.child_count).collect();
    complete(
        conn, interner, paths, nodes, counts, starts, opts, deadline, tm, t_start,
    )
    .await
}

/// Roles, states, text and assembly for already fetched structure. `starts` are the indices where
/// each independent tree begins.
#[allow(clippy::too_many_arguments)]
async fn complete(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    mut nodes: Vec<RawNode>,
    mut child_counts: Vec<i32>,
    starts: Vec<usize>,
    opts: &FetchOpts,
    deadline: Deadline,
    mut tm: FetchTimings,
    t_start: Instant,
) -> Result<FetchedDoc, FetchError> {
    let t_asm = Instant::now();
    let children = rebuild_forest(&nodes, &child_counts, &starts)?;
    tm.assemble += t_asm.elapsed();

    // 2. Roles.
    let n = nodes.len();
    let incomplete = Arc::new(AtomicBool::new(false));
    let mut raw_roles = vec![0u32; n];
    {
        let t = Instant::now();
        let targets: Arc<Vec<String>> = Arc::new(
            nodes
                .iter()
                .map(|nd| paths.path_of(nd.path).unwrap_or_default())
                .collect(),
        );
        let idx: Vec<usize> = (0..n).collect();
        let (res, failed) = pass(
            conn,
            &targets,
            idx,
            opts,
            deadline,
            &incomplete,
            ACCESSIBLE,
            "GetRole",
            None,
            |b| wire::parse_u32(b).ok(),
        )
        .await;
        tm.failed_calls += failed;
        for (i, r) in res {
            raw_roles[i] = r;
        }
        tm.roles = t.elapsed();
        tm.calls += n as u64;

        // 3. States.
        let mut states_raw: Vec<Option<u64>> = vec![None; n];
        let want: Vec<usize> = match opts.states {
            StatePolicy::None => Vec::new(),
            StatePolicy::All => (0..n).collect(),
            StatePolicy::Stateful => (0..n)
                .filter(|i| roles::is_stateful(raw_roles[*i]))
                .collect(),
        };
        let t = Instant::now();
        tm.state_calls = want.len() as u64;
        let (res, failed) = pass(
            conn,
            &targets,
            want,
            opts,
            deadline,
            &incomplete,
            ACCESSIBLE,
            "GetState",
            None,
            |b| wire::parse_state(b).ok(),
        )
        .await;
        tm.failed_calls += failed;
        for (i, s) in res {
            states_raw[i] = Some(s);
        }
        tm.states = t.elapsed();
        tm.calls += tm.state_calls;

        // 4. Text.
        let mut texts: Vec<Option<Arc<str>>> = vec![None; n];
        if opts.text == TextPolicy::All {
            let want: Vec<usize> = (0..n)
                .filter(|i| roles::may_have_text(raw_roles[*i]))
                .collect();
            tm.text_calls = want.len() as u64;
            let t = Instant::now();
            let (res, failed) = pass(
                conn,
                &targets,
                want,
                opts,
                deadline,
                &incomplete,
                TEXT,
                "GetText",
                Some((0i32, -1i32)),
                |b| wire::parse_string(b).ok().map(Arc::<str>::from),
            )
            .await;
            tm.failed_calls += failed;
            for (i, s) in res {
                if !s.is_empty() {
                    texts[i] = Some(s);
                }
            }
            tm.text = t.elapsed();
            tm.calls += tm.text_calls;
        }

        // 4b. The error message of invalid controls (M8b): a relation, never in `Description`.
        let mut error_text: HashMap<usize, Arc<str>> = HashMap::new();
        {
            let invalid = 1u64 << roles::state_bit::INVALID_ENTRY;
            let want: Vec<usize> = (0..n)
                .filter(|i| {
                    states_raw[*i].is_some_and(|s| s & invalid != 0)
                        && crate::extras::takes_error_text(roles::map_role(raw_roles[*i]))
                })
                .take(MAX_ERROR_TEXTS)
                .collect();
            if !want.is_empty() {
                let want_paths: Vec<String> = want.iter().map(|i| targets[*i].clone()).collect();
                let res = crate::extras::error_texts(conn, want_paths, opts.quirks, deadline).await;
                tm.calls += want.len() as u64 * 2;
                for (i, t) in want.into_iter().zip(res) {
                    if let Some((target, t)) = t {
                        let tk = paths.intern(&target);
                        paths.note_error_target(tk, nodes[i].path);
                        error_text.insert(i, t);
                    }
                }
            }
        }

        // 5. Assemble neutral nodes.
        let t_asm = Instant::now();
        let mut out_nodes = Vec::with_capacity(n);
        let mut keys = Vec::with_capacity(n);
        let key_names = crate::norm::KeyNames::of(interner);
        for (i, rn) in nodes.drain(..).enumerate() {
            let rr = raw_roles[i];
            let role = roles::map_role(rr);
            let mut attrs: SmallVec<[(AttrKey, Arc<str>); 4]> = rn.attrs;
            // The message of an invalid control is an attribute before the normaliser runs, so the
            // fused entry carries it (the browse presenter reads it from there).
            if let Some(t) = error_text.remove(&i) {
                attrs.push((web_attrs::ERROR_TEXT, t));
            }
            // The normaliser sees what the engine said: the raw name (before the engine rows of
            // `WebQuirks`, which the S-1 quirk rows replace) and the attributes as fetched.
            let rec = crate::norm::record_interned(
                &key_names,
                rr,
                states_raw[i],
                rn.name.as_deref(),
                &attrs,
                &rn.aliased,
            );
            if role == Role::Unknown {
                attrs.push((web_attrs::RAW_ROLE, Arc::from(rr.to_string().as_str())));
            }
            let states = match states_raw[i] {
                Some(s) => opts.quirks.states(role, roles::map_web_states(s)),
                None => roles::DEFAULT_PLAIN_STATES,
            };
            let authored = attrs
                .iter()
                .any(|(k, v)| *k == web_attrs::EXPLICIT_NAME && &**v == "true");
            let name = opts.quirks.name(role, authored, rn.name);
            keys.push(rn.path);
            out_nodes.push(SrcNode {
                id: SrcNodeId(rn.path.0),
                role,
                states,
                name,
                text: texts[i].take(),
                attrs,
                local: targets.get(i).map(|p| gorca_dbus::drain::local_id(p)),
                rec: Some(rec),
            });
        }
        let completeness = match (opts.states, opts.text) {
            (_, TextPolicy::All) => Completeness::Text,
            (StatePolicy::None, _) => Completeness::Roles,
            _ => Completeness::States,
        };
        let root = out_nodes.first().map(|x| x.id).ok_or(FetchError::BadRoot)?;
        let roots = starts.clone();
        let child_lists: Vec<(SrcNodeId, SmallVec<[SrcNodeId; 8]>)> = children
            .into_iter()
            .map(|(p, cs)| {
                (
                    SrcNodeId(keys[p].0),
                    cs.into_iter().map(|c| SrcNodeId(keys[c].0)).collect(),
                )
            })
            .collect();
        tm.assemble += t_asm.elapsed();
        tm.total = t_start.elapsed();
        Ok(FetchedDoc {
            block: TreeBlock {
                root,
                completeness,
                nodes: out_nodes,
                children: child_lists,
            },
            keys,
            raw_roles,
            child_counts: std::mem::take(&mut child_counts),
            roots,
            timings: tm,
            incomplete: incomplete.load(Ordering::Relaxed),
        })
    }
}

/// Most invalid controls whose error text one fetch reads (a form of hundreds of invalid
/// fields must not turn into hundreds of round trips).
const MAX_ERROR_TEXTS: usize = 64;

type Slot<T> = (usize, T);

/// One pipelined pass: `idx` selects the targets; `L * window` workers pull from a shared cursor.
#[allow(clippy::too_many_arguments)]
async fn pass<T, P>(
    conn: &Arc<AppConn>,
    targets: &Arc<Vec<String>>,
    idx: Vec<usize>,
    opts: &FetchOpts,
    deadline: Deadline,
    incomplete: &Arc<AtomicBool>,
    iface: &'static str,
    member: &'static str,
    args: Option<(i32, i32)>,
    parse: P,
) -> (Vec<Slot<T>>, u64)
where
    T: Send + 'static,
    P: Fn(&[u8]) -> Option<T> + Send + Sync + Copy + 'static,
{
    if idx.is_empty() {
        return (Vec::new(), 0);
    }
    let idx = Arc::new(idx);
    let cursor = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicU64::new(0));
    let lanes = conn.lanes();
    let mut tasks = Vec::with_capacity(lanes * opts.window);
    for lane in 0..lanes {
        for _ in 0..opts.window.max(1) {
            let conn = Arc::clone(conn);
            let targets = Arc::clone(targets);
            let idx = Arc::clone(&idx);
            let cursor = Arc::clone(&cursor);
            let incomplete = Arc::clone(incomplete);
            let failed = Arc::clone(&failed);
            let call_timeout = opts.call_timeout;
            tasks.push(tokio::spawn(async move {
                let mut local: Vec<Slot<T>> = Vec::new();
                loop {
                    let k = cursor.fetch_add(1, Ordering::Relaxed);
                    let Some(&i) = idx.get(k) else { break };
                    let now = Instant::now();
                    if deadline.expired(now) {
                        incomplete.store(true, Ordering::Relaxed);
                        break;
                    }
                    let dl = Deadline((now + call_timeout).min(deadline.0));
                    let path = &targets[i];
                    let r = match args {
                        Some(a) => conn.call(lane, dl, path, iface, member, &a).await,
                        None => conn.call(lane, dl, path, iface, member, &()).await,
                    };
                    match r {
                        Ok(m) => match parse(m.body().data().bytes()) {
                            Some(v) => local.push((i, v)),
                            None => {
                                failed.fetch_add(1, Ordering::Relaxed);
                            }
                        },
                        Err(_) => {
                            failed.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                local
            }));
        }
    }
    let mut out = Vec::with_capacity(idx.len());
    for t in tasks {
        if let Ok(v) = t.await {
            out.extend(v);
        }
    }
    (out, failed.load(Ordering::Relaxed))
}

/// The empty `MatchRule` `(aiia{ss}iaiiasib)` that `GetTree` reads and ignores.
pub type EmptyRule = (
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

/// Body of a `GetTree` call: the rule and the properties to return.
pub type TreeArgs = (EmptyRule, Vec<&'static str>);

pub fn tree_args() -> TreeArgs {
    // Empty match rule (the server reads but ignores it for GetTree) and the properties we want.
    (
        (
            Vec::new(),
            0,
            HashMap::new(),
            0,
            Vec::new(),
            0,
            Vec::new(),
            0,
            false,
        ),
        vec!["Name", "ChildCount", "Attributes", "Description"],
    )
}

pub async fn get_tree_raw(
    conn: &AppConn,
    lane: usize,
    path: &str,
    deadline: Deadline,
) -> Result<zbus::Message, DbusError> {
    let t = Instant::now();
    let r = conn
        .call(lane, deadline, path, COLLECTION, "GetTree", &tree_args())
        .await;
    conn.note_tree_call(t.elapsed());
    r
}

async fn decode(
    msg: zbus::Message,
    interner: &Arc<AttrInterner>,
    offload: usize,
) -> Result<(wire::TreeReply, u64), FetchError> {
    let len = msg.body().data().bytes().len();
    let interner = Arc::clone(interner);
    if len >= offload {
        let r = tokio::task::spawn_blocking(move || {
            wire::parse_get_tree(msg.body().data().bytes(), &interner)
        })
        .await
        .map_err(|_| FetchError::Shape("decode task failed"))??;
        Ok((r, len as u64))
    } else {
        Ok((
            wire::parse_get_tree(msg.body().data().bytes(), &interner)?,
            len as u64,
        ))
    }
}

#[allow(clippy::too_many_lines)]
async fn fetch_structure(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    root_path: &str,
    opts: &FetchOpts,
    deadline: Deadline,
    tm: &mut FetchTimings,
) -> Result<(Vec<RawNode>, Vec<i32>), FetchError> {
    let call_dl = |now: Instant| Deadline((now + opts.call_timeout).min(deadline.0));
    match opts.split {
        Split::Whole => {
            let t = Instant::now();
            let msg = get_tree_raw(conn, 0, root_path, call_dl(t)).await?;
            let (mut reply, bytes) = decode(msg, interner, opts.offload_bytes).await?;
            tm.get_tree = t.elapsed();
            tm.tree_bytes = bytes;
            tm.calls += 1;
            conn.note_tree_nodes(reply.nodes.len());
            fix_paths(&mut reply.nodes, paths);
            let counts = reply.nodes.iter().map(|n| n.child_count).collect();
            Ok((reply.nodes, counts))
        }
        Split::Frontier {
            min_subtrees,
            max_depth,
        } => {
            let spec = FrontierSpec {
                min_subtrees,
                max_depth,
                call_timeout: opts.call_timeout,
                deadline,
                heavy: &opts.heavy,
            };
            let plan = plan_frontier(conn, interner, paths, root_path, &spec, tm).await?;
            let t = Instant::now();
            let mut by_root = trees_of(conn, interner, &plan.leaves, opts, deadline, tm).await?;
            tm.get_tree = t.elapsed();
            // Stitch in DFS order: walk the frontier tree.
            let mut out: Vec<RawNode> = Vec::new();
            let child_map: HashMap<&str, &Vec<String>> =
                plan.interior.iter().map(|(p, k)| (p.as_str(), k)).collect();
            let mut node_of: HashMap<&str, usize> = HashMap::new();
            for (i, (p, _)) in plan.interior.iter().enumerate() {
                node_of.insert(p.as_str(), i);
            }
            let root_index: HashMap<&str, usize> = plan
                .leaves
                .iter()
                .enumerate()
                .map(|(i, p)| (p.as_str(), i))
                .collect();
            let mut stack: Vec<&str> = vec![root_path];
            while let Some(p) = stack.pop() {
                if let Some(kids) = child_map.get(p) {
                    if let Some(&i) = node_of.get(p) {
                        out.push(plan.interior_nodes[i].clone());
                    }
                    for c in kids.iter().rev() {
                        stack.push(c.as_str());
                    }
                } else if let Some(&ri) = root_index.get(p)
                    && let Some(mut r) = by_root[ri].take()
                {
                    fix_paths(&mut r.nodes, paths);
                    out.extend(r.nodes);
                }
            }
            let counts = out.iter().map(|n| n.child_count).collect();
            Ok((out, counts))
        }
    }
}

/// The top of a document expanded with `GetChildren`: interior nodes (their own properties and the
/// complete ordered list of their children) and the frontier leaves, in document order, each of
/// which is fetched with one `GetTree`.
#[derive(Clone, Debug)]
pub struct FrontierPlan {
    /// `(path, child paths)` of every expanded node, root first.
    pub interior: Vec<(String, Vec<String>)>,
    /// The properties of each interior node, parallel to `interior`.
    pub interior_nodes: Vec<RawNode>,
    /// Frontier leaves in document order.
    pub leaves: Vec<String>,
}

/// How [`plan_frontier`] expands the top of a document.
#[derive(Copy, Clone, Debug)]
pub struct FrontierSpec<'a> {
    /// Expand every frontier node while fewer subtrees than this exist...
    pub min_subtrees: usize,
    /// ... and at most this many levels.
    pub max_depth: u32,
    pub call_timeout: Duration,
    pub deadline: Deadline,
    /// Leaves an earlier fetch found heavy: expanded a few levels further (B-3).
    pub heavy: &'a Arc<std::collections::HashSet<String>>,
}

/// Expand the tree from `root_path` until at least `limits.0` subtrees exist or `limits.1` levels
/// were expanded, and read the properties of the expanded nodes.
pub async fn plan_frontier(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    root_path: &str,
    spec: &FrontierSpec<'_>,
    tm: &mut FetchTimings,
) -> Result<FrontierPlan, FetchError> {
    let FrontierSpec {
        min_subtrees,
        max_depth,
        call_timeout,
        deadline,
        heavy,
    } = *spec;
    let call_dl = |now: Instant| Deadline((now + call_timeout).min(deadline.0));
    let t = Instant::now();
    let mut interior: Vec<(String, Vec<String>)> = Vec::new();
    let mut frontier: Vec<String> = vec![root_path.to_owned()];
    let mut depth = 0;
    loop {
        // Expand every frontier node while the frontier is narrow; afterwards only the leaves
        // an earlier fetch found heavy (B-3), a few extra levels deep.
        let widen = frontier.len() < min_subtrees && depth < max_depth;
        let deepen =
            depth < max_depth + HEAVY_EXTRA_DEPTH && frontier.iter().any(|p| heavy.contains(p));
        if !widen && !deepen {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        let mut expanded_any = false;
        let mut replies: Vec<Option<Vec<(String, String)>>> = Vec::new();
        for (k, p) in frontier.iter().enumerate() {
            if !(widen || heavy.contains(p)) {
                replies.push(None);
                continue;
            }
            let m = conn
                .call(
                    k,
                    call_dl(Instant::now()),
                    p,
                    ACCESSIBLE,
                    "GetChildren",
                    &(),
                )
                .await?;
            tm.calls += 1;
            replies.push(Some(wire::parse_children(m.body().data().bytes())?));
        }
        for (p, kids) in frontier.iter().zip(replies) {
            match kids {
                Some(kids) if !kids.is_empty() => {
                    expanded_any = true;
                    let kp: Vec<String> = kids.into_iter().map(|(_, c)| c).collect();
                    interior.push((p.clone(), kp.clone()));
                    next.extend(kp);
                }
                Some(_) | None => next.push(p.clone()),
            }
        }
        frontier = next;
        depth += 1;
        if !expanded_any {
            break;
        }
    }
    tm.frontier = t.elapsed();
    let expanded: std::collections::HashSet<&str> =
        interior.iter().map(|(p, _)| p.as_str()).collect();
    let leaves: Vec<String> = frontier
        .iter()
        .filter(|p| !expanded.contains(p.as_str()))
        .cloned()
        .collect();
    // Interior node properties: Name, attributes (ChildCount is the list length).
    let mut interior_nodes = Vec::with_capacity(interior.len());
    for (k, (p, kids)) in interior.iter().enumerate() {
        let dl = call_dl(Instant::now());
        let name = conn
            .call(k, dl, p, PROPERTIES, "Get", &(ACCESSIBLE, "Name"))
            .await
            .ok()
            .and_then(|m| {
                wire::parse_variant_string(m.body().data().bytes())
                    .ok()
                    .map(str::to_owned)
            });
        let attrs = conn
            .call(k, dl, p, ACCESSIBLE, "GetAttributes", &())
            .await
            .ok()
            .and_then(|m| wire::parse_string_map(m.body().data().bytes()).ok())
            .unwrap_or_default();
        tm.calls += 2;
        let mut rn = RawNode {
            path: paths.intern(p),
            bus: 0,
            name: name
                .filter(|s| !s.is_empty())
                .map(|s| Arc::from(s.as_str())),
            child_count: i32::try_from(kids.len()).unwrap_or(i32::MAX),
            attrs: SmallVec::new(),
            other_path: None,
            aliased: SmallVec::new(),
        };
        for (ak, av) in attrs {
            let val: Arc<str> = Arc::from(av.as_str());
            if let Some(exact) = wire::alias_name(&ak) {
                rn.aliased.push((exact, Arc::clone(&val)));
            }
            rn.attrs.push((interner.key(&ak), val));
        }
        interior_nodes.push(rn);
    }
    Ok(FrontierPlan {
        interior,
        interior_nodes,
        leaves,
    })
}

/// One `GetTree` per root, spread over the lanes; replies decoded (large ones on the blocking
/// pool). The result is indexed like `roots`.
async fn trees_of(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    roots: &[String],
    opts: &FetchOpts,
    deadline: Deadline,
    tm: &mut FetchTimings,
) -> Result<Vec<Option<wire::TreeReply>>, FetchError> {
    let cursor = Arc::new(AtomicUsize::new(0));
    let roots_arc: Arc<Vec<String>> = Arc::new(roots.to_vec());
    let mut tasks = Vec::new();
    for lane in 0..conn.lanes() {
        let conn = Arc::clone(conn);
        let interner = Arc::clone(interner);
        let cursor = Arc::clone(&cursor);
        let roots = Arc::clone(&roots_arc);
        let (call_timeout, offload) = (opts.call_timeout, opts.offload_bytes);
        tasks.push(tokio::spawn(async move {
            let mut got = Vec::new();
            loop {
                let k = cursor.fetch_add(1, Ordering::Relaxed);
                let Some(p) = roots.get(k) else { break };
                let dl = Deadline((Instant::now() + call_timeout).min(deadline.0));
                let msg = get_tree_raw(&conn, lane, p, dl).await?;
                let (r, b) = decode(msg, &interner, offload).await?;
                conn.note_tree_nodes(r.nodes.len());
                got.push((k, r, b));
            }
            Ok::<_, FetchError>(got)
        }));
    }
    let mut by_root: Vec<Option<wire::TreeReply>> = (0..roots.len()).map(|_| None).collect();
    for t in tasks {
        for (k, r, b) in t
            .await
            .map_err(|_| FetchError::Shape("tree task failed"))??
        {
            tm.tree_bytes += b;
            tm.calls += 1;
            if r.nodes.len() > opts.heavy_nodes {
                tm.heavy_roots.push(roots[k].clone());
            }
            by_root[k] = Some(r);
        }
    }
    Ok(by_root)
}

/// First block of a progressive build (ADR-0003 section 8, viewport first): the interior of the
/// top of the document plus as many leading subtrees as it takes to reach `first_nodes` nodes
/// (the top of the page is the first viewport of a fresh load), complete with roles and states.
/// The children lists of the interior nodes are complete (SL-4) and name nodes that are not
/// fetched yet; the rest comes from [`fetch_forest`] over `plan.leaves[consumed..]`.
pub async fn fetch_first_block(
    conn: &Arc<AppConn>,
    interner: &Arc<AttrInterner>,
    paths: &mut PathTable,
    root_path: &str,
    opts: &FetchOpts,
    first_nodes: usize,
) -> Result<(FetchedDoc, FrontierPlan, usize), FetchError> {
    let t_start = Instant::now();
    let deadline = Deadline(t_start + opts.budget);
    let mut tm = FetchTimings::default();
    conn.reset_tree_stats();
    if opts.warm && !conn.is_warm() {
        let w =
            crate::warm::warm_domains(conn, interner, root_path, Duration::from_millis(2000)).await;
        tm.warm = w.waited;
        tm.calls += u64::from(w.probes);
    }
    let (min_subtrees, max_depth) = match opts.split {
        Split::Frontier {
            min_subtrees,
            max_depth,
        } => (min_subtrees, max_depth),
        Split::Whole => (8, 3),
    };
    let spec = FrontierSpec {
        min_subtrees,
        max_depth,
        call_timeout: opts.call_timeout,
        deadline,
        heavy: &opts.heavy,
    };
    let plan = plan_frontier(conn, interner, paths, root_path, &spec, &mut tm).await?;
    let t = Instant::now();
    let mut by_root: Vec<Option<wire::TreeReply>> = Vec::new();
    let mut have = plan.interior_nodes.len();
    let batch = conn.lanes().max(2);
    while have < first_nodes && by_root.len() < plan.leaves.len() {
        let from = by_root.len();
        let to = (from + batch).min(plan.leaves.len());
        let got = trees_of(
            conn,
            interner,
            &plan.leaves[from..to],
            opts,
            deadline,
            &mut tm,
        )
        .await?;
        have += got.iter().flatten().map(|r| r.nodes.len()).sum::<usize>();
        by_root.extend(got);
    }
    let take = by_root.len();
    tm.get_tree = t.elapsed();

    // A forest: every interior node as a one-node tree, then the fetched subtrees.
    let mut nodes: Vec<RawNode> = Vec::new();
    let mut counts: Vec<i32> = Vec::new();
    let mut starts: Vec<usize> = Vec::new();
    for n in &plan.interior_nodes {
        starts.push(nodes.len());
        nodes.push(n.clone());
        counts.push(0);
    }
    for r in by_root.into_iter().flatten() {
        let mut r = r;
        fix_paths(&mut r.nodes, paths);
        starts.push(nodes.len());
        counts.extend(r.nodes.iter().map(|x| x.child_count));
        nodes.extend(r.nodes);
    }
    if nodes.is_empty() {
        return Err(FetchError::BadRoot);
    }
    let mut doc = complete(
        conn, interner, paths, nodes, counts, starts, opts, deadline, tm, t_start,
    )
    .await?;
    // Complete child lists of the interior nodes (they reference nodes of later blocks too).
    let mut lists = Vec::with_capacity(plan.interior.len());
    for (p, kids) in &plan.interior {
        let parent = SrcNodeId(paths.intern(p).0);
        let ids: SmallVec<[SrcNodeId; 8]> =
            kids.iter().map(|k| SrcNodeId(paths.intern(k).0)).collect();
        lists.push((parent, ids));
    }
    lists.append(&mut doc.block.children);
    doc.block.children = lists;
    doc.block.root = SrcNodeId(paths.intern(root_path).0);
    doc.timings.heaviest_get_tree = conn.tree_stats().0;
    doc.timings.heaviest_get_tree_nodes = conn.tree_stats().1;
    Ok((doc, plan, take))
}

fn fix_paths(nodes: &mut [RawNode], paths: &mut PathTable) {
    for n in nodes.iter_mut() {
        if let Some(p) = n.other_path.take() {
            n.path = paths.intern(&p);
        }
    }
}

/// [`rebuild_children`] over several independent trees.
fn rebuild_forest(
    nodes: &[RawNode],
    counts: &[i32],
    starts: &[usize],
) -> Result<Vec<(usize, Vec<usize>)>, FetchError> {
    let mut out = Vec::new();
    for (k, &s) in starts.iter().enumerate() {
        let e = starts.get(k + 1).copied().unwrap_or(nodes.len());
        for (p, cs) in rebuild_children(&nodes[s..e], &counts[s..e])? {
            out.push((p + s, cs.into_iter().map(|c| c + s).collect()));
        }
    }
    Ok(out)
}

/// Reconstruct `(parent index, child indices)` from the DFS order and `ChildCount`. Fails when the
/// counts do not describe the list exactly.
fn rebuild_children(
    nodes: &[RawNode],
    counts: &[i32],
) -> Result<Vec<(usize, Vec<usize>)>, FetchError> {
    let mut out: Vec<(usize, Vec<usize>)> = Vec::new();
    // stack of (index into `out`, remaining)
    let mut stack: Vec<(usize, i32)> = Vec::new();
    for (i, _) in nodes.iter().enumerate() {
        if i > 0 {
            while let Some(&(_, rem)) = stack.last() {
                if rem == 0 {
                    stack.pop();
                } else {
                    break;
                }
            }
            let Some(top) = stack.last_mut() else {
                return Err(FetchError::Shape("more nodes than ChildCount allows"));
            };
            top.1 -= 1;
            out[top.0].1.push(i);
        }
        let c = counts[i];
        if c > 0 {
            out.push((
                i,
                Vec::with_capacity(usize::try_from(c.min(1024)).unwrap_or(0)),
            ));
            stack.push((out.len() - 1, c));
        }
    }
    if stack.iter().any(|&(_, rem)| rem != 0) {
        return Err(FetchError::Shape(
            "ChildCount larger than the nodes returned",
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rn(cc: i32) -> RawNode {
        RawNode {
            path: PathKey(1),
            bus: 0,
            name: None,
            child_count: cc,
            attrs: SmallVec::new(),
            other_path: None,
            aliased: SmallVec::new(),
        }
    }

    #[test]
    fn rebuild_dfs_children() {
        // 0(2) -> 1(1) -> 2(0) ; 3(0)
        let nodes = vec![rn(2), rn(1), rn(0), rn(0)];
        let counts: Vec<i32> = nodes.iter().map(|n| n.child_count).collect();
        let c = rebuild_children(&nodes, &counts).unwrap();
        assert_eq!(c, vec![(0, vec![1, 3]), (1, vec![2])]);
    }

    #[test]
    fn rebuild_detects_capped_or_undescended_nodes() {
        // Node 0 claims 3 children but only 1 follows.
        let nodes = vec![rn(3), rn(0)];
        let counts: Vec<i32> = nodes.iter().map(|n| n.child_count).collect();
        assert!(rebuild_children(&nodes, &counts).is_err());
        // More nodes than counts allow.
        let nodes = vec![rn(0), rn(0)];
        let counts = vec![0, 0];
        assert!(rebuild_children(&nodes, &counts).is_err());
    }

    #[test]
    fn absorb_keeps_error_targets_and_resolves_the_forks_keys() {
        let mut live = PathTable::default();
        let mut fork = live.fork();
        // The background fetch learns an aria-errormessage target.
        fork.note_error_target(PathKey(70), PathKey(80));
        // Meanwhile the live table interns the same odd path the fork also interned.
        let kf = fork.intern("/odd/path");
        let kl = live.intern("/odd/path");
        assert_ne!(kf, kl);
        live.absorb(fork);
        assert_eq!(live.error_fields(PathKey(70)), &[PathKey(80)]);
        // The fetched nodes carry the fork's key: it still names the path.
        assert_eq!(live.path_of(kf).as_deref(), Some("/odd/path"));
        assert_eq!(live.path_of(kl).as_deref(), Some("/odd/path"));
        assert_eq!(
            live.intern("/odd/path"),
            kl,
            "the live key stays the one handed out"
        );
    }

    #[test]
    fn error_targets_map_to_their_fields_and_stay_bounded() {
        let mut t = PathTable::default();
        let (target, f1, f2) = (PathKey(7), PathKey(8), PathKey(9));
        t.note_error_target(target, f1);
        t.note_error_target(target, f1);
        t.note_error_target(target, f2);
        assert_eq!(t.error_fields(target), [f1, f2]);
        assert!(t.error_fields(PathKey(1)).is_empty());
        for i in 0..(MAX_ERROR_TARGETS as u64 + 5) {
            t.note_error_target(PathKey(1000 + i), f1);
        }
        assert!(t.error_targets.len() <= MAX_ERROR_TARGETS);
    }

    #[test]
    fn path_table_interns_odd_paths_once() {
        let mut t = PathTable::default();
        let a = t.intern("/x/y");
        let b = t.intern("/x/y");
        assert_eq!(a, b);
        assert_eq!(t.path_of(a).as_deref(), Some("/x/y"));
        assert_eq!(t.intern("/org/a11y/atspi/accessible/5"), PathKey(6));
    }
}
