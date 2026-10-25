// SPDX-License-Identifier: GPL-3.0-or-later
//! The document actor's fusion stage (M22a-C2, ADR-0022 SM-8, ADR-0024 H-7).
//!
//! The document actor is the single writer of its nodes, so it is where `FUSE` runs. The source
//! delivers normalised records ([`gorca_model::SrcNode::rec`], built by the normaliser in the
//! source crate, never here); this stage mirrors the source's tree into a [`gorca_fuse::Ivm`],
//! feeds every record as the `atspi` primary's registers, and answers "what is the fused entry
//! of this source node". The translator asks it while it builds the mirror, so the entry
//! (`Arc<SemEntry>`) is part of the node's [`gorca_doc::web::NodeMeta`] when the batch is
//! published: fusion lands in the left-right back copy, never after the swap.
//!
//! # Timeline
//!
//! Registers keep the newest write per source (Lemma 1), so every write carries a sequence number
//! on the actor timeline: a batch is `2n` for the `n`-th flush; a fact issued while `t` batches
//! had been flushed (a focus fetch of a web node forwarded by the app actor, SM-8) is `2t + 1`,
//! newer than the batch that produced revision `t` and older than the next one.
//!
//! # Structure
//!
//! The Ivm follows the source's complete child lists (SL-4), not the mirror: placeholders, caps
//! and the translator's orphan handling stay the translator's business. A node listed before its
//! own upsert exists in the engine but is not alive (no life register) until the upsert arrives.
//! Everything the source sends is untrusted (SL-3): the node count is capped and every engine
//! error is counted, never propagated.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_doc_source_api::FuseSetup;
use gorca_doc_source_api::limits::{MAX_CHILDREN, MAX_DOC_NODES};
use gorca_fuse::{
    AnchorId, Budget, FactIn, Grant, Input, Ivm, NoPatches, NodeIdx, Payload, PrimarySlot,
    QuirkSet, SourceInfo, SourceState, Val, Write,
};
use gorca_model::fact::{Basis, SourceId};
use gorca_model::sem::{Group, SemEntry, SemRole, SrcRecord};
use gorca_model::sem::{SourceClass, TrustTier};
use gorca_model::{DocDelta, DocHint, SrcNode, SrcNodeId};

use crate::plugin_port::{
    FactOp, PluginBridge, PluginService, PluginSources, Row, ScopeSpec, Tree, now_ns,
};

/// Work one run of the engine may do before the actor goes back to its queues.
const BATCH_BUDGET: Duration = Duration::from_millis(2);

/// The AT-SPI primary's register slot.
const ATSPI: PrimarySlot = PrimarySlot::P0;

/// Where the translator reads fused entries from.
pub trait SemLookup {
    /// The published entry of a source node, `None` when it is unknown or not alive.
    fn sem(&self, src: SrcNodeId) -> Option<Arc<SemEntry>>;
}

/// No fusion: every node has no entry.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoSem;

impl SemLookup for NoSem {
    fn sem(&self, _src: SrcNodeId) -> Option<Arc<SemEntry>> {
        None
    }
}

/// Counters of the stage (process-independent: one per document).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct FuseStats {
    /// Batches and facts applied.
    pub applies: u64,
    /// Node fusions the engine performed.
    pub fusions: u64,
    /// Engine refusals (unknown node, cycle, duplicate): a hostile or buggy list, never fatal.
    pub errors: u64,
    /// Nodes not added because the document cap was reached.
    pub capped: u64,
}

/// The fusion stage of one document.
#[derive(Debug)]
pub struct FuseStage {
    setup: FuseSetup,
    ivm: Ivm<QuirkSet, NoPatches>,
    ids: HashMap<SrcNodeId, NodeIdx>,
    /// Source id of each engine slot (`None`: free).
    src_of: Vec<Option<SrcNodeId>>,
    free: Vec<u32>,
    root: Option<SrcNodeId>,
    batches: u64,
    stats: FuseStats,
    /// Sequence of the inferred-label facts (newest wins per node).
    label_seq: u64,
    /// The plugin port of this document (M18a), when plugins are enabled.
    plugins: Option<DocPlugins>,
}

/// The document's plugin scope: what opens it, and the open bridge with its engine sources.
struct DocPlugins {
    service: Arc<dyn PluginService>,
    app: Arc<str>,
    engine: u16,
    verified: bool,
    open: Option<(PluginBridge, PluginSources)>,
    /// The rows of a document whose scope is waiting for its URL (the URL arrives after the first
    /// block, never before it): replayed into the bridge when the scope opens, so the plugins
    /// derive what is already there. Empty unless some plugin matches on URLs.
    waiting: HashMap<u32, Row>,
}

impl std::fmt::Debug for DocPlugins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocPlugins")
            .field("app", &self.app)
            .field("open", &self.open.is_some())
            .finish_non_exhaustive()
    }
}

/// The engine's structure as the plugin snapshot reads it (S0 structure only, PF-2).
struct IvmTree<'a>(&'a Ivm<QuirkSet, NoPatches>);

impl Tree for IvmTree<'_> {
    fn parent(&self, n: u32) -> Option<u32> {
        self.0.parent(NodeIdx(n)).map(|p| p.0)
    }
    fn children(&self, n: u32) -> Vec<u32> {
        self.0.children(NodeIdx(n)).iter().map(|c| c.0).collect()
    }
}

/// The S0 row of a source node: the primary's observed values and neutral attributes, never a
/// fused entry (PF-2).
fn row_of(n: &SrcNode) -> Row {
    match &n.rec {
        Some(rec) => Row {
            role: rec.sem.role.code(),
            states: n.states.bits(),
            name: rec.name.clone(),
            // What is typed into a secret field is not for a plugin to see or to count.
            text: (!Row::is_secret_role(rec.sem.role))
                .then(|| n.text.clone())
                .flatten(),
            tag: (!rec.tag.is_empty()).then(|| Arc::from(rec.tag.as_str())),
            class: rec.class.clone(),
            aria_role: rec.aria_role.clone(),
            dom_id: rec.dom_id.clone(),
            secret: Row::is_secret_role(rec.sem.role),
        },
        // A synthetic node (a text run): the text is its content.
        None => Row {
            role: SemRole::StaticText.code(),
            states: n.states.bits(),
            name: n.text.clone(),
            text: n.text.clone(),
            ..Row::default()
        },
    }
}

/// The fact source of layout-inferred labels (C3b, ADR-0024 S4): the lowest-trust stratum, so the
/// label fills only a name nothing else gave, and its taint hedges it.
pub const LABEL_SOURCE: SourceId = SourceId {
    slot: 0x4C42,
    generation: 0,
};

/// How long an inferred label stays valid (`Inferred` facts need a TTL); a change of the node's
/// layout revision (`Basis::VisRev`) retires it earlier.
const LABEL_TTL_MS: u64 = 300_000;

/// Confidence of a layout-inferred label (per mille): above the grant floor, below any observed
/// source.
const LABEL_P_MILLI: u16 = 850;

fn engine(setup: &FuseSetup) -> Ivm<QuirkSet, NoPatches> {
    let mut ivm = Ivm::new(setup.profile.clone(), setup.quirks.clone(), NoPatches);
    let info = SourceInfo {
        id: LABEL_SOURCE,
        name: Arc::from("layout-label"),
        class: SourceClass::Inferred,
        tier: TrustTier::T3Untrusted,
        kind_order: 9,
        grant: Grant::standard(),
    };
    // A fresh engine has room for its own sources; a refusal only means no inferred labels.
    if ivm.set_source(info).is_ok() {
        ivm.set_source_state(LABEL_SOURCE, 1, SourceState::Active);
    }
    // Production only: tests and replays keep the drain purely work-based.
    if !cfg!(test) {
        ivm.set_sync_valve(Some(gorca_fuse::SYNC_WALL_VALVE));
    }
    ivm
}

impl FuseStage {
    pub fn new(setup: FuseSetup) -> FuseStage {
        FuseStage {
            ivm: engine(&setup),
            setup,
            ids: HashMap::new(),
            src_of: Vec::new(),
            free: Vec::new(),
            root: None,
            batches: 0,
            stats: FuseStats::default(),
            label_seq: 0,
            plugins: None,
        }
    }

    /// Enable plugins for this document: `app` is the core's application identity. The scope
    /// opens when the document's URL is known (or at once when no loaded manifest has a URL
    /// glob).
    #[must_use]
    pub fn with_plugins(
        mut self,
        service: Arc<dyn PluginService>,
        app: Arc<str>,
        engine: u16,
        verified: bool,
    ) -> FuseStage {
        self.plugins = Some(DocPlugins {
            service,
            app,
            engine,
            verified,
            open: None,
            waiting: HashMap::new(),
        });
        self
    }

    /// Open (or re-open) the plugin scope for `url`. A scope that is already open stays: a
    /// route change inside a document never swaps the plugin set (ADR-0026 gap, M18a-5).
    fn open_plugins(&mut self, url: Option<&Arc<str>>) {
        let Some(pl) = self.plugins.as_mut() else {
            return;
        };
        if pl.open.is_some() {
            return;
        }
        let scope = ScopeSpec {
            app: Arc::clone(&pl.app),
            url: if pl.service.wants_url() {
                url.cloned()
            } else {
                None
            },
            engine: pl.engine,
            verified: pl.verified,
            doc: true,
        };
        if pl.service.wants_url() && scope.url.is_none() {
            return;
        }
        let Some(runner) = pl.service.open(&scope, Arc::new(|| {})) else {
            return;
        };
        let metas = runner.plugins().to_vec();
        let mut bridge = PluginBridge::new(runner, pl.service.governor(), scope);
        // A scope that opens late (the URL came after the first block) learns what is there.
        let mut rows: Vec<(u32, Row)> = pl.waiting.drain().collect();
        rows.sort_by_key(|(k, _)| *k);
        for (k, row) in rows {
            bridge.upsert(k, row);
        }
        let sources = PluginSources::register(&mut self.ivm, &metas);
        pl.open = Some((bridge, sources));
    }

    fn bridge(&mut self) -> Option<&mut PluginBridge> {
        self.plugins
            .as_mut()
            .and_then(|p| p.open.as_mut())
            .map(|(b, _)| b)
    }

    /// Apply the bridge's fact operations to the engine.
    fn apply_plugin_ops(&mut self) {
        let Some((bridge, sources)) = self.plugins.as_mut().and_then(|p| p.open.as_mut()) else {
            return;
        };
        let ops: Vec<FactOp> = bridge.take_ops();
        if !ops.is_empty() {
            sources.apply(&mut self.ivm, ops);
        }
    }

    /// Plugin work is waiting (dirty nodes to send, replies outstanding).
    fn plugin_work(&self) -> bool {
        self.plugins
            .as_ref()
            .and_then(|p| p.open.as_ref())
            .is_some_and(|(b, _)| b.has_work())
    }

    pub fn stats(&self) -> FuseStats {
        self.stats
    }

    /// Name of the profile the tables came from (logs).
    pub fn profile_name(&self) -> &str {
        &self.setup.name
    }

    /// Source nodes the engine holds.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// The timeline value a fact issued now would carry: the number of batches applied.
    pub fn timeline(&self) -> u64 {
        self.batches
    }

    fn reset(&mut self) {
        // A new document: the scope closes (its actors stop) and opens again on the new URL.
        if let Some(p) = self.plugins.as_mut() {
            p.waiting.clear();
            p.open = None;
        }
        self.ivm = engine(&self.setup);
        self.ids.clear();
        self.src_of.clear();
        self.free.clear();
        self.root = None;
    }

    fn alloc(&mut self, src: SrcNodeId) -> Option<NodeIdx> {
        if let Some(n) = self.ids.get(&src) {
            return Some(*n);
        }
        if self.ids.len() >= MAX_DOC_NODES {
            self.stats.capped += 1;
            return None;
        }
        let ix = if let Some(i) = self.free.pop() {
            i
        } else {
            let i = u32::try_from(self.src_of.len()).ok()?;
            self.src_of.push(None);
            i
        };
        let n = NodeIdx(ix);
        if self.ivm.add_node(n, None).is_err() {
            self.stats.errors += 1;
            self.free.push(ix);
            return None;
        }
        if let Some(slot) = self.src_of.get_mut(ix as usize) {
            *slot = Some(src);
        }
        self.ids.insert(src, n);
        Some(n)
    }

    /// Forget the engine slot (the engine already dropped the node).
    fn release(&mut self, n: NodeIdx) {
        if let Some(slot) = self.src_of.get_mut(n.0 as usize)
            && let Some(src) = slot.take()
        {
            self.ids.remove(&src);
            self.free.push(n.0);
        }
    }

    /// Remove `n` and its subtree from the engine and the id maps.
    fn remove_subtree(&mut self, n: NodeIdx) {
        let mut all = vec![n];
        let mut i = 0;
        while let Some(&cur) = all.get(i) {
            all.extend_from_slice(self.ivm.children(cur));
            i += 1;
        }
        if self.ivm.remove_subtree(n).is_err() {
            self.stats.errors += 1;
            return;
        }
        for x in all {
            if let Some(pl) = self.plugins.as_mut() {
                pl.waiting.remove(&x.0);
            }
            if let Some(b) = self.bridge() {
                b.remove(x.0);
            }
            self.release(x);
        }
    }

    /// Write a record's known groups as the `atspi` primary's registers at `seq`. The record's
    /// HTML `id` goes to the engine too: a document-source record is the whole attribute list
    /// (no `id` there means the node has none), a focus-fetch record is not (it never carries
    /// the `id`, so it must not clear the one the document source reported).
    fn write(&mut self, node: NodeIdx, rec: &SrcRecord, seq: u64, whole_node: bool) {
        let writes = writes_of(rec);
        let mut inputs: Vec<Input> = Vec::with_capacity(writes.len() + 2);
        inputs.push(Input::Life {
            node,
            src: ATSPI,
            seq,
            alive: true,
        });
        if whole_node || rec.dom_id.is_some() {
            inputs.push(Input::Attr {
                node,
                seq,
                id: rec.dom_id.clone(),
            });
        }
        inputs.extend(writes.into_iter().map(|write| Input::Obs {
            node,
            src: ATSPI,
            seq,
            write,
        }));
        let rep = self.ivm.apply(inputs);
        self.stats.errors += rep.errors.len() as u64;
    }

    fn upsert(&mut self, n: &SrcNode, seq: u64) {
        let Some(ix) = self.alloc(n.id) else { return };
        if let Some(pl) = self.plugins.as_mut() {
            match pl.open.as_mut() {
                Some((b, _)) => {
                    b.upsert(ix.0, row_of(n));
                }
                None if pl.service.wants_url() => {
                    pl.waiting.insert(ix.0, row_of(n));
                }
                None => {}
            }
        }
        if let Some(rec) = &n.rec {
            self.write(ix, rec, seq, true);
            return;
        }
        // A synthetic node (a text run): the text is its content.
        let mut rec = SrcRecord::default();
        rec.sem.role = SemRole::StaticText;
        rec.known.set(Group::Role, true);
        if let Some(t) = &n.text {
            rec.name = Some(Arc::clone(t));
            rec.known.set(Group::Name, true);
        }
        self.write(ix, &rec, seq, true);
    }

    fn set_children(&mut self, parent: SrcNodeId, list: &[SrcNodeId], detached: &mut Vec<NodeIdx>) {
        let Some(p) = self.alloc(parent) else { return };
        let mut kids: Vec<NodeIdx> = Vec::with_capacity(list.len().min(MAX_CHILDREN));
        for c in list.iter().take(MAX_CHILDREN) {
            if let Some(k) = self.alloc(*c) {
                kids.push(k);
            }
        }
        let before: Vec<NodeIdx> = self.ivm.children(p).to_vec();
        if self.ivm.set_children(p, &kids).is_err() {
            self.stats.errors += 1;
            return;
        }
        if before != kids
            && let Some(b) = self.bridge()
        {
            b.touch(p.0);
        }
        detached.extend(before.into_iter().filter(|c| self.ivm.parent(*c).is_none()));
    }

    /// Apply one settled batch (the same deltas the translator applies) and run the engine.
    /// Returns the source nodes whose entry changed (an entry that equals the published one is
    /// cut off by the engine and not listed).
    pub fn apply(&mut self, batch: &[DocDelta]) -> Vec<SrcNodeId> {
        self.batches += 1;
        self.stats.applies += 1;
        let seq = 2 * self.batches;
        // A new document: everything of the batch belongs to it, like the translator's reset.
        let reset_root = batch.iter().rev().find_map(|d| {
            if let DocDelta::Reset { root, .. } = d {
                Some(*root)
            } else {
                None
            }
        });
        if let Some(root) = reset_root {
            self.reset();
            self.root = Some(root);
        }
        if self.plugins.is_some() {
            let mut url: Option<Arc<str>> = None;
            for d in batch {
                if let DocDelta::Hint(DocHint::RouteChanged { url: u }) = d {
                    url = Some(Arc::clone(u));
                }
            }
            self.open_plugins(url.as_ref());
            if let Some(b) = self.bridge() {
                b.begin_turn();
            }
        }
        for d in batch {
            if let DocDelta::Upsert(nodes) = d {
                for n in nodes {
                    self.upsert(n, seq);
                }
            }
        }
        // The last list of a parent wins (as in the translator).
        let mut lists: HashMap<SrcNodeId, &[SrcNodeId]> = HashMap::new();
        let mut order: Vec<SrcNodeId> = Vec::new();
        for d in batch {
            if let DocDelta::Children { parent, children } = d
                && lists.insert(*parent, children.as_slice()).is_none()
            {
                order.push(*parent);
            }
        }
        let mut detached: Vec<NodeIdx> = Vec::new();
        for p in order {
            if let Some(list) = lists.get(&p) {
                self.set_children(p, list, &mut detached);
            }
        }
        // Dropped from a list and not taken by another: gone (the translator reaps these).
        for n in detached {
            if self.is_root(n) || self.ivm.parent(n).is_some() || !self.ivm.contains(n) {
                continue;
            }
            self.remove_subtree(n);
        }
        for d in batch {
            if let DocDelta::Removed(ids) = d {
                for id in ids {
                    let Some(n) = self.ids.get(id).copied() else {
                        continue;
                    };
                    // A node a list kept stays (removals only take what nothing kept).
                    if self.is_root(n) || self.ivm.parent(n).is_some() {
                        continue;
                    }
                    self.remove_subtree(n);
                }
            }
        }
        self.plugin_background();
        self.run()
    }

    /// Send the dirty watched nodes to their plugins (no wait) and apply what has arrived.
    fn plugin_background(&mut self) {
        if let Some((bridge, _)) = self.plugins.as_mut().and_then(|p| p.open.as_mut()) {
            bridge.background(&IvmTree(&self.ivm));
        }
        self.apply_plugin_ops();
    }

    /// Write a record the app actor fetched for a node of this document (a focus fetch of a web
    /// node, SM-8) at `2 * issued_at + 1`. `None` when the node is not in the engine.
    pub fn fact(
        &mut self,
        src: SrcNodeId,
        rec: &SrcRecord,
        issued_at: u64,
    ) -> Option<Vec<SrcNodeId>> {
        let t0 = now_ns();
        let n = self.ids.get(&src).copied()?;
        self.stats.applies += 1;
        // The focus chain (the fetched node and its ancestors, nearest first) fuses first.
        let mut chain = vec![n];
        let mut cur = n;
        while let Some(p) = self.ivm.parent(cur) {
            chain.push(p);
            cur = p;
            if chain.len() > 64 {
                break;
            }
        }
        self.ivm.set_focus_chain(&chain);
        self.write(n, rec, 2 * issued_at + 1, false);
        if let Some((bridge, _)) = self.plugins.as_mut().and_then(|p| p.open.as_mut()) {
            // The start of this writer turn: the bounded wait is measured from here.
            bridge.begin_turn();
            let ids: Vec<u32> = chain.iter().map(|c| c.0).collect();
            bridge.interactive(&ids, &IvmTree(&self.ivm), t0);
        }
        self.apply_plugin_ops();
        Some(self.run())
    }

    /// Admit the label inferred from the layout for the nameless node `src` as an S4 fact: it
    /// fills the name only while nothing better exists, carries the T3 taint (so the entry is
    /// hedged) and is valid for the layout revision the node has now. `None` when the node is
    /// not in the engine.
    pub fn infer_label(&mut self, src: SrcNodeId, text: &str) -> Option<Vec<SrcNodeId>> {
        let n = *self.ids.get(&src)?;
        let anchor = AnchorId::Node(n);
        self.label_seq += 1;
        let fact = FactIn {
            source: LABEL_SOURCE,
            group: Group::Name.group_code(),
            seq: self.label_seq,
            payload: Payload::Sem(Write::Val(Val::Name(Arc::from(text)))),
            p_milli: LABEL_P_MILLI,
            basis: Basis::VisRev(self.ivm.vis_rev(n)),
            bind_gen: self.ivm.resolve_anchor(&anchor).generation,
            admitted_at_ms: self.ivm.clock(),
            ttl_ms: Some(LABEL_TTL_MS),
            anchor,
        };
        let rep = self.ivm.apply([Input::Fact(Box::new(fact))]);
        self.stats.errors += rep.errors.len() as u64;
        Some(self.run())
    }

    fn is_root(&self, n: NodeIdx) -> bool {
        self.root
            .and_then(|r| self.ids.get(&r))
            .is_some_and(|r| *r == n)
    }

    /// Fusions still owed after a budgeted run (dirty nodes and background dependents).
    pub fn pending(&self) -> usize {
        self.ivm.pending() + usize::from(self.plugin_work())
    }

    /// Continue the fusion the last run left (one more budget); returns the entries that changed.
    pub fn step(&mut self) -> Vec<SrcNodeId> {
        // Late replies of the plugin actors are admitted here, in this writer's own turn
        // (refinements after the first utterance, LB-4).
        if let Some(b) = self.bridge() {
            b.begin_turn();
        }
        self.plugin_background();
        self.run()
    }

    /// One bounded run: the focus chain and the visible window first (the engine's priority
    /// classes), at most [`BATCH_BUDGET`] of work (ADR-0022: the writer's interactive messages
    /// never wait for a whole page to fuse). What is left stays dirty for [`FuseStage::step`].
    fn run(&mut self) -> Vec<SrcNodeId> {
        let rep = self.ivm.run(Budget::until(Instant::now() + BATCH_BUDGET));
        self.stats.fusions += rep.fuse_calls as u64;
        rep.published
            .iter()
            .filter(|p| p.entry.is_some())
            .filter_map(|p| self.src_of.get(p.node.0 as usize).copied().flatten())
            .collect()
    }
}

impl SemLookup for FuseStage {
    fn sem(&self, src: SrcNodeId) -> Option<Arc<SemEntry>> {
        let n = self.ids.get(&src)?;
        // PV-1: a node waiting for its fusion still has an entry (provisional, replaced by the
        // first real fusion), so no reader needs a branch for "not fused yet".
        self.ivm.entry_or_provisional(*n)
    }
}

/// The register writes of a record: a known group holding its default is `Absent` (the engine
/// was asked and said nothing), anything else is a value; an unknown group writes nothing.
pub fn writes_of(rec: &SrcRecord) -> Vec<Write> {
    fn pick<T: Default + PartialEq>(v: T, g: Group, f: impl FnOnce(T) -> Val) -> Write {
        if v == T::default() {
            Write::Absent(g)
        } else {
            Write::Val(f(v))
        }
    }
    let s = &rec.sem;
    let mut out = Vec::with_capacity(gorca_model::sem::GROUP_COUNT);
    for &g in Group::ALL {
        if !rec.known.has(g) {
            continue;
        }
        out.push(match g {
            Group::Role => pick(s.role, g, Val::Role),
            Group::Name => match &rec.name {
                Some(n) if !n.is_empty() => Write::Val(Val::Name(Arc::clone(n))),
                _ => Write::Absent(g),
            },
            Group::Description => match s.description() {
                Some(d) if !d.is_empty() => Write::Val(Val::Description(Arc::from(d))),
                _ => Write::Absent(g),
            },
            Group::Check => pick(s.check, g, Val::Check),
            Group::Press => pick(s.press, g, Val::Press),
            Group::Expand => pick(s.expand, g, Val::Expand),
            Group::Select => pick(s.select, g, Val::Select),
            Group::Current => pick(s.current, g, Val::Current),
            Group::Invalid => pick(s.invalid, g, Val::Invalid),
            Group::Popup => pick(s.popup, g, Val::Popup),
            Group::Avail => pick(s.avail, g, Val::Avail),
            Group::Flags => pick(s.flags, g, Val::Flags),
            Group::Level => pick(s.level, g, Val::Level),
            Group::SetPos => s
                .pos
                .map_or(Write::Absent(g), |p| Write::Val(Val::SetPos(p))),
            Group::Grid => s
                .grid
                .map_or(Write::Absent(g), |p| Write::Val(Val::Grid(p))),
            Group::Live => s
                .live
                .map_or(Write::Absent(g), |l| Write::Val(Val::Live(Some(l)))),
            Group::Orientation => pick(s.orientation, g, Val::Orientation),
            Group::Sort => pick(s.sort, g, Val::Sort),
            Group::RoleDescription
            | Group::Placeholder
            | Group::KeyShortcuts
            | Group::ValueText
            | Group::ErrorText
            | Group::NameFrom => gorca_fuse::rare_write(s, g).unwrap_or(Write::Absent(g)),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::{Check, GroupMask};
    use gorca_model::{AttrMap, Role, StateSet};

    use super::*;

    fn setup() -> FuseSetup {
        use gorca_fuse::{Profile, Rules};
        let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
        profile.default_role = SemRole::Generic;
        FuseSetup {
            name: Arc::from("test"),
            profile,
            quirks: QuirkSet::default(),
        }
    }

    fn rec(role: SemRole, name: Option<&str>) -> Arc<SrcRecord> {
        let mut r = SrcRecord::default();
        r.sem.role = role;
        r.name = name.map(Arc::from);
        r.known = GroupMask::EMPTY;
        r.known.set(Group::Role, true);
        r.known.set(Group::Name, true);
        Arc::new(r)
    }

    fn node(id: u64, role: SemRole, name: Option<&str>) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role: Role::Unknown,
            states: StateSet::default(),
            name: name.map(Arc::from),
            text: None,
            attrs: AttrMap::default(),
            local: None,
            rec: Some(rec(role, name)),
        }
    }

    fn kids(p: u64, c: &[u64]) -> DocDelta {
        DocDelta::Children {
            parent: SrcNodeId(p),
            children: c.iter().map(|x| SrcNodeId(*x)).collect(),
        }
    }

    fn name_of(s: &FuseStage, id: u64) -> Option<String> {
        s.sem(SrcNodeId(id))
            .and_then(|e| e.name.as_deref().map(str::to_owned))
    }

    fn reset(root: u64) -> DocDelta {
        DocDelta::Reset {
            root: SrcNodeId(root),
            nav_id: 1,
        }
    }

    #[test]
    fn a_document_is_fused_and_content_names_flow_up() {
        let mut s = FuseStage::new(setup());
        let changed = s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Link, None),
                node(3, SemRole::StaticText, Some("Home")),
            ]),
            kids(1, &[2]),
            kids(2, &[3]),
        ]);
        assert!(changed.contains(&SrcNodeId(2)));
        // The link has no name of its own: it is named from its content.
        assert_eq!(name_of(&s, 2).as_deref(), Some("Home"));
        assert_eq!(s.sem(SrcNodeId(2)).map(|e| e.sem.role), Some(SemRole::Link));
        assert_eq!(s.len(), 3);
    }

    fn with_id(mut n: SrcNode, id: Option<&str>) -> SrcNode {
        let mut r = n.rec.as_deref().cloned().unwrap_or_default();
        r.dom_id = id.map(Arc::from);
        n.rec = Some(Arc::new(r));
        n
    }

    fn dom_id_of(s: &FuseStage, id: u64) -> Option<String> {
        s.sem(SrcNodeId(id))
            .and_then(|e| e.sem.dom_id().map(ToString::to_string))
    }

    #[test]
    fn the_html_id_of_a_record_reaches_the_entry_and_only_a_whole_node_record_clears_it() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                with_id(node(2, SemRole::Paragraph, None), Some("msg-1")),
            ]),
            kids(1, &[2]),
        ]);
        assert_eq!(dom_id_of(&s, 2).as_deref(), Some("msg-1"));
        assert_eq!(dom_id_of(&s, 1), None);
        // A focus fetch never carries the id; it must not clear what the source reported.
        let fetched = rec(SemRole::Paragraph, Some("Hello"));
        s.fact(SrcNodeId(2), &fetched, 5);
        assert_eq!(dom_id_of(&s, 2).as_deref(), Some("msg-1"));
        // The source's next record of the node is the whole attribute list: no id, none.
        s.apply(&[DocDelta::Upsert(vec![node(2, SemRole::Paragraph, None)])]);
        assert_eq!(dom_id_of(&s, 2), None);
    }

    #[test]
    fn a_later_batch_changes_only_what_it_touches_and_equal_input_changes_nothing() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Button, Some("Save")),
            ]),
            kids(1, &[2]),
        ]);
        let again = s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Save"),
        )])]);
        assert!(again.is_empty(), "an equal record is cut off: {again:?}");
        let ch = s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Store"),
        )])]);
        assert_eq!(ch, vec![SrcNodeId(2)]);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Store"));
    }

    #[test]
    fn child_lists_reorder_move_and_reap_like_the_translator() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Group, None),
                node(3, SemRole::Group, None),
                node(4, SemRole::StaticText, Some("a")),
                node(5, SemRole::StaticText, Some("b")),
            ]),
            kids(1, &[2, 3]),
            kids(2, &[4, 5]),
        ]);
        assert_eq!(s.len(), 5);
        // Move 5 under 3 and drop 4 from the tree: 4 is gone, 5 follows its new parent.
        s.apply(&[kids(2, &[]), kids(3, &[5])]);
        assert_eq!(s.len(), 4, "node 4 was dropped from every list and reaped");
        assert!(s.sem(SrcNodeId(4)).is_none());
        assert!(s.sem(SrcNodeId(5)).is_some());
        // An explicit removal takes a subtree nothing kept; one a list kept stays.
        s.apply(&[
            DocDelta::Removed(vec![SrcNodeId(3), SrcNodeId(1)]),
            kids(1, &[2, 3]),
        ]);
        assert_eq!(
            s.len(),
            4,
            "3 stayed (a list kept it), the root is never removed"
        );
        s.apply(&[kids(1, &[2]), DocDelta::Removed(vec![SrcNodeId(3)])]);
        assert_eq!(s.len(), 2, "3 and its child 5 left");
        // Slots are reused.
        let slots = s.src_of.len();
        s.apply(&[
            DocDelta::Upsert(vec![node(9, SemRole::Button, Some("x"))]),
            kids(1, &[2, 9]),
        ]);
        assert_eq!(s.src_of.len(), slots, "a freed engine slot was reused");
    }

    #[test]
    fn a_listed_child_without_an_upsert_is_not_alive_until_it_arrives() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![node(1, SemRole::DocumentWeb, None)]),
            kids(1, &[7]),
        ]);
        assert!(s.sem(SrcNodeId(7)).is_none(), "a placeholder has no entry");
        s.apply(&[DocDelta::Upsert(vec![node(
            7,
            SemRole::Button,
            Some("Late"),
        )])]);
        assert_eq!(name_of(&s, 7).as_deref(), Some("Late"));
    }

    #[test]
    fn a_reset_starts_a_new_document_and_hostile_lists_only_count_errors() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![node(1, SemRole::DocumentWeb, None)]),
            kids(1, &[2]),
        ]);
        s.apply(&[
            reset(10),
            DocDelta::Upsert(vec![node(10, SemRole::DocumentWeb, None)]),
        ]);
        assert_eq!(s.len(), 1, "the old page is gone");
        assert!(s.sem(SrcNodeId(2)).is_none());
        // A cycle (a node listing its ancestor) is refused and counted, nothing panics.
        s.apply(&[
            DocDelta::Upsert(vec![node(11, SemRole::Group, None)]),
            kids(10, &[11]),
            kids(11, &[10]),
        ]);
        assert!(s.stats().errors >= 1);
        assert_eq!(
            s.sem(SrcNodeId(10)).map(|e| e.sem.role),
            Some(SemRole::DocumentWeb)
        );
    }

    #[test]
    fn a_fact_is_older_than_the_next_batch_and_newer_than_the_last() {
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::Button, Some("Old")),
            ]),
            kids(1, &[2]),
        ]);
        let t = s.timeline();
        // Issued after batch t: wins over it.
        let ch = s.fact(SrcNodeId(2), &rec(SemRole::Button, Some("Fresh")), t);
        assert_eq!(ch, Some(vec![SrcNodeId(2)]));
        assert_eq!(name_of(&s, 2).as_deref(), Some("Fresh"));
        // A fact issued before the batch that followed it loses to that batch.
        s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::Button,
            Some("Batch"),
        )])]);
        let late = s.fact(SrcNodeId(2), &rec(SemRole::Button, Some("Stale")), t);
        assert_eq!(late, Some(Vec::new()), "older than the batch: no change");
        assert_eq!(name_of(&s, 2).as_deref(), Some("Batch"));
        // Unknown node: no entry to write.
        assert!(
            s.fact(SrcNodeId(99), &rec(SemRole::Button, None), t)
                .is_none()
        );
    }

    #[test]
    fn writes_are_absent_for_defaults_and_nothing_for_unknown_groups() {
        let mut r = SrcRecord::default();
        r.sem.role = SemRole::Checkbox;
        r.sem.check = Check::On;
        r.known.set(Group::Role, true);
        r.known.set(Group::Check, true);
        r.known.set(Group::Expand, true);
        let w = writes_of(&r);
        assert_eq!(w.len(), 3, "unknown groups write nothing: {w:?}");
        assert!(w.contains(&Write::Val(Val::Check(Check::On))));
        assert!(w.contains(&Write::Absent(Group::Expand)));
    }

    /// A first block of a big page is fused inside the flush that publishes it: the cost per node
    /// is part of the first-useful latency. The bound is loose (debug build, shared host); the
    /// printed number is the measurement.
    #[test]
    fn a_big_first_block_fuses_in_bounded_time() {
        const N: u64 = 20_000;
        let mut nodes = vec![node(1, SemRole::DocumentWeb, None)];
        let mut lists: Vec<DocDelta> = Vec::new();
        let mut top: Vec<u64> = Vec::new();
        for g in 0..(N / 20) {
            let gid = 2 + g * 21;
            nodes.push(node(gid, SemRole::Group, None));
            let mut ks = Vec::new();
            for i in 1..=20 {
                nodes.push(node(gid + i, SemRole::Link, Some("link")));
                ks.push(gid + i);
            }
            lists.push(kids(gid, &ks));
            top.push(gid);
        }
        lists.push(kids(1, &top));
        let mut batch = vec![reset(1), DocDelta::Upsert(nodes)];
        batch.extend(lists);
        let mut s = FuseStage::new(setup());
        let t = std::time::Instant::now();
        let mut changed = s.apply(&batch);
        let first = t.elapsed();
        while s.pending() > 0 {
            changed.extend(s.step());
        }
        let took = t.elapsed();
        eprintln!("first run {first:?}");
        assert!(
            first < Duration::from_millis(500),
            "the first run is bounded: {first:?}"
        );
        eprintln!(
            "fuse stage: {} nodes in {:?} ({:.2} us/node, debug build)",
            s.len(),
            took,
            took.as_secs_f64() * 1e6 / f64::from(u32::try_from(s.len()).unwrap_or(u32::MAX))
        );
        assert!(changed.len() >= usize::try_from(N).unwrap());
        assert!(took < std::time::Duration::from_secs(20), "{took:?}");
    }

    /// C3b: the label inferred from the layout is an S4 fact: it fills a nameless control, is
    /// hedged and tainted, never replaces a name a primary gave, and shows up in the entry.
    #[test]
    fn an_inferred_label_is_the_lowest_trust_name() {
        use gorca_model::sem::{ConfClass, ProvCode, TrustTier};
        let mut s = FuseStage::new(setup());
        s.apply(&[
            reset(1),
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::TextBox, None),
                node(3, SemRole::TextBox, Some("Email")),
            ]),
            kids(1, &[2, 3]),
        ]);
        assert_eq!(name_of(&s, 2), None, "nothing named it");
        let changed = s
            .infer_label(SrcNodeId(2), "First name")
            .expect("a node of the engine");
        assert!(changed.contains(&SrcNodeId(2)));
        assert_eq!(name_of(&s, 2).as_deref(), Some("First name"));
        let e = s.sem(SrcNodeId(2)).expect("entry");
        assert_eq!(e.prov.get(Group::Name), Some(ProvCode::Inferred));
        assert_ne!(
            ConfClass::from_code(e.hedge.get(Group::Name)),
            Some(ConfClass::Asserted),
            "hedged"
        );
        assert_eq!(
            TrustTier::from_code(e.taint.get(Group::Name)),
            Some(TrustTier::T3Untrusted)
        );
        // A primary name wins: the label is shadowed.
        s.infer_label(SrcNodeId(3), "Other");
        assert_eq!(name_of(&s, 3).as_deref(), Some("Email"));
        // A node the engine does not know is refused, not invented.
        assert!(s.infer_label(SrcNodeId(99), "x").is_none());
        // The primary's name arriving later supersedes the guess.
        s.apply(&[DocDelta::Upsert(vec![node(
            2,
            SemRole::TextBox,
            Some("Given name"),
        )])]);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Given name"));
    }

    /// M-3 storm bench (C3 review, pre-M22b): a streaming leaf renamed in three tree shapes, the
    /// fusion stage end to end (apply, run to quiescence). The counts are exact and asserted; the
    /// times are printed (`--nocapture`, use `--release` for the recorded numbers) and bounded
    /// loosely so a debug build passes.
    mod storm {
        use super::*;

        struct Tree {
            nodes: Vec<SrcNode>,
            kids: Vec<DocDelta>,
            leaf: u64,
        }

        fn leaf_node(id: u64, text: &str) -> SrcNode {
            node(id, SemRole::StaticText, Some(text))
        }

        /// 500 list items with a paragraph and 20 leaves each in the last; the last leaf streams.
        fn chat_log() -> Tree {
            let mut nodes = vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::List, None),
            ];
            let mut kids = vec![kids(1, &[2])];
            let mut items = Vec::new();
            let mut next = 10;
            let mut leaf = 0;
            for i in 0..500u64 {
                let item = next;
                next += 1;
                nodes.push(node(item, SemRole::ListItem, None));
                items.push(item);
                let wrapper = next;
                next += 1;
                nodes.push(node(wrapper, SemRole::Generic, None));
                let para = next;
                next += 1;
                nodes.push(node(para, SemRole::Paragraph, None));
                let n_leaves = if i == 499 { 20 } else { 1 };
                let mut ls = Vec::new();
                for _ in 0..n_leaves {
                    leaf = next;
                    next += 1;
                    nodes.push(leaf_node(leaf, "word"));
                    ls.push(leaf);
                }
                kids.push(self::kids(item, &[wrapper]));
                kids.push(self::kids(wrapper, &[para]));
                kids.push(self::kids(para, &ls));
            }
            kids.push(self::kids(2, &items));
            Tree { nodes, kids, leaf }
        }

        /// One list item over 8 nameless levels with fan-out 2: 256 leaves.
        fn deep_item() -> Tree {
            let mut nodes = vec![
                node(1, SemRole::DocumentWeb, None),
                node(2, SemRole::ListItem, None),
            ];
            let mut kids = vec![kids(1, &[2])];
            let mut level = vec![2u64];
            let mut next = 10;
            let mut leaf = 0;
            for depth in 0..8 {
                let mut below = Vec::new();
                for p in &level {
                    let a = next;
                    let b = next + 1;
                    next += 2;
                    let role = if depth == 7 {
                        SemRole::StaticText
                    } else {
                        SemRole::Generic
                    };
                    let name = (depth == 7).then_some("t");
                    nodes.push(node(a, role, name));
                    nodes.push(node(b, role, name));
                    kids.push(self::kids(*p, &[a, b]));
                    below.extend([a, b]);
                    leaf = b;
                }
                level = below;
            }
            Tree { nodes, kids, leaf }
        }

        /// 64 list items nested through nameless lists.
        fn nested_items() -> Tree {
            let mut nodes = vec![node(1, SemRole::DocumentWeb, None)];
            let mut kids = Vec::new();
            let mut parent = 1;
            let mut next = 10;
            for _ in 0..64 {
                let list = next;
                let item = next + 1;
                next += 2;
                nodes.push(node(list, SemRole::List, None));
                nodes.push(node(item, SemRole::ListItem, None));
                kids.push(self::kids(parent, &[list]));
                kids.push(self::kids(list, &[item]));
                parent = item;
            }
            let leaf = next;
            nodes.push(leaf_node(leaf, "tok"));
            kids.push(self::kids(parent, &[leaf]));
            Tree { nodes, kids, leaf }
        }

        fn settle(s: &mut FuseStage) {
            let mut guard = 0;
            while s.pending() > 0 && guard < 1_000 {
                s.step();
                guard += 1;
            }
        }

        /// (fusions per rename, mean time per rename) over `n` renames of the tree's leaf.
        fn measure(t: Tree, n: u32) -> (f64, std::time::Duration) {
            let mut s = FuseStage::new(setup());
            let mut batch = vec![reset(1), DocDelta::Upsert(t.nodes)];
            batch.extend(t.kids);
            s.apply(&batch);
            settle(&mut s);
            let before = s.stats().fusions;
            let start = std::time::Instant::now();
            for i in 0..n {
                let text = format!("tok{i}");
                s.apply(&[DocDelta::Upsert(vec![leaf_node(t.leaf, &text)])]);
                settle(&mut s);
            }
            let dt = start.elapsed() / n;
            let delta = u32::try_from(s.stats().fusions - before).unwrap_or(u32::MAX);
            let per = f64::from(delta) / f64::from(n);
            assert_eq!(s.stats().errors, 0);
            (per, dt)
        }

        #[test]
        fn a_streaming_leaf_costs_a_bounded_number_of_fusions() {
            let (chat, t_chat) = measure(chat_log(), 200);
            let (deep, t_deep) = measure(deep_item(), 200);
            let (nested, t_nested) = measure(nested_items(), 200);
            eprintln!("M-3 chat log     : {chat:.1} fusions/rename, {t_chat:?}/rename");
            eprintln!("M-3 deep item    : {deep:.1} fusions/rename, {t_deep:?}/rename");
            eprintln!("M-3 nested items : {nested:.1} fusions/rename, {t_nested:?}/rename");
            // The counts (2.0, 2.0, 65: one fusion per reader whose name really changes; was 3.0, 3.0, 130): a regression that widens the cascade fails.
            assert!(chat <= 3.0, "{chat}");
            assert!(deep <= 3.0, "{deep}");
            assert!(nested <= 70.0, "{nested}");
            // 10k events/s is 100 us per event; the worst shape gets a debug-build allowance.
            let limit = std::time::Duration::from_millis(2);
            assert!(t_chat < limit && t_deep < limit && t_nested < limit);
        }
    }

    // ---- plugins (M18a) -----------------------------------------------------------------

    use crate::plugin_port::testing::{svc, svc_with};

    fn with_class(mut n: SrcNode, class: &str) -> SrcNode {
        let mut r = n.rec.as_deref().cloned().unwrap_or_default();
        r.class = Some(Arc::from(class));
        n.rec = Some(Arc::new(r));
        n
    }

    fn hint(url: &str) -> DocDelta {
        DocDelta::Hint(DocHint::RouteChanged {
            url: Arc::from(url),
        })
    }

    fn chat_doc(child_text: &str) -> Vec<DocDelta> {
        vec![
            DocDelta::Upsert(vec![
                node(1, SemRole::DocumentWeb, None),
                with_class(node(2, SemRole::Button, Some("AL Alice 15:00")), "watch"),
                node(3, SemRole::StaticText, Some(child_text)),
            ]),
            kids(1, &[2]),
            kids(2, &[3]),
        ]
    }

    #[test]
    fn a_plugin_patch_is_in_the_first_published_entry_and_follows_a_real_change_only() {
        let (service, jobs) = svc(false);
        let mut s = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        let mut batch = vec![reset(1)];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        // S1 beat the observed name, in the same publish as the observation.
        assert_eq!(name_of(&s, 2).as_deref(), Some("Alice!"));
        assert_eq!(*jobs.lock().unwrap(), 1);
        // Unrelated churn does not call the plugin again (PF-4).
        s.apply(&[DocDelta::Upsert(vec![node(1, SemRole::DocumentWeb, None)])]);
        assert_eq!(*jobs.lock().unwrap(), 1);
        // The child the call read changed: one more call, new value.
        s.apply(&[DocDelta::Upsert(vec![node(
            3,
            SemRole::StaticText,
            Some("Bob"),
        )])]);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Bob!"));
        assert_eq!(*jobs.lock().unwrap(), 2);
        // The same row again is not a change.
        s.apply(&[DocDelta::Upsert(vec![node(
            3,
            SemRole::StaticText,
            Some("Bob"),
        )])]);
        assert_eq!(*jobs.lock().unwrap(), 2);
    }

    #[test]
    fn a_scope_that_needs_a_url_stays_closed_until_the_document_reports_one() {
        let (service, jobs) = svc(true);
        let mut s = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        let mut batch = vec![reset(1)];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        assert_eq!(name_of(&s, 2).as_deref(), Some("AL Alice 15:00"));
        assert_eq!(*jobs.lock().unwrap(), 0);
        // A new document with a URL opens it.
        let mut batch = vec![reset(1), hint("https://web.telegram.org/a/")];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Alice!"));
    }

    #[test]
    fn a_removed_node_and_a_new_document_leave_no_patch_behind() {
        let (service, _) = svc(false);
        let mut s = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        let mut batch = vec![reset(1)];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        s.apply(&[DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec::SmallVec::new(),
        }]);
        assert!(s.sem(SrcNodeId(2)).is_none());
        s.apply(&[
            reset(9),
            DocDelta::Upsert(vec![node(9, SemRole::DocumentWeb, None)]),
        ]);
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn a_url_that_arrives_after_the_first_block_activates_the_plugin_for_what_is_there() {
        let (service, jobs) = svc(true);
        let mut s = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        // The first block is published at once, without the URL: nothing is patched.
        let mut batch = vec![reset(1)];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        assert_eq!(name_of(&s, 2).as_deref(), Some("AL Alice 15:00"));
        assert_eq!(*jobs.lock().unwrap(), 0);
        // The hint comes as a batch of its own; the plugin then derives the nodes already in the
        // document and its facts are a refinement of the entry.
        let changed = s.apply(&[hint("https://web.telegram.org/a/")]);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Alice!"));
        assert!(changed.contains(&SrcNodeId(2)), "{changed:?}");
        assert_eq!(*jobs.lock().unwrap(), 1);
        // A removed node leaves nothing waiting for a scope that never opens.
        let (service, _) = svc(true);
        let mut s2 = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        s2.apply(&batch);
        s2.apply(&[DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec::SmallVec::new(),
        }]);
        s2.apply(&[hint("https://web.telegram.org/a/")]);
        assert!(s2.sem(SrcNodeId(2)).is_none());
    }

    #[test]
    fn an_unverified_document_gets_no_url_scoped_plugin() {
        // The service refuses an unverified scope; the stage then runs exactly as without.
        let (service, jobs) = svc_with(true, true);
        let mut s =
            FuseStage::new(setup()).with_plugins(service, Arc::from("web.telegram.org"), 1, false);
        let mut batch = vec![reset(1), hint("https://web.telegram.org/a/")];
        batch.extend(chat_doc("Alice"));
        s.apply(&batch);
        assert_eq!(name_of(&s, 2).as_deref(), Some("AL Alice 15:00"));
        assert_eq!(*jobs.lock().unwrap(), 0);
        // The same page from a verified peer is served.
        let (service, _) = svc_with(true, true);
        let mut s = FuseStage::new(setup()).with_plugins(service, Arc::from("firefox"), 1, true);
        s.apply(&batch);
        assert_eq!(name_of(&s, 2).as_deref(), Some("Alice!"));
    }
}
