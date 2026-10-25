// SPDX-License-Identifier: GPL-3.0-or-later
//! The plugin port of the fusion writers (ADR-0026 section 2.4.6, M18a): how an app actor or a
//! document actor hands `derive` jobs to plugin actors and admits their facts.
//!
//! The core names no WASM runtime: [`PluginService`] and [`ScopeRunner`] are implemented by the
//! daemon over `gorca-plugin-host`. Everything that decides *what* may become a fact lives here,
//! in the node's single writer (SM-8):
//!
//! * **PF-2 S0-only views.** The rows a plugin can read ([`Row`]) are built by the writer from
//!   the primary's observed record, never from a fused entry, so no plugin sees any plugin's
//!   patch, hide or category (and a W-lane read cannot depend on another plugin's output).
//! * **PF-4 trigger rule.** A job is created only by a change of the S0 projection of a node
//!   ([`PluginBridge::upsert`]), by a new `[[watch]]` match, by attach, or by the first focus of
//!   a node that has no live fact. Admitting a fact enqueues nothing, so a plugin's output can
//!   never re-trigger it.
//! * **PF-7 / MQ-2 revision token.** Every reply carries `(t, R)`: the writer turn of the
//!   snapshot and the set of nodes the call read. The facts are admitted only if no node of `R`
//!   changed after `t`; otherwise they are dropped and the targets re-derived. A live token goes
//!   stale (its facts are retracted and the target re-derived) when a node of its `R` changes.
//! * **One wait per keypress path.** [`Governor`] lets exactly one writer of a keypress path take
//!   the bounded wait; the other sends its jobs and publishes without waiting.
//! * **Stratum by tier.** A computed patch of a first-party plugin is S1 and plain; of a
//!   user-trusted third party S1 and tainted; everything else, and every `inferred` fact, is S4
//!   and hedged (ADR-0026 section 2.6).
//!
//! Nothing here blocks except the single bounded wait inside [`ScopeRunner::interactive`].

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gorca_fuse::{AnchorId, FactIn, Grant, Input};
use gorca_fuse::{Ivm, NodeIdx, PatchSource, Payload, Quirks, SourceInfo, SourceState, Val, Write};
use gorca_model::fact::{Basis, SourceId};
use gorca_model::sem::{Group, GroupClass, GroupCode, SemRole, SourceClass, TrustTier};

/// The shared interactive deadline measured from the start of the writer turn (ADR-0026 r4).
pub const INTERACTIVE_WINDOW_NS: u64 = 300_000;
/// Writer turns within this window of a wait belong to the same keypress path.
pub const PATH_WINDOW_NS: u64 = 2_000_000;
/// A writer takes an interactive wait at most once per this long (flapping focus).
pub const WAIT_EVERY_NS: u64 = 20_000_000;
/// Focus-chain nodes considered for an interactive handoff.
pub const MAX_INTERACTIVE_NODES: usize = 16;
/// Nodes per background job.
pub const MAX_BATCH: usize = 64;
/// Dirty nodes held per plugin; beyond it the oldest are dropped (counted).
pub const MAX_PENDING: usize = 4096;
/// Depth and size of the subtree snapshot taken for a target (the import bounds).
pub const SNAPSHOT_DEPTH: usize = 8;
/// Nodes of a target's subtree in a snapshot.
pub const SNAPSHOT_NODES: usize = 256;
/// Ancestors included in a snapshot.
pub const SNAPSHOT_ANCESTORS: usize = 8;

/// Cause codes handed to `derive`.
pub mod cause {
    /// First derivation of a watched node.
    pub const ATTACH: u8 = 0;
    /// The node (or a node it read) changed.
    pub const CHANGED: u8 = 1;
    /// The node holds the focus and has no live fact.
    pub const FOCUS: u8 = 2;
}

// ------------------------------------------------------------------------------------------
// Scope, rows, snapshots
// ------------------------------------------------------------------------------------------

/// What a plugin scope is (one application, one document of it).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScopeSpec {
    /// The core's application identity.
    pub app: Arc<str>,
    /// The document URL, set only for a document scope and only when a loaded manifest has a URL
    /// glob (privacy).
    pub url: Option<Arc<str>>,
    /// Engine registry code.
    pub engine: u16,
    /// The identity was verified from the peer's credentials ([`scope_identity`]).
    pub verified: bool,
    /// A document scope (one web document, K2 plugins); else an application scope (K1).
    pub doc: bool,
}

/// The S0 projection of one node a plugin may read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// `SemRole` code of the observed role.
    pub role: u16,
    /// State bits.
    pub states: u64,
    /// Observed accessible name.
    pub name: Option<Arc<str>>,
    /// Cached text content.
    pub text: Option<Arc<str>>,
    /// Neutral attribute `tag`.
    pub tag: Option<Arc<str>>,
    /// Neutral attribute `class`.
    pub class: Option<Arc<str>>,
    /// Neutral attribute `aria-role`.
    pub aria_role: Option<Arc<str>>,
    /// Neutral attribute `id`.
    pub dom_id: Option<Arc<str>>,
    /// A secret node (password): absent from every read (AC-1, SS-3).
    pub secret: bool,
}

impl Row {
    /// True when the node is a secret one by its role.
    pub fn is_secret_role(role: SemRole) -> bool {
        role == SemRole::PasswordBox
    }

    /// The neutral attribute `key` (`tag`, `class`, `aria-role`, `id`).
    pub fn attr(&self, key: &str) -> Option<&str> {
        match key {
            "tag" => self.tag.as_deref(),
            "class" => self.class.as_deref(),
            "aria-role" => self.aria_role.as_deref(),
            "id" => self.dom_id.as_deref(),
            _ => None,
        }
    }
}

/// One row in a snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenRow {
    /// The S0 row.
    pub row: Row,
    /// Slot generation (a reused slot is a different node).
    pub generation: u16,
    /// Parent index.
    pub parent: Option<u32>,
    /// Child indices in order.
    pub children: Vec<u32>,
}

/// The immutable snapshot a job reads (`FrozenRows`, ADR-0026 section 2.4.6).
#[derive(Clone, Debug, Default)]
pub struct FrozenRows {
    /// The scope.
    pub scope: ScopeSpec,
    /// Rows by node index.
    pub rows: HashMap<u32, FrozenRow>,
}

/// The tree structure of the writer's engine (S0 structure, PF-2).
pub trait Tree {
    /// Parent of `n`.
    fn parent(&self, n: u32) -> Option<u32>;
    /// Children of `n` in order.
    fn children(&self, n: u32) -> Vec<u32>;
}

// ------------------------------------------------------------------------------------------
// Runner contract
// ------------------------------------------------------------------------------------------

/// Trust of a plugin as the writer needs it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PluginTier {
    /// T2: built from this repository and pinned.
    FirstParty,
    /// A third party the user marked trusted.
    TrustedThirdParty,
    /// A third party (or a plugin under development).
    Untrusted,
}

/// A plugin of a scope.
#[derive(Clone, Debug)]
pub struct PluginMeta {
    /// Stable plugin id (orders ties).
    pub name: Arc<str>,
    /// Trust tier.
    pub tier: PluginTier,
}

/// One dirty node of a job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dirty {
    /// Node index.
    pub node: u32,
    /// Matching `[[watch]]` id.
    pub watch: u16,
    /// Cause code.
    pub cause: u8,
}

/// A `derive` job.
#[derive(Clone, Debug)]
pub struct Job {
    /// Index into [`ScopeRunner::plugins`].
    pub plugin: usize,
    /// Writer turn of the snapshot.
    pub t: u64,
    /// Dirty nodes.
    pub nodes: Vec<Dirty>,
    /// The snapshot.
    pub rows: Arc<FrozenRows>,
}

/// A value of an asserted fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FactValue {
    /// Explicit absence.
    Absent,
    /// Registry code.
    Code(u16),
    /// Bits.
    Bits(u64),
    /// Number.
    Number(u32),
    /// Text.
    Text(Arc<str>),
    /// Boolean.
    Truth(bool),
}

/// One fact effect of a call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactEffect {
    /// Target node.
    pub node: u32,
    /// Group code.
    pub group: u16,
    /// Value; `None` is a retract.
    pub value: Option<FactValue>,
    /// `layer = patch` (else `inferred`).
    pub patch: bool,
}

/// How a job ended.
#[derive(Clone, Debug)]
pub enum ReplyBody {
    /// Returned normally.
    Done {
        /// Fact effects.
        effects: Vec<FactEffect>,
        /// Read set R.
        reads: Vec<u32>,
        /// More than the cap was read: the scope-wide stamp applies.
        overflow: bool,
    },
    /// Dropped without facts (fault, overrun, skipped slot, cancelled/displaced by a newer job).
    Failed {
        /// The targets still need a derivation (displaced, skipped).
        requeue: bool,
    },
    /// The plugin was masked (backoff, disabled, quarantined): its facts leave the model.
    Masked,
    /// The plugin is back after a backoff.
    Unmasked,
}

/// A reply of the runner to the writer.
#[derive(Clone, Debug)]
pub struct Reply {
    /// Plugin index.
    pub plugin: usize,
    /// Turn of the job's snapshot.
    pub t: u64,
    /// The job's target nodes.
    pub targets: Vec<u32>,
    /// Outcome.
    pub body: ReplyBody,
}

/// What an interactive handoff produced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Handoff {
    /// Jobs sent.
    pub asked: u32,
    /// Replies that arrived before the wait ended.
    pub arrived: u32,
    /// Time spent waiting, ns.
    pub waited_ns: u64,
}

/// The plugins of one scope, as a writer sees them.
pub trait ScopeRunner: Send {
    /// The plugins, indexed.
    fn plugins(&self) -> &[PluginMeta];
    /// `[[watch]]` matches of a row: `(plugin, watch id)`. Native, cheap, no guest code.
    fn watches(&self, row: &Row) -> Vec<(usize, u16)>;
    /// Send the jobs and wait until all replied or the deadline `t0_ns + D` when `wait`.
    /// Replies are fetched with [`ScopeRunner::poll`]. The only blocking call (bounded).
    fn interactive(&mut self, t0_ns: u64, wait: bool, jobs: Vec<Job>) -> Handoff;
    /// Send background jobs; no wait.
    fn background(&mut self, jobs: Vec<Job>);
    /// Take the replies that have arrived.
    fn poll(&mut self, out: &mut Vec<Reply>);
    /// Replies are still outstanding.
    fn outstanding(&self) -> bool;
}

/// The daemon's plugin runtime as the writers see it.
pub trait PluginService: Send + Sync + std::fmt::Debug {
    /// Some loaded manifest has a URL glob: documents must report their URL.
    fn wants_url(&self) -> bool;
    /// Open the plugins matching `scope`; `None` when nothing matches. `wake` must be called
    /// (non-blocking) whenever a reply is waiting, so the writer drains it in a later turn.
    fn open(
        &self,
        scope: &ScopeSpec,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Box<dyn ScopeRunner>>;
    /// The keypress-path governor shared by every writer.
    fn governor(&self) -> Arc<Governor>;
}

/// The application identity a plugin scope is matched on, and whether it is verified.
///
/// `app_name` is what the application says about itself; anyone can claim any name. The
/// identity is **verified** only when the connector established the executable of the bus peer
/// from the bus credentials and `/proc` (`AppInfo::exe`: a root-owned system executable, a flatpak
/// app id or a snap name), and then that verified identity is the application name. Without it the self-reported name is used for display only and the scope is
/// unverified: the service refuses URL-scoped and third-party plugins for it.
pub fn scope_identity(app_name: &Arc<str>, exe: Option<&Arc<str>>) -> (Arc<str>, bool) {
    match exe {
        // A flatpak sandbox is named by its id but never verified: its evidence is forgeable.
        Some(e) if e.starts_with("flatpak:") => (Arc::clone(e), false),
        Some(e) if !e.is_empty() => (Arc::clone(e), true),
        Some(_) | None => (Arc::clone(app_name), false),
    }
}

/// Monotonic nanoseconds (`CLOCK_MONOTONIC`), the clock of the wait deadline.
pub fn now_ns() -> u64 {
    gorca_rt::process::monotonic_ns()
}

// ------------------------------------------------------------------------------------------
// One wait per keypress path
// ------------------------------------------------------------------------------------------

/// Decides which writer of a keypress path takes the bounded wait (ADR-0026 r4).
#[derive(Debug, Default)]
pub struct Governor {
    path_start: AtomicU64,
    /// Waits granted / refused (diagnostics).
    pub granted: AtomicU64,
    /// Waits refused because the path already waited.
    pub refused: AtomicU64,
}

impl Governor {
    /// Claim the wait for a writer turn that started at `t0_ns`. The first writer of a path
    /// gets `wait = true`; a later one within [`PATH_WINDOW_NS`] of that start gets `false`
    /// and publishes without waiting (its late facts follow as refinements).
    pub fn claim(&self, t0_ns: u64) -> bool {
        let mut cur = self.path_start.load(Ordering::Acquire);
        loop {
            if cur != 0 && t0_ns >= cur && t0_ns - cur < PATH_WINDOW_NS {
                self.refused.fetch_add(1, Ordering::Relaxed);
                return false;
            }
            match self.path_start.compare_exchange(
                cur,
                t0_ns.max(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.granted.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                Err(now) => cur = now,
            }
        }
    }
}

// ------------------------------------------------------------------------------------------
// Fact operations the writer applies to its engine
// ------------------------------------------------------------------------------------------

/// What admission decided; the writer applies it to its `Ivm` with [`PluginSources::apply`].
#[derive(Clone, Debug)]
pub enum FactOp {
    /// Computed S1 patch.
    Patch {
        /// Node.
        node: u32,
        /// Plugin id.
        plugin: Arc<str>,
        /// Value.
        write: Write,
        /// Taint the value inherits.
        taint: TrustTier,
    },
    /// S4 inferred fact (hedged).
    Inferred {
        /// Node.
        node: u32,
        /// Plugin index.
        plugin: usize,
        /// Value.
        write: Write,
    },
    /// Retract one fact.
    Retract {
        /// Node.
        node: u32,
        /// Plugin index.
        plugin: usize,
        /// Group.
        group: Group,
        /// The fact was a patch (else inferred).
        patch: bool,
    },
    /// The plugin is masked: all its facts leave the model.
    Mask {
        /// Plugin index.
        plugin: usize,
    },
    /// The plugin may contribute again.
    Unmask {
        /// Plugin index.
        plugin: usize,
    },
}

/// Counters of one bridge (diagnostics, tests, `explain`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BridgeStats {
    /// Jobs sent interactive / background.
    pub interactive_jobs: u64,
    /// Background jobs sent.
    pub background_jobs: u64,
    /// Replies admitted.
    pub admitted: u64,
    /// Replies dropped because the token was stale.
    pub stale_dropped: u64,
    /// Live tokens that went stale and were re-derived (PF-4).
    pub invalidated: u64,
    /// Facts admitted / dropped as unsupported.
    pub facts: u64,
    /// Facts the writer could not represent.
    pub unsupported: u64,
    /// Targets requeued after a displaced or failed job.
    pub requeued: u64,
    /// Dirty entries dropped by the cap.
    pub pending_dropped: u64,
    /// Interactive turns that waited / did not wait.
    pub waited: u64,
    /// Interactive turns that were refused the wait.
    pub no_wait: u64,
    /// Facts dropped because their node is a secret one (AC-1, SS-3).
    pub secret_dropped: u64,
    /// Interactive jobs sent, and replies that had arrived when the bounded wait ended.
    pub handoff_asked: u64,
    /// See `handoff_asked`.
    pub handoff_arrived: u64,
    /// Nanoseconds spent in the bounded wait, summed.
    pub handoff_waited_ns: u64,
    /// Nanoseconds spent admitting the replies after the wait, summed.
    pub handoff_drain_ns: u64,
}

struct LiveToken {
    reads: Vec<u32>,
    facts: Vec<(u32, Group, bool)>,
}

/// The writer's side of the plugin port for one scope.
pub struct PluginBridge {
    runner: Box<dyn ScopeRunner>,
    gov: Arc<Governor>,
    metas: Vec<PluginMeta>,
    scope: ScopeSpec,
    /// S0 rows by node index.
    rows: Vec<Option<Row>>,
    generation: Vec<u16>,
    /// Turn of the last S0 change of each node.
    stamp: Vec<u64>,
    /// Turn of the last S0 change anywhere in the scope.
    scope_stamp: u64,
    /// Writer clock T.
    t: u64,
    /// `(plugin, watch)` matches by node.
    watched: HashMap<u32, Vec<(usize, u16)>>,
    /// Dirty nodes per plugin, coalesced (latest cause wins per node).
    pending: Vec<BTreeMap<u32, (u16, u8)>>,
    /// Jobs in flight: `(plugin, target) -> turn sent`.
    inflight: HashMap<(usize, u32), u64>,
    live: HashMap<(usize, u32), LiveToken>,
    /// Reverse index `node -> tokens that read it`.
    rev: HashMap<u32, HashSet<(usize, u32)>>,
    overflowed: HashSet<(usize, u32)>,
    masked: Vec<bool>,
    ops: Vec<FactOp>,
    last_wait_ns: u64,
    stats: BridgeStats,
    replies: Vec<Reply>,
}

impl std::fmt::Debug for PluginBridge {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PluginBridge")
            .field("plugins", &self.metas.len())
            .field("live", &self.live.len())
            .field("t", &self.t)
            .finish_non_exhaustive()
    }
}

fn ix(n: u32) -> usize {
    n as usize
}

impl PluginBridge {
    /// A bridge over `runner`.
    pub fn new(runner: Box<dyn ScopeRunner>, gov: Arc<Governor>, scope: ScopeSpec) -> Self {
        let metas = runner.plugins().to_vec();
        let n = metas.len();
        Self {
            runner,
            gov,
            metas,
            scope,
            rows: Vec::new(),
            generation: Vec::new(),
            stamp: Vec::new(),
            scope_stamp: 0,
            t: 1,
            watched: HashMap::new(),
            pending: (0..n).map(|_| BTreeMap::new()).collect(),
            inflight: HashMap::new(),
            live: HashMap::new(),
            rev: HashMap::new(),
            overflowed: HashSet::new(),
            masked: vec![false; n],
            ops: Vec::new(),
            last_wait_ns: 0,
            stats: BridgeStats::default(),
            replies: Vec::new(),
        }
    }

    /// The plugins of this scope.
    pub fn metas(&self) -> &[PluginMeta] {
        &self.metas
    }

    /// Counters.
    pub fn stats(&self) -> BridgeStats {
        self.stats
    }

    /// Live tokens (tests).
    pub fn live_len(&self) -> usize {
        self.live.len()
    }

    /// Dirty nodes waiting (tests).
    pub fn pending_len(&self) -> usize {
        self.pending.iter().map(BTreeMap::len).sum()
    }

    /// Start a writer turn: the clock `T` advances. Every S0 change noted until the next call
    /// carries this turn as its stamp.
    pub fn begin_turn(&mut self) {
        self.t += 1;
    }

    /// The current writer turn.
    pub fn turn(&self) -> u64 {
        self.t
    }

    fn grow(&mut self, n: u32) {
        let need = ix(n) + 1;
        if self.rows.len() < need {
            self.rows.resize(need, None);
            self.generation.resize(need, 1);
            self.stamp.resize(need, 0);
        }
    }

    /// The S0 projection of `node` is `row` (a new node or a changed one). Returns true when it
    /// changed. Stamps the node, invalidates the live tokens that read it (PF-4: their targets
    /// are re-derived) and matches the `[[watch]]` selectors.
    pub fn upsert(&mut self, node: u32, row: Row) -> bool {
        self.grow(node);
        if self.rows[ix(node)].as_ref() == Some(&row) {
            return false;
        }
        // A secret node (a password) is never watched and never derived (AC-1, SS-3): a re-derive
        // on every keystroke would tell a plugin the length and rhythm of what is typed.
        let secret = row.secret;
        let matches = if secret {
            Vec::new()
        } else {
            self.runner.watches(&row)
        };
        self.rows[ix(node)] = Some(row);
        self.touch(node);
        if secret {
            self.forget_target(node);
        }
        if matches.is_empty() {
            self.watched.remove(&node);
        } else {
            self.watched.insert(node, matches.clone());
            let first = !self.live.keys().any(|(_, t)| *t == node);
            for (p, w) in matches {
                self.mark_dirty(
                    p,
                    node,
                    w,
                    if first { cause::ATTACH } else { cause::CHANGED },
                );
            }
        }
        true
    }

    /// Drop every pending derivation and live fact whose target is `node` (it became secret).
    fn forget_target(&mut self, node: u32) {
        self.watched.remove(&node);
        for p in &mut self.pending {
            p.remove(&node);
        }
        self.inflight.retain(|(_, t), _| *t != node);
        let own: Vec<(usize, u32)> = self
            .live
            .keys()
            .filter(|(_, t)| *t == node)
            .copied()
            .collect();
        for key in own {
            if let Some(tok) = self.drop_token(key) {
                for (n, group, patch) in tok.facts {
                    self.ops.push(FactOp::Retract {
                        node: n,
                        plugin: key.0,
                        group,
                        patch,
                    });
                }
            }
        }
    }

    /// The structure below or above `node` changed (children list, parent): same effect on the
    /// tokens that read `node` as a change of its row.
    pub fn touch(&mut self, node: u32) {
        self.grow(node);
        self.stamp[ix(node)] = self.t;
        self.scope_stamp = self.t;
        let mut hit: Vec<(usize, u32)> = self
            .rev
            .get(&node)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default();
        hit.extend(self.overflowed.iter().copied());
        hit.sort_unstable();
        hit.dedup();
        for key in hit {
            self.invalidate(key);
        }
    }

    /// The node left the engine.
    pub fn remove(&mut self, node: u32) {
        if ix(node) < self.rows.len() {
            self.touch(node);
            self.rows[ix(node)] = None;
            self.generation[ix(node)] = self.generation[ix(node)].wrapping_add(1).max(1);
        }
        self.watched.remove(&node);
        for p in &mut self.pending {
            p.remove(&node);
        }
        let own: Vec<(usize, u32)> = self
            .live
            .keys()
            .filter(|(_, t)| *t == node)
            .copied()
            .collect();
        for key in own {
            self.drop_token(key);
        }
        self.inflight.retain(|(_, t), _| *t != node);
    }

    fn mark_dirty(&mut self, plugin: usize, node: u32, watch: u16, c: u8) {
        let Some(p) = self.pending.get_mut(plugin) else {
            return;
        };
        p.insert(node, (watch, c));
        while p.len() > MAX_PENDING {
            p.pop_first();
            self.stats.pending_dropped += 1;
        }
    }

    fn drop_token(&mut self, key: (usize, u32)) -> Option<LiveToken> {
        let tok = self.live.remove(&key)?;
        for r in &tok.reads {
            if let Some(s) = self.rev.get_mut(r) {
                s.remove(&key);
                if s.is_empty() {
                    self.rev.remove(r);
                }
            }
        }
        self.overflowed.remove(&key);
        Some(tok)
    }

    /// A live token read a node that changed: retract its facts and re-derive its target.
    fn invalidate(&mut self, key: (usize, u32)) {
        let Some(tok) = self.drop_token(key) else {
            return;
        };
        self.stats.invalidated += 1;
        for (node, group, patch) in tok.facts {
            self.ops.push(FactOp::Retract {
                node,
                plugin: key.0,
                group,
                patch,
            });
        }
        let (watch, _) = self
            .watched
            .get(&key.1)
            .and_then(|w| w.iter().find(|(p, _)| *p == key.0))
            .map_or((0, 0), |(_, w)| (*w, 0));
        if self.watched.contains_key(&key.1) {
            self.mark_dirty(key.0, key.1, watch, cause::CHANGED);
        }
    }

    // ---- snapshots ----------------------------------------------------------------------

    fn snapshot(&self, targets: &[u32], tree: &dyn Tree) -> Arc<FrozenRows> {
        let mut rows: HashMap<u32, FrozenRow> = HashMap::new();
        let add = |n: u32, rows: &mut HashMap<u32, FrozenRow>, tree: &dyn Tree| -> bool {
            if rows.contains_key(&n) {
                return true;
            }
            let Some(Some(row)) = self.rows.get(ix(n)) else {
                return false;
            };
            if row.secret {
                return false;
            }
            rows.insert(
                n,
                FrozenRow {
                    row: row.clone(),
                    generation: self.generation.get(ix(n)).copied().unwrap_or(1),
                    parent: tree.parent(n),
                    children: tree.children(n),
                },
            );
            true
        };
        for &t in targets {
            if !add(t, &mut rows, tree) {
                continue;
            }
            // Ancestors (structure only, bounded).
            let mut cur = tree.parent(t);
            let mut up = 0;
            while let Some(p) = cur {
                if up >= SNAPSHOT_ANCESTORS || !add(p, &mut rows, tree) {
                    break;
                }
                cur = tree.parent(p);
                up += 1;
            }
            // Subtree breadth first, bounded in depth and size.
            let mut q: VecDeque<(u32, usize)> = VecDeque::new();
            q.push_back((t, 0));
            let mut taken = 0;
            while let Some((n, d)) = q.pop_front() {
                if taken >= SNAPSHOT_NODES || d >= SNAPSHOT_DEPTH {
                    continue;
                }
                for c in tree.children(n) {
                    if taken >= SNAPSHOT_NODES {
                        break;
                    }
                    if add(c, &mut rows, tree) {
                        taken += 1;
                        q.push_back((c, d + 1));
                    }
                }
            }
        }
        Arc::new(FrozenRows {
            scope: self.scope.clone(),
            rows,
        })
    }

    fn jobs_for(&mut self, per_plugin: Vec<Vec<Dirty>>, tree: &dyn Tree) -> Vec<Job> {
        let targets: Vec<u32> = per_plugin
            .iter()
            .flat_map(|v| v.iter().map(|d| d.node))
            .collect();
        if targets.is_empty() {
            return Vec::new();
        }
        let rows = self.snapshot(&targets, tree);
        let t = self.t;
        per_plugin
            .into_iter()
            .enumerate()
            .filter(|(_, v)| !v.is_empty())
            .map(|(plugin, nodes)| {
                for d in &nodes {
                    self.inflight.insert((plugin, d.node), t);
                }
                Job {
                    plugin,
                    t,
                    nodes,
                    rows: Arc::clone(&rows),
                }
            })
            .collect()
    }

    // ---- interactive ---------------------------------------------------------------------

    /// The focus changed (or a focus fetch landed): derive the watched nodes of the focus chain
    /// that have no live fact, wait at most once for the keypress path (the deadline is
    /// `t0_ns + 0.3 ms`, `t0_ns` the start of this writer turn), and admit what arrived.
    /// Returns true when the writer has new fact operations to apply.
    pub fn interactive(&mut self, chain: &[u32], tree: &dyn Tree, t0_ns: u64) -> bool {
        let mut per: Vec<Vec<Dirty>> = vec![Vec::new(); self.metas.len()];
        for &n in chain.iter().take(MAX_INTERACTIVE_NODES) {
            let Some(w) = self.watched.get(&n).cloned() else {
                continue;
            };
            for (p, watch) in w {
                if self.masked.get(p).copied().unwrap_or(true) || self.live.contains_key(&(p, n)) {
                    continue;
                }
                // Already asked about this very view: its reply will do.
                if self
                    .inflight
                    .get(&(p, n))
                    .is_some_and(|sent| *sent >= self.scope_stamp)
                {
                    continue;
                }
                if let Some(pp) = per.get_mut(p) {
                    pp.push(Dirty {
                        node: n,
                        watch,
                        cause: cause::FOCUS,
                    });
                }
                if let Some(pend) = self.pending.get_mut(p) {
                    pend.remove(&n);
                }
            }
        }
        let jobs = self.jobs_for(per, tree);
        if jobs.is_empty() {
            return self.drain();
        }
        self.stats.interactive_jobs += jobs.len() as u64;
        let may =
            t0_ns.saturating_sub(self.last_wait_ns) >= WAIT_EVERY_NS || self.last_wait_ns == 0;
        let wait = may && self.gov.claim(t0_ns);
        if wait {
            self.last_wait_ns = t0_ns;
            self.stats.waited += 1;
        } else {
            self.stats.no_wait += 1;
        }
        let h = self.runner.interactive(t0_ns, wait, jobs);
        self.stats.handoff_asked += u64::from(h.asked);
        self.stats.handoff_arrived += u64::from(h.arrived);
        self.stats.handoff_waited_ns += h.waited_ns;
        let t = now_ns();
        let any = self.drain();
        self.stats.handoff_drain_ns += now_ns().saturating_sub(t);
        any
    }

    /// Send up to [`MAX_BATCH`] dirty nodes per plugin as background jobs (no wait) and admit
    /// the replies that have arrived. Called from the writer's step.
    pub fn background(&mut self, tree: &dyn Tree) -> bool {
        let mut per: Vec<Vec<Dirty>> = vec![Vec::new(); self.metas.len()];
        for (p, pend) in self.pending.iter_mut().enumerate() {
            if self.masked.get(p).copied().unwrap_or(true) {
                pend.clear();
                continue;
            }
            for _ in 0..MAX_BATCH {
                let Some((node, (watch, c))) = pend.pop_first() else {
                    break;
                };
                if self.live.contains_key(&(p, node)) {
                    continue;
                }
                per[p].push(Dirty {
                    node,
                    watch,
                    cause: c,
                });
            }
        }
        let jobs = self.jobs_for(per, tree);
        if !jobs.is_empty() {
            self.stats.background_jobs += jobs.len() as u64;
            self.runner.background(jobs);
        }
        self.drain()
    }

    /// Admit the replies that arrived (late facts, refinements).
    pub fn drain(&mut self) -> bool {
        let mut replies = std::mem::take(&mut self.replies);
        replies.clear();
        self.runner.poll(&mut replies);
        let any = !replies.is_empty();
        for r in replies.drain(..) {
            self.admit(r);
        }
        self.replies = replies;
        any || !self.ops.is_empty()
    }

    /// Work is waiting: dirty nodes to send or replies outstanding.
    pub fn has_work(&self) -> bool {
        self.pending.iter().any(|p| !p.is_empty()) || self.runner.outstanding()
    }

    /// The fact operations to apply to the engine.
    pub fn take_ops(&mut self) -> Vec<FactOp> {
        std::mem::take(&mut self.ops)
    }

    fn admit(&mut self, r: Reply) {
        for tgt in &r.targets {
            if self.inflight.get(&(r.plugin, *tgt)) == Some(&r.t) {
                self.inflight.remove(&(r.plugin, *tgt));
            }
        }
        match r.body {
            ReplyBody::Masked => {
                if let Some(m) = self.masked.get_mut(r.plugin) {
                    *m = true;
                }
                let keys: Vec<_> = self
                    .live
                    .keys()
                    .filter(|(p, _)| *p == r.plugin)
                    .copied()
                    .collect();
                for k in keys {
                    // The facts leave the model now: an unmask must not bring back stale ones.
                    if let Some(tok) = self.drop_token(k) {
                        for (node, group, patch) in tok.facts {
                            self.ops.push(FactOp::Retract {
                                node,
                                plugin: r.plugin,
                                group,
                                patch,
                            });
                        }
                    }
                }
                if let Some(p) = self.pending.get_mut(r.plugin) {
                    p.clear();
                }
                self.ops.push(FactOp::Mask { plugin: r.plugin });
            }
            ReplyBody::Unmasked => {
                if let Some(m) = self.masked.get_mut(r.plugin) {
                    *m = false;
                }
                self.ops.push(FactOp::Unmask { plugin: r.plugin });
                // Everything watched is derived again, in the background.
                let nodes: Vec<(u32, u16)> = self
                    .watched
                    .iter()
                    .filter_map(|(n, w)| {
                        w.iter()
                            .find(|(p, _)| *p == r.plugin)
                            .map(|(_, w)| (*n, *w))
                    })
                    .collect();
                for (n, w) in nodes {
                    self.mark_dirty(r.plugin, n, w, cause::ATTACH);
                }
            }
            ReplyBody::Failed { requeue } => {
                if requeue {
                    for tgt in r.targets {
                        if let Some((w, _)) = self
                            .watched
                            .get(&tgt)
                            .and_then(|v| v.iter().find(|(p, _)| *p == r.plugin))
                            .map(|(_, w)| (*w, 0))
                        {
                            self.stats.requeued += 1;
                            self.mark_dirty(r.plugin, tgt, w, cause::CHANGED);
                        }
                    }
                }
            }
            ReplyBody::Done {
                effects,
                reads,
                overflow,
            } => self.admit_done(r.plugin, r.t, &r.targets, &effects, reads, overflow),
        }
    }

    fn admit_done(
        &mut self,
        plugin: usize,
        t: u64,
        targets: &[u32],
        effects: &[FactEffect],
        mut reads: Vec<u32>,
        overflow: bool,
    ) {
        if self.masked.get(plugin).copied().unwrap_or(true) {
            return;
        }
        // PF-7: stale iff a node the call read changed after the snapshot turn (the targets are
        // part of R). Zero false accepts; false rejects only by over-approximation.
        for tgt in targets {
            if !reads.contains(tgt) {
                reads.push(*tgt);
            }
        }
        let stale = if overflow {
            self.scope_stamp > t
        } else {
            reads
                .iter()
                .any(|n| self.stamp.get(ix(*n)).copied().unwrap_or(0) > t)
        };
        if stale {
            self.stats.stale_dropped += 1;
            for tgt in targets {
                if let Some(w) = self
                    .watched
                    .get(tgt)
                    .and_then(|v| v.iter().find(|(p, _)| *p == plugin))
                    .map(|(_, w)| *w)
                {
                    self.mark_dirty(plugin, *tgt, w, cause::CHANGED);
                }
            }
            return;
        }
        self.stats.admitted += 1;
        let tier = self
            .metas
            .get(plugin)
            .map_or(PluginTier::Untrusted, |m| m.tier);
        let name = self
            .metas
            .get(plugin)
            .map_or_else(|| Arc::from(""), |m| Arc::clone(&m.name));
        // Per target: the new fact set replaces the old one (a missing fact is a retract).
        for tgt in targets {
            let mut new_facts: Vec<(u32, Group, bool)> = Vec::new();
            for e in effects.iter().filter(|e| owned_by(e.node, *tgt, &reads)) {
                if self
                    .rows
                    .get(ix(e.node))
                    .and_then(Option::as_ref)
                    .is_none_or(|r| r.secret)
                {
                    self.stats.secret_dropped += 1;
                    continue;
                }
                let Some(GroupClass::Semantic(group)) = Some(GroupCode(e.group).classify()) else {
                    self.stats.unsupported += 1;
                    continue;
                };
                let Some(v) = &e.value else {
                    continue;
                };
                let Some(write) = to_write(group, v) else {
                    self.stats.unsupported += 1;
                    continue;
                };
                // Stratum by tier: only a computed patch of a first-party or user-trusted
                // plugin is S1; everything else is S4 and hedged.
                let s1 = e.patch
                    && matches!(tier, PluginTier::FirstParty | PluginTier::TrustedThirdParty);
                self.stats.facts += 1;
                if s1 {
                    let taint = match tier {
                        PluginTier::FirstParty => TrustTier::T0Core,
                        PluginTier::TrustedThirdParty | PluginTier::Untrusted => {
                            TrustTier::T3Untrusted
                        }
                    };
                    self.ops.push(FactOp::Patch {
                        node: e.node,
                        plugin: Arc::clone(&name),
                        write,
                        taint,
                    });
                } else {
                    self.ops.push(FactOp::Inferred {
                        node: e.node,
                        plugin,
                        write,
                    });
                }
                new_facts.push((e.node, group, s1));
            }
            // Explicit retracts of the call.
            for e in effects
                .iter()
                .filter(|e| e.value.is_none() && owned_by(e.node, *tgt, &reads))
            {
                if let Some(GroupClass::Semantic(group)) = Some(GroupCode(e.group).classify()) {
                    for patch in [true, false] {
                        self.ops.push(FactOp::Retract {
                            node: e.node,
                            plugin,
                            group,
                            patch,
                        });
                    }
                }
            }
            let old = self.drop_token((plugin, *tgt));
            if let Some(old) = old {
                for (node, group, patch) in old.facts {
                    if !new_facts
                        .iter()
                        .any(|(n, g, p)| *n == node && *g == group && *p == patch)
                    {
                        self.ops.push(FactOp::Retract {
                            node,
                            plugin,
                            group,
                            patch,
                        });
                    }
                }
            }
            for r in &reads {
                self.rev.entry(*r).or_default().insert((plugin, *tgt));
            }
            if overflow {
                self.overflowed.insert((plugin, *tgt));
            }
            self.live.insert(
                (plugin, *tgt),
                LiveToken {
                    reads: reads.clone(),
                    facts: new_facts,
                },
            );
        }
    }
}

/// A fact belongs to the target's call when it names the target or a node the call read.
fn owned_by(node: u32, target: u32, reads: &[u32]) -> bool {
    node == target || reads.contains(&node)
}

/// A fact value as the fusion's typed write; `None` for a group or shape this slice does not
/// represent.
pub fn to_write(group: Group, v: &FactValue) -> Option<Write> {
    Some(match (group, v) {
        (g, FactValue::Absent) => Write::Absent(g),
        (Group::Name, FactValue::Text(t)) if !t.is_empty() => Write::Val(Val::Name(Arc::clone(t))),
        (Group::Description, FactValue::Text(t)) if !t.is_empty() => {
            Write::Val(Val::Description(Arc::clone(t)))
        }
        (Group::ValueText, FactValue::Text(t)) => Write::Val(Val::ValueText(Arc::clone(t))),
        (Group::Placeholder, FactValue::Text(t)) => Write::Val(Val::Placeholder(Arc::clone(t))),
        (Group::Role, FactValue::Code(c)) => Write::Val(Val::Role(SemRole::from_code(*c)?)),
        (Group::Level, FactValue::Number(n)) => Write::Val(Val::Level(u8::try_from(*n).ok()?)),
        _ => return None,
    })
}

// ------------------------------------------------------------------------------------------
// Applying operations to a writer's engine
// ------------------------------------------------------------------------------------------

/// Base slot of plugin sources in a writer's engine (below it: the layout label source).
const SOURCE_SLOT_BASE: u32 = 0x5000;
/// TTL of an S4 plugin fact; a re-derivation renews it.
const S4_TTL_MS: u64 = 600_000;
/// Confidence of an inferred plugin fact (per mille); the tier cap hedges it.
const S4_P_MILLI: u16 = 850;

/// The plugin sources registered in one writer's engine.
#[derive(Debug)]
pub struct PluginSources {
    ids: Vec<SourceId>,
    names: Vec<Arc<str>>,
    seq: u64,
    epoch: u64,
}

impl PluginSources {
    /// Register one S4 source per plugin in `ivm`.
    pub fn register<Q: Quirks, P: PatchSource>(ivm: &mut Ivm<Q, P>, metas: &[PluginMeta]) -> Self {
        let mut ids = Vec::new();
        let mut names = Vec::new();
        for (i, m) in metas.iter().enumerate() {
            let id = SourceId {
                slot: SOURCE_SLOT_BASE + u32::try_from(i).unwrap_or(0),
                generation: 0,
            };
            let info = SourceInfo {
                id,
                name: Arc::clone(&m.name),
                class: SourceClass::Inferred,
                tier: TrustTier::T3Untrusted,
                kind_order: 8,
                grant: Grant::standard(),
            };
            if ivm.set_source(info).is_ok() {
                ivm.set_source_state(id, 1, SourceState::Active);
            }
            ids.push(id);
            names.push(Arc::clone(&m.name));
        }
        Self {
            ids,
            names,
            seq: 0,
            epoch: 1,
        }
    }

    /// Apply the operations. Returns the engine nodes that now need a run.
    pub fn apply<Q: Quirks, P: PatchSource>(&mut self, ivm: &mut Ivm<Q, P>, ops: Vec<FactOp>) {
        for op in ops {
            match op {
                FactOp::Patch {
                    node,
                    plugin,
                    write,
                    taint,
                } => {
                    ivm.set_computed(NodeIdx(node), &plugin, write, taint);
                }
                FactOp::Inferred {
                    node,
                    plugin,
                    write,
                } => {
                    self.seq += 1;
                    self.fact(ivm, node, plugin, Payload::Sem(write));
                }
                FactOp::Retract {
                    node,
                    plugin,
                    group,
                    patch,
                } => {
                    if patch {
                        if let Some(name) = self.names.get(plugin) {
                            ivm.retract_computed(NodeIdx(node), name, group);
                        }
                    } else {
                        self.seq += 1;
                        let seq = self.seq;
                        if let Some(id) = self.ids.get(plugin).copied() {
                            let anchor = AnchorId::Node(NodeIdx(node));
                            let f = FactIn {
                                source: id,
                                group: group.group_code(),
                                seq,
                                payload: Payload::Retract,
                                p_milli: S4_P_MILLI,
                                basis: Basis::None,
                                bind_gen: ivm.resolve_anchor(&anchor).generation,
                                admitted_at_ms: ivm.clock(),
                                ttl_ms: Some(S4_TTL_MS),
                                anchor,
                            };
                            let _ = ivm.apply([Input::Fact(Box::new(f))]);
                        }
                    }
                }
                FactOp::Mask { plugin } => {
                    if let (Some(name), Some(id)) = (self.names.get(plugin), self.ids.get(plugin)) {
                        ivm.clear_computed_source(name);
                        self.epoch += 1;
                        ivm.set_source_state(*id, self.epoch, SourceState::Quarantined);
                    }
                }
                FactOp::Unmask { plugin } => {
                    if let Some(id) = self.ids.get(plugin) {
                        self.epoch += 1;
                        ivm.set_source_state(*id, self.epoch, SourceState::Active);
                    }
                }
            }
        }
    }

    fn fact<Q: Quirks, P: PatchSource>(
        &mut self,
        ivm: &mut Ivm<Q, P>,
        node: u32,
        plugin: usize,
        payload: Payload,
    ) {
        let Some(id) = self.ids.get(plugin).copied() else {
            return;
        };
        let group = match &payload {
            Payload::Sem(w) => w.group().group_code(),
            Payload::Retract | Payload::Evidence(_) => return,
        };
        let anchor = AnchorId::Node(NodeIdx(node));
        let f = FactIn {
            source: id,
            group,
            seq: self.seq,
            payload,
            p_milli: S4_P_MILLI,
            basis: Basis::None,
            bind_gen: ivm.resolve_anchor(&anchor).generation,
            admitted_at_ms: ivm.clock(),
            ttl_ms: Some(S4_TTL_MS),
            anchor,
        };
        let _ = ivm.apply([Input::Fact(Box::new(f))]);
    }
}

#[cfg(test)]
pub(crate) mod testing;
#[cfg(test)]
mod tests;
