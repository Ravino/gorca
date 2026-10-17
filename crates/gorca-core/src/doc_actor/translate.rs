//! The translator: a settled batch of `DocDelta`s becomes `DocOp`s on the document mirror
//! (browse-mode design section 4.2, TR-1 to TR-6).
//!
//! The batch is first *collected* (upserts, child lists, removals, text edits), then applied in a
//! fixed order that never depends on the order the source emitted:
//!
//! 1. `Reset`: a new document in the same arena. The root node stays; its old children are
//!    detached by the first `SyncChildren` and reaped at the end, so every old id becomes `Gone`.
//! 2. Child lists whose parent is in the mirror become one `SyncChildren` each. New nodes are
//!    assembled bottom-up into [`SubtreeSpec`] forests first (TR-2): a 100k-node first block is one
//!    bulk build, not 100k inserts. A listed id with no upsert yet becomes a placeholder
//!    (`Role::Unknown`, TR-3) that a later upsert fills in place.
//! 3. Removals detach subtrees that no list kept.
//! 4. Property and text changes of known nodes (`SetProps`, `SetText`).
//! 5. Block breaks (FT-4) are recomputed for every parent whose child list or whose children's
//!    block-ness changed, and object markers (FT-6) for the touched nodes and their ancestors.
//! 6. Everything still detached is reaped; the source's ids and transport keys of the dead
//!    nodes leave the maps (TR-4 without the grace, which is M7b).
//!
//! The translator never trusts the source (SL-3): a rejected op aborts the batch, the maps are
//! rebuilt from the mirror, and the actor asks the source for a rebuild.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gorca_doc::web::{
    ChildSpec, DocError, DocOp, DocWriter, NodeFlags, NodeMeta, NodeSpec, PropsPatch, SubtreeSpec,
    WebDoc,
};
use gorca_doc_source_api::web_attrs;
use gorca_model::{
    AttrKey, DocDelta, DocNodeId, LocalId, Role, SrcNode, SrcNodeId, StateSet, TextEdit,
};

use gorca_doc_source_api::limits::{
    MAX_CHILDREN, MAX_DOC_NODES, MAX_NODE_TEXT_BYTES, MAX_PENDING_IDS as MAX_PENDING,
};

/// Text cut at [`MAX_NODE_TEXT_BYTES`], process wide (text clipping happens in free functions).
static TEXT_CLIPPED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Clip a node's text to the cap; the original `Arc` is reused when nothing is cut.
fn clip_arc(t: Option<&Arc<str>>) -> Option<Arc<str>> {
    match t {
        Some(a) if a.len() > MAX_NODE_TEXT_BYTES => {
            TEXT_CLIPPED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let mut s = a.to_string();
            gorca_doc_source_api::limits::clip_text(&mut s, MAX_NODE_TEXT_BYTES);
            Some(Arc::from(s))
        }
        other => other.cloned(),
    }
}
/// Deepest node the mirror keeps (a page nests a few dozen levels; 512 is far beyond any real
/// one). Deeper content is cut and counted, never recursed into.
pub const MAX_DEPTH: u32 = gorca_doc_source_api::limits::MAX_DEPTH;

/// Touched nodes reported per batch.
const TOUCHED_CAP: usize = 8192;

/// What a batch changed, for the actor (join table, notices).
#[derive(Debug, Default)]
pub struct BatchOutcome {
    /// Transport keys that now name a node: insert after the publish (JN-1).
    pub join_add: Vec<(LocalId, DocNodeId)>,
    /// Transport keys whose node left the mirror.
    pub join_del: Vec<LocalId>,
    pub created: usize,
    pub removed: usize,
    /// The batch started a new document (`Reset`).
    pub reset: bool,
    /// Nodes the batch created, changed or whose child list it changed (capped), for the
    /// live-region tracker.
    pub touched: Vec<DocNodeId>,
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct TranslatorStats {
    pub batches: u64,
    pub rejected: u64,
    pub placeholders: u64,
    pub orphans_dropped: u64,
    /// Nodes cut by the depth, fan-out, node-count or pending-list caps.
    pub truncated: u64,
    /// Texts cut at the per-node byte cap (process wide).
    pub text_clipped: u64,
}

#[derive(Debug, Default)]
pub struct Translator {
    by_src: HashMap<SrcNodeId, DocNodeId>,
    orphans: HashMap<SrcNodeId, SrcNode>,
    deferred: HashMap<SrcNodeId, Vec<SrcNodeId>>,
    stats: TranslatorStats,
}

/// One batch, collected.
#[derive(Debug, Default)]
struct Collected {
    reset: Option<SrcNodeId>,
    upserts: Vec<SrcNode>,
    upsert_ix: HashMap<SrcNodeId, usize>,
    lists: HashMap<SrcNodeId, Vec<SrcNodeId>>,
    list_order: Vec<SrcNodeId>,
    removed: Vec<SrcNodeId>,
    texts: Vec<(SrcNodeId, TextEdit)>,
}

impl Collected {
    fn take_node(&mut self, id: SrcNodeId) -> Option<SrcNode> {
        let ix = self.upsert_ix.get(&id).copied()?;
        // Leave a hole the later passes skip; indexes stay valid.
        Some(std::mem::replace(
            &mut self.upserts[ix],
            placeholder_node(SrcNodeId(u64::MAX)),
        ))
        .filter(|n| n.id == id)
    }
}

fn placeholder_node(id: SrcNodeId) -> SrcNode {
    SrcNode {
        id,
        role: Role::Unknown,
        states: StateSet::VISIBLE,
        name: None,
        text: None,
        attrs: smallvec::SmallVec::default(),
        local: None,
    }
}

fn attr(n: &SrcNode, k: AttrKey) -> Option<&str> {
    n.attrs.iter().find(|(a, _)| *a == k).map(|(_, v)| &**v)
}

fn has_token(list: Option<&str>, tok: &str) -> bool {
    list.is_some_and(|l| l.split_ascii_whitespace().any(|t| t == tok))
}

/// CSS `display` values that start a new line (FT-4). Everything else is inline-level.
fn display_is_block(d: &str) -> bool {
    matches!(
        d,
        "block"
            | "list-item"
            | "table"
            | "table-row"
            | "table-row-group"
            | "table-header-group"
            | "table-footer-group"
            | "table-cell"
            | "table-caption"
            | "flex"
            | "grid"
            | "flow-root"
            | "-webkit-box"
            | "details"
    )
}

fn role_is_block(r: Role) -> bool {
    matches!(
        r,
        Role::Paragraph
            | Role::Heading
            | Role::List
            | Role::ListItem
            | Role::Table
            | Role::Row
            | Role::Landmark
            | Role::Separator
            | Role::Document
            | Role::DocumentWeb
            | Role::Dialog
            | Role::Alert
    )
}

/// The node flags that follow from the source's data (everything but the break flags).
pub fn derive_flags(n: &SrcNode, nested_doc: bool) -> (NodeFlags, u8) {
    let mut f = NodeFlags::NONE;
    // `showing` is viewport state and flips without events (TBD-WEB-1): only `visible` counts.
    if !n.states.contains(StateSet::VISIBLE) {
        f = f | NodeFlags::HIDDEN;
    }
    if n.states.contains(StateSet::VISITED) {
        f = f | NodeFlags::VISITED;
    }
    if n.states.contains(StateSet::FOCUSABLE) {
        f = f | NodeFlags::FOCUSABLE;
    }
    let tag = attr(n, web_attrs::TAG);
    let xml = attr(n, web_attrs::XML_ROLES);
    if n.role == Role::Landmark && (has_token(xml, "region") || tag == Some("section")) {
        f = f | NodeFlags::REGION;
    }
    if tag == Some("article") || has_token(xml, "article") {
        f = f | NodeFlags::ARTICLE;
    }
    if tag == Some("blockquote") {
        f = f | NodeFlags::BLOCKQUOTE;
    }
    if n.role == Role::Table && attr(n, web_attrs::LAYOUT_GUESS) == Some("true") {
        f = f | NodeFlags::LAYOUT;
    }
    if matches!(attr(n, web_attrs::LIVE), Some("polite" | "assertive"))
        || ["status", "log", "marquee", "timer"]
            .iter()
            .any(|t| has_token(xml, t))
    {
        f = f | NodeFlags::LIVE;
    }
    if n.role == Role::List && (has_token(xml, "listbox") || tag == Some("select")) {
        f = f | NodeFlags::LISTBOX;
    }
    if matches!(tag, Some("ins" | "del"))
        || ["comment", "suggestion", "insertion", "deletion"]
            .iter()
            .any(|t| has_token(xml, t))
    {
        f = f | NodeFlags::ANNOTATION;
    }
    // A form field that is invalid is announced as the field; W finds the marked-up text.
    if n.states.contains(StateSet::INVALID)
        && !matches!(
            n.role,
            Role::TextEntry
                | Role::PasswordEntry
                | Role::ComboBox
                | Role::SpinButton
                | Role::Slider
        )
    {
        f = f | NodeFlags::ERROR;
    }
    if n.role == Role::DocumentWeb && nested_doc {
        f = f | NodeFlags::FRAME;
    }
    // Table cells stay inline: a row is one line, cells separated by a space (NVDA screen layout).
    let cell = matches!(n.role, Role::Cell | Role::ColumnHeader | Role::RowHeader);
    if cell {
        f = f | NodeFlags::CELL;
    }
    let block = !cell
        && match attr(n, web_attrs::DISPLAY) {
            Some(d) => display_is_block(d),
            None => role_is_block(n.role),
        };
    if block {
        f = f | NodeFlags::BLOCK;
    }
    let level = if n.role == Role::Heading {
        attr(n, web_attrs::LEVEL)
            .and_then(|l| l.parse::<u8>().ok())
            .filter(|l| (1..=6).contains(l))
            .unwrap_or(0)
    } else {
        0
    };
    (f, level)
}

fn meta_of(n: &SrcNode) -> Arc<NodeMeta> {
    Arc::new(NodeMeta {
        src: Some(n.id),
        local: n.local,
        states: n.states,
        description: n
            .attrs
            .iter()
            .find(|(k, _)| *k == web_attrs::DESCRIPTION)
            .map(|(_, v)| Arc::clone(v)),
        value: None,
        attrs: n.attrs.clone(),
    })
}

/// A node's mirror spec (without break flags, which depend on the siblings).
pub fn spec_of(n: &SrcNode, nested_doc: bool) -> NodeSpec {
    let (flags, level) = derive_flags(n, nested_doc);
    let mut s = NodeSpec::new(n.role).level(level).flags(flags);
    // A page can put a megabyte in aria-label: the translator applies the source's cap again.
    s.name = n.name.as_ref().map(|a| {
        if a.len() > gorca_doc_source_api::limits::MAX_NAME_BYTES {
            let mut t = a.to_string();
            gorca_doc_source_api::limits::clip_text(
                &mut t,
                gorca_doc_source_api::limits::MAX_NAME_BYTES,
            );
            Arc::from(t)
        } else {
            Arc::clone(a)
        }
    });
    s.text = clip_arc(n.text.as_ref());
    s.meta(meta_of(n))
}

fn without_breaks(f: NodeFlags) -> NodeFlags {
    NodeFlags(f.0 & !(NodeFlags::BREAK_BEFORE.0 | NodeFlags::CELL_SEP.0))
}

/// FT-4 over one sibling list: a line break before a block and before whatever follows one.
/// Hidden siblings are skipped. Returns the new flags.
pub fn assign_breaks(flags: &mut [NodeFlags]) {
    let mut prev_block = false;
    let mut seen = false;
    for f in flags.iter_mut() {
        let hidden = f.contains(NodeFlags::HIDDEN);
        let block = f.contains(NodeFlags::BLOCK);
        let want = !hidden && (block || prev_block);
        let gap = !hidden && !want && seen && f.contains(NodeFlags::CELL);
        *f = if want {
            without_breaks(*f) | NodeFlags::BREAK_BEFORE
        } else if gap {
            without_breaks(*f) | NodeFlags::CELL_SEP
        } else {
            without_breaks(*f)
        };
        if !hidden {
            prev_block = block;
            seen = true;
        }
    }
}

/// Roles that get a U+FFFC object marker when nothing else speaks for them (FT-6).
fn marker_role(r: Role) -> bool {
    matches!(
        r,
        Role::Image
            | Role::CheckBox
            | Role::RadioButton
            | Role::ToggleButton
            | Role::Button
            | Role::TextEntry
            | Role::PasswordEntry
            | Role::ComboBox
            | Role::Slider
            | Role::SpinButton
            | Role::ProgressBar
    )
}

struct Built {
    spec: SubtreeSpec,
    /// Source ids of the spec's nodes in preorder (zip with `OpOutput::created`).
    order: Vec<SrcNodeId>,
}

impl Translator {
    pub fn new() -> Self {
        Self::default()
    }

    #[cfg(test)]
    pub fn stats(&self) -> TranslatorStats {
        TranslatorStats {
            text_clipped: TEXT_CLIPPED.load(std::sync::atomic::Ordering::Relaxed),
            ..self.stats
        }
    }

    /// Count `n` cut nodes; the first cut of a document is logged (once, no content).
    fn cut(&mut self, n: u64) {
        if self.stats.truncated == 0 && n > 0 {
            tracing::warn!("document exceeded a mirror cap; excess nodes are cut and counted");
        }
        self.stats.truncated = self.stats.truncated.saturating_add(n);
    }

    /// The mirror id of a source id.
    #[cfg(test)]
    pub fn id_of(&self, src: SrcNodeId) -> Option<DocNodeId> {
        self.by_src.get(&src).copied()
    }

    #[cfg(test)]
    pub fn known(&self) -> usize {
        self.by_src.len()
    }

    /// Rebuild the id map from the mirror (after a rejected batch, or a copy-on-publish clone).
    pub fn resync(&mut self, doc: &WebDoc) {
        self.by_src.clear();
        for id in doc.preorder() {
            if let Some(src) = doc.node(id).and_then(|v| v.meta).and_then(|m| m.src) {
                self.by_src.insert(src, id);
            }
        }
    }

    fn collect(batch: Vec<DocDelta>) -> Collected {
        let mut c = Collected::default();
        for d in batch {
            match d {
                DocDelta::Reset { root, .. } => c.reset = Some(root),
                DocDelta::Upsert(nodes) => {
                    for n in nodes {
                        // Last wins within a batch.
                        if let Some(ix) = c.upsert_ix.get(&n.id).copied() {
                            c.upserts[ix] = n;
                        } else {
                            c.upsert_ix.insert(n.id, c.upserts.len());
                            c.upserts.push(n);
                        }
                    }
                }
                DocDelta::Children { parent, children } => {
                    if c.lists.insert(parent, children.to_vec()).is_none() {
                        c.list_order.push(parent);
                    }
                }
                DocDelta::Text { node, edit } => c.texts.push((node, edit)),
                DocDelta::Removed(ids) => c.removed.extend(ids),
                DocDelta::Hint(_) | DocDelta::Settled { .. } => {}
            }
        }
        c
    }

    /// Apply one settled batch. On `Err` the maps were rebuilt from the mirror and the caller
    /// should ask the source for a rebuild.
    pub fn apply(
        &mut self,
        w: &mut DocWriter,
        batch: Vec<DocDelta>,
    ) -> Result<BatchOutcome, DocError> {
        self.stats.batches += 1;
        let r = self.apply_inner(w, batch);
        if r.is_err() {
            self.stats.rejected += 1;
            let snapshot = w.doc().clone();
            self.resync(&snapshot);
            self.orphans.clear();
            self.deferred.clear();
        }
        r
    }

    fn apply_inner(
        &mut self,
        w: &mut DocWriter,
        batch: Vec<DocDelta>,
    ) -> Result<BatchOutcome, DocError> {
        let mut c = Self::collect(batch);
        let mut out = BatchOutcome::default();
        let mut touched: HashSet<DocNodeId> = HashSet::new();
        let mut break_parents: HashSet<DocNodeId> = HashSet::new();

        // Orphans and deferred lists from earlier batches join this one.
        for (id, n) in self.orphans.drain() {
            if !c.upsert_ix.contains_key(&id) {
                c.upsert_ix.insert(id, c.upserts.len());
                c.upserts.push(n);
            }
        }
        for (p, list) in self.deferred.drain() {
            if !c.lists.contains_key(&p) {
                c.list_order.push(p);
                c.lists.insert(p, list);
            }
        }

        // 1. A new document: the root stays, the rest of the tree goes.
        if let Some(root_src) = c.reset {
            out.reset = true;
            self.begin_document(w, root_src, &mut c, &mut out, &mut touched)?;
        }

        // 2. Child lists, parents first: a list whose parent is mirrored is applied now; the
        // lists of nodes created on the way are consumed while their subtree is assembled.
        let order = c.list_order.clone();
        for p_src in order {
            let Some(p) = self.by_src.get(&p_src).copied() else {
                continue;
            };
            let Some(list) = c.lists.remove(&p_src) else {
                continue;
            };
            self.sync_children(w, p, &list, &mut c, &mut out, &mut touched)?;
            break_parents.insert(p);
        }
        // Lists whose parent is still unknown wait for it.
        if !c.lists.is_empty() {
            let held: usize =
                self.deferred.values().map(Vec::len).sum::<usize>() + self.orphans.len();
            let mut room = MAX_PENDING.saturating_sub(held);
            for (p, mut list) in c.lists.drain() {
                list.truncate(MAX_CHILDREN);
                if list.len() <= room {
                    room -= list.len();
                    self.deferred.insert(p, list);
                } else {
                    self.stats.orphans_dropped += 1;
                }
            }
        }

        // 3. Removals nothing kept.
        for src in std::mem::take(&mut c.removed) {
            let Some(id) = self.by_src.get(&src).copied() else {
                self.orphans.remove(&src);
                continue;
            };
            let doc = w.doc();
            if !doc.is_attached(id) || doc.root() == Some(id) {
                continue;
            }
            if let Some(p) = doc.parent(id) {
                break_parents.insert(p);
            }
            w.apply(DocOp::RemoveSubtree { id })?;
            out.removed += 1;
        }

        // 4. Props and text of nodes that already existed.
        let upserts = std::mem::take(&mut c.upserts);
        // One budget for both pending pools (N4): parked ids plus parked orphans.
        let mut held: usize =
            self.deferred.values().map(Vec::len).sum::<usize>() + self.orphans.len();
        for n in upserts {
            if n.id.0 == u64::MAX {
                continue; // consumed by a subtree build
            }
            match self.by_src.get(&n.id).copied() {
                Some(id) => {
                    Self::update_known(w, id, &n, &mut out, &mut touched, &mut break_parents)?;
                }
                None => {
                    if held < MAX_PENDING {
                        self.orphans.insert(n.id, n);
                        held += 1;
                    } else {
                        self.stats.orphans_dropped += 1;
                    }
                }
            }
        }
        for (src, edit) in std::mem::take(&mut c.texts) {
            if let Some(id) = self.by_src.get(&src).copied() {
                Self::apply_text_edit(w, id, &edit, &mut touched)?;
            } else if let Some(n) = self.orphans.get_mut(&src)
                && let TextEdit::Replace(t) = edit
            {
                n.text = Some(t);
            }
        }

        // 5. Breaks, then markers.
        for p in break_parents {
            if w.doc().is_attached(p) {
                Self::recompute_breaks(w, p)?;
            }
        }
        Self::markers(w, &touched)?;

        out.touched = touched.iter().copied().take(TOUCHED_CAP).collect();
        // 6. Reap what is still detached.
        self.reap(w, &mut out)?;
        Ok(out)
    }

    /// Depth of `id` (root = 0), walking the parent links with a step budget so a corrupt chain
    /// cannot loop; saturates just above [`MAX_DEPTH`].
    fn depth_of(doc: &WebDoc, id: DocNodeId) -> u32 {
        let mut d = 0;
        let mut cur = doc.parent(id);
        while let Some(p) = cur {
            d += 1;
            if d > MAX_DEPTH {
                break;
            }
            cur = doc.parent(p);
        }
        d
    }

    fn nested_doc(doc: &WebDoc, parent: DocNodeId) -> bool {
        let mut cur = Some(parent);
        let mut hops = 0;
        while let Some(p) = cur {
            if doc.node(p).is_some_and(|v| v.role == Role::DocumentWeb) {
                return true;
            }
            cur = doc.parent(p);
            hops += 1;
            if hops > 512 {
                break;
            }
        }
        false
    }

    fn begin_document(
        &mut self,
        w: &mut DocWriter,
        root_src: SrcNodeId,
        c: &mut Collected,
        out: &mut BatchOutcome,
        touched: &mut HashSet<DocNodeId>,
    ) -> Result<(), DocError> {
        let root_node = c
            .take_node(root_src)
            .or_else(|| self.orphans.remove(&root_src))
            .unwrap_or_else(|| placeholder_node(root_src));
        let existing = w.doc().root();
        match existing {
            None => {
                // First document: build the root with whatever children list came with it.
                let built = self.build_spec(&root_node, false, c, true, 0, 0);
                let o = w.apply(DocOp::InsertSubtree {
                    parent: None,
                    before: None,
                    subtree: Arc::new(built.spec),
                })?;
                self.register_created(w.doc(), &built.order, &o.created, out, touched);
            }
            Some(root_id) => {
                // Navigation: forget every old source id, keep the arena and the root node.
                let old_locals: Vec<LocalId> = {
                    let doc = w.doc();
                    doc.preorder()
                        .filter(|i| *i != root_id)
                        .filter_map(|i| doc.node(i).and_then(|v| v.meta).and_then(|m| m.local))
                        .collect()
                };
                out.join_del.extend(old_locals);
                if let Some(old_local) = w
                    .doc()
                    .node(root_id)
                    .and_then(|v| v.meta)
                    .and_then(|m| m.local)
                {
                    out.join_del.push(old_local);
                }
                self.by_src.clear();
                self.orphans.clear();
                self.deferred.clear();
                self.by_src.insert(root_src, root_id);
                let spec = spec_of(&root_node, false);
                let patch = PropsPatch {
                    role: Some(spec.role),
                    level: Some(spec.level),
                    flags: Some(without_breaks(spec.flags)),
                    name: Some(spec.name.clone()),
                    meta: Some(spec.meta.clone()),
                    ..PropsPatch::default()
                };
                w.apply(DocOp::SetProps { id: root_id, patch })?;
                w.apply(DocOp::SetText {
                    id: root_id,
                    text: spec.text,
                })?;
                if let Some(l) = root_node.local {
                    out.join_add.push((l, root_id));
                }
                // The first list of the new root detaches the old children.
                if let std::collections::hash_map::Entry::Vacant(e) = c.lists.entry(root_src) {
                    e.insert(Vec::new());
                    c.list_order.push(root_src);
                }
                touched.insert(root_id);
            }
        }
        Ok(())
    }

    /// Assemble a spec for `node` and the new children its lists name, iteratively: the source
    /// is untrusted, so a page 100k levels deep or a list cycle must cost time, not the stack
    /// (SL-3). `depth` is the node's depth in the document; anything deeper than [`MAX_DEPTH`]
    /// is cut and counted. Each list is consumed when read, so a cycle ends by itself.
    fn build_spec(
        &mut self,
        node: &SrcNode,
        nested: bool,
        c: &mut Collected,
        is_root: bool,
        depth: u32,
        already: usize,
    ) -> Built {
        struct Frame {
            spec: NodeSpec,
            kids: std::vec::IntoIter<SrcNodeId>,
            children: Vec<SubtreeSpec>,
            nested: bool,
            depth: u32,
        }
        fn open(
            this: &mut Translator,
            c: &mut Collected,
            order: &mut Vec<SrcNodeId>,
            n: &SrcNode,
            (nested, depth, root): (bool, u32, bool),
        ) -> Frame {
            let mut spec = spec_of(n, nested);
            if root {
                spec.flags = without_breaks(spec.flags);
            }
            order.push(n.id);
            let mut kids = c
                .lists
                .remove(&n.id)
                .or_else(|| this.deferred.remove(&n.id))
                .unwrap_or_default();
            if kids.len() > MAX_CHILDREN {
                this.cut((kids.len() - MAX_CHILDREN) as u64);
                kids.truncate(MAX_CHILDREN);
            }
            Frame {
                spec,
                kids: kids.into_iter(),
                children: Vec::new(),
                nested: nested || n.role == Role::DocumentWeb,
                depth,
            }
        }
        let mut order: Vec<SrcNodeId> = Vec::new();
        let mut stack = vec![open(self, c, &mut order, node, (nested, depth, is_root))];
        loop {
            let Some(top) = stack.last_mut() else {
                // Unreachable: the root frame is only popped below, which returns.
                return Built {
                    spec: SubtreeSpec::leaf(NodeSpec::new(Role::Unknown)),
                    order,
                };
            };
            if let Some(k) = top.kids.next() {
                if top.depth + 1 > MAX_DEPTH
                    || self.by_src.len() + already + order.len() >= MAX_DOC_NODES
                {
                    self.cut(1);
                    c.take_node(k);
                    self.orphans.remove(&k);
                    continue;
                }
                let (child_nested, child_depth) = (top.nested, top.depth + 1);
                let data = c
                    .take_node(k)
                    .or_else(|| self.orphans.remove(&k))
                    .unwrap_or_else(|| {
                        self.stats.placeholders += 1;
                        placeholder_node(k)
                    });
                let f = open(
                    self,
                    c,
                    &mut order,
                    &data,
                    (child_nested, child_depth, false),
                );
                stack.push(f);
                continue;
            }
            let Some(mut done) = stack.pop() else {
                continue;
            };
            let mut flags: Vec<NodeFlags> = done.children.iter().map(|s| s.node.flags).collect();
            assign_breaks(&mut flags);
            for (s, f) in done.children.iter_mut().zip(flags) {
                s.node.flags = f;
            }
            let spec = SubtreeSpec::with_children(done.spec, done.children);
            match stack.last_mut() {
                Some(parent) => parent.children.push(spec),
                None => return Built { spec, order },
            }
        }
    }

    /// Record the ids the mirror assigned to newly built nodes.
    fn register_created(
        &mut self,
        doc: &WebDoc,
        order: &[SrcNodeId],
        created: &[DocNodeId],
        out: &mut BatchOutcome,
        touched: &mut HashSet<DocNodeId>,
    ) {
        for (src, id) in order.iter().zip(created) {
            self.by_src.insert(*src, *id);
            touched.insert(*id);
            if let Some(l) = doc.node(*id).and_then(|v| v.meta).and_then(|m| m.local) {
                out.join_add.push((l, *id));
            }
        }
        out.created += created.len();
    }

    fn sync_children(
        &mut self,
        w: &mut DocWriter,
        parent: DocNodeId,
        list: &[SrcNodeId],
        c: &mut Collected,
        out: &mut BatchOutcome,
        touched: &mut HashSet<DocNodeId>,
    ) -> Result<(), DocError> {
        let nested = Self::nested_doc(w.doc(), parent);
        let child_depth = Self::depth_of(w.doc(), parent) + 1;
        let mut desired: Vec<ChildSpec> = Vec::with_capacity(list.len());
        let mut new_orders: Vec<Vec<SrcNodeId>> = Vec::new();
        let mut seen: HashSet<SrcNodeId> = HashSet::with_capacity(list.len().min(MAX_CHILDREN));
        let mut moved: Vec<DocNodeId> = Vec::new();
        if list.len() > MAX_CHILDREN {
            self.cut((list.len() - MAX_CHILDREN) as u64);
        }
        for k in list.iter().take(MAX_CHILDREN) {
            if !seen.insert(*k) {
                return Err(DocError::Duplicate(
                    self.by_src.get(k).copied().unwrap_or(parent),
                ));
            }
            match self.by_src.get(k).copied() {
                Some(id) if w.doc().contains(id) => {
                    if w.doc().parent(id) != Some(parent) {
                        moved.push(id);
                    }
                    desired.push(ChildSpec::Existing(id));
                }
                Some(_) | None => {
                    let built_now: usize = new_orders.iter().map(Vec::len).sum();
                    if child_depth > MAX_DEPTH || self.by_src.len() + built_now >= MAX_DOC_NODES {
                        self.cut(1);
                        continue;
                    }
                    let data = c
                        .take_node(*k)
                        .or_else(|| self.orphans.remove(k))
                        .unwrap_or_else(|| {
                            self.stats.placeholders += 1;
                            placeholder_node(*k)
                        });
                    let built = self.build_spec(&data, nested, c, false, child_depth, built_now);
                    new_orders.push(built.order);
                    desired.push(ChildSpec::New(Arc::new(built.spec)));
                }
            }
        }
        let o = w.apply(DocOp::SyncChildren { parent, desired })?;
        // `created` is the preorder of every new subtree, in desired order.
        let mut at = 0;
        for order in &new_orders {
            let end = (at + order.len()).min(o.created.len());
            self.register_created(w.doc(), order, &o.created[at..end], out, touched);
            at = end;
        }
        touched.insert(parent);
        self.enforce_depth_of_moved(w, &moved, child_depth)?;
        Ok(())
    }

    /// M1: a moved subtree keeps its height, so it can land deeper than [`MAX_DEPTH`]. Walk each
    /// moved subtree (iteratively) from its new depth and remove what is too deep.
    fn enforce_depth_of_moved(
        &mut self,
        w: &mut DocWriter,
        moved: &[DocNodeId],
        child_depth: u32,
    ) -> Result<(), DocError> {
        let mut too_deep: Vec<DocNodeId> = Vec::new();
        for &top in moved {
            let doc = w.doc();
            let mut stack: Vec<(DocNodeId, u32)> = vec![(top, child_depth)];
            let mut steps = 0usize;
            while let Some((n, d)) = stack.pop() {
                steps += 1;
                if steps > MAX_DOC_NODES {
                    break;
                }
                if d > MAX_DEPTH {
                    too_deep.push(n);
                    continue;
                }
                for k in doc.children(n) {
                    stack.push((k, d + 1));
                }
            }
        }
        for id in too_deep {
            if w.doc().contains(id) && w.doc().root() != Some(id) {
                w.apply(DocOp::RemoveSubtree { id })?;
                self.cut(1);
            }
        }
        Ok(())
    }

    fn update_known(
        w: &mut DocWriter,
        id: DocNodeId,
        n: &SrcNode,
        out: &mut BatchOutcome,
        touched: &mut HashSet<DocNodeId>,
        break_parents: &mut HashSet<DocNodeId>,
    ) -> Result<(), DocError> {
        let doc = w.doc();
        let Some(cur) = doc.node(id) else {
            return Ok(());
        };
        let nested =
            n.role == Role::DocumentWeb && doc.parent(id).is_some_and(|p| Self::nested_doc(doc, p));
        let spec = spec_of(n, nested);
        let keep_breaks = cur.flags.0 & (NodeFlags::BREAK_BEFORE.0 | NodeFlags::CELL_SEP.0);
        let new_flags = NodeFlags(without_breaks(spec.flags).0 | keep_breaks);
        let mut patch = PropsPatch::default();
        let mut any = false;
        if cur.role != spec.role {
            patch.role = Some(spec.role);
            any = true;
        }
        if cur.level != spec.level {
            patch.level = Some(spec.level);
            any = true;
        }
        if cur.flags != new_flags {
            let changed = cur.flags.0 ^ new_flags.0;
            if changed & (NodeFlags::BLOCK.0 | NodeFlags::HIDDEN.0) != 0
                && let Some(p) = doc.parent(id)
            {
                break_parents.insert(p);
            }
            patch.flags = Some(new_flags);
            any = true;
        }
        if cur.name != spec.name.as_deref() {
            patch.name = Some(spec.name.clone());
            any = true;
        }
        let old_local = cur.meta.and_then(|m| m.local);
        let meta_same = cur.meta.is_some_and(|m| {
            m.src == Some(n.id) && m.local == n.local && m.states == n.states && m.attrs == n.attrs
        });
        if !meta_same {
            patch.meta = Some(spec.meta.clone());
            any = true;
        }
        let cur_text = cur.text;
        let want_text: Option<&str> = n.text.as_deref();
        let keeps_marker = cur_text == Some("\u{FFFC}") && want_text.is_none();
        let text_changed = !keeps_marker && cur_text != want_text;
        if any {
            w.apply(DocOp::SetProps { id, patch })?;
            touched.insert(id);
        }
        if old_local != n.local {
            if let Some(l) = old_local {
                out.join_del.push(l);
            }
            if let Some(l) = n.local {
                out.join_add.push((l, id));
            }
        }
        if text_changed {
            w.apply(DocOp::SetText {
                id,
                text: n.text.clone(),
            })?;
            touched.insert(id);
        }
        Ok(())
    }

    fn apply_text_edit(
        w: &mut DocWriter,
        id: DocNodeId,
        edit: &TextEdit,
        touched: &mut HashSet<DocNodeId>,
    ) -> Result<(), DocError> {
        let new: Arc<str> = match edit {
            TextEdit::Replace(t) => clip_arc(Some(t)).unwrap_or_else(|| Arc::clone(t)),
            TextEdit::Splice { range, text } => {
                let cur: Vec<char> = w
                    .doc()
                    .node(id)
                    .and_then(|v| v.text)
                    .unwrap_or("")
                    .chars()
                    .collect();
                let s = (range.start as usize).min(cur.len());
                let e = (range.end as usize).clamp(s, cur.len());
                let mut out = String::with_capacity(cur.len() + text.len());
                out.extend(&cur[..s]);
                out.push_str(text);
                out.extend(&cur[e..]);
                {
                    let whole: Arc<str> = Arc::from(out);
                    clip_arc(Some(&whole)).unwrap_or(whole)
                }
            }
        };
        w.apply(DocOp::SetText {
            id,
            text: Some(new),
        })?;
        touched.insert(id);
        Ok(())
    }

    /// FT-4: recompute the break flag of every child of `parent`.
    fn recompute_breaks(w: &mut DocWriter, parent: DocNodeId) -> Result<(), DocError> {
        let doc = w.doc();
        let kids: Vec<DocNodeId> = doc.children(parent).collect();
        let mut flags: Vec<NodeFlags> = kids
            .iter()
            .map(|k| doc.node(*k).map_or(NodeFlags::NONE, |v| v.flags))
            .collect();
        let before = flags.clone();
        assign_breaks(&mut flags);
        let patches: Vec<(DocNodeId, NodeFlags)> = kids
            .iter()
            .zip(before.iter().zip(&flags))
            .filter(|(_, (b, a))| b != a)
            .map(|(k, (_, a))| (*k, *a))
            .collect();
        for (id, f) in patches {
            w.apply(DocOp::SetProps {
                id,
                patch: PropsPatch {
                    flags: Some(f),
                    ..PropsPatch::default()
                },
            })?;
        }
        Ok(())
    }

    /// FT-6: give a control with no text a place in its line, take the marker away again when
    /// the control gains text. Looks at the touched nodes and a few ancestors.
    fn markers(w: &mut DocWriter, touched: &HashSet<DocNodeId>) -> Result<(), DocError> {
        let mut cands: HashSet<DocNodeId> = HashSet::new();
        {
            let doc = w.doc();
            for t in touched {
                let mut cur = Some(*t);
                for _ in 0..4 {
                    let Some(id) = cur else { break };
                    if !cands.insert(id) {
                        break;
                    }
                    cur = doc.parent(id);
                }
            }
        }
        // Deepest first, each change applied before the next node is looked at: a button's
        // anchor depends on whether its icon child already carries one.
        let mut order: Vec<(usize, DocNodeId)> = {
            let doc = w.doc();
            cands
                .into_iter()
                .map(|id| {
                    let mut depth = 0usize;
                    let mut cur = doc.parent(id);
                    while let Some(p) = cur {
                        depth += 1;
                        cur = doc.parent(p);
                    }
                    (depth, id)
                })
                .collect()
        };
        order.sort_by_key(|(depth, _)| std::cmp::Reverse(*depth));
        for (_, id) in order {
            let op = {
                let doc = w.doc();
                let Some(v) = doc.node(id) else { continue };
                if !marker_role(v.role) || v.flags.contains(NodeFlags::HIDDEN) {
                    continue;
                }
                let Some((s, e)) = doc.subtree_text_range(id) else {
                    continue;
                };
                let own = doc.own_len(id).unwrap_or(0);
                // MI-1: what the descendants contribute counts only if a user could land on it:
                // a line break or white space (a block icon such as a canvas) is no anchor.
                let mut after_own = String::new();
                doc.read_text(s.saturating_add(own), e, &mut after_own);
                let descendants = after_own.chars().filter(|c| !c.is_whitespace()).count();
                let has_marker = v.text == Some("\u{FFFC}");
                let own_text_empty = v.text.is_none_or(str::is_empty);
                if descendants == 0 && own_text_empty {
                    DocOp::SetText {
                        id,
                        text: Some(Arc::from("\u{FFFC}")),
                    }
                } else if descendants > 0 && has_marker {
                    DocOp::SetText { id, text: None }
                } else {
                    continue;
                }
            };
            w.apply(op)?;
        }
        Ok(())
    }

    /// Free every detached subtree and forget its ids and transport keys.
    fn reap(&mut self, w: &mut DocWriter, out: &mut BatchOutcome) -> Result<(), DocError> {
        let roots: Vec<DocNodeId> = w.doc().detached_roots().collect();
        if roots.is_empty() {
            return Ok(());
        }
        {
            let doc = w.doc();
            for r in &roots {
                for id in doc.subtree(*r) {
                    if let Some(m) = doc.node(id).and_then(|v| v.meta) {
                        if let Some(s) = m.src
                            && self.by_src.get(&s) == Some(&id)
                        {
                            self.by_src.remove(&s);
                        }
                        if let Some(l) = m.local {
                            out.join_del.push(l);
                        }
                    }
                }
            }
        }
        w.apply(DocOp::Reap { id: None })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_doc::web::channel;

    fn n(id: u64, role: Role) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role,
            states: StateSet::VISIBLE | StateSet::SHOWING | StateSet::ENABLED,
            name: None,
            text: None,
            attrs: smallvec::SmallVec::default(),
            local: Some(LocalId(id + 1000)),
        }
    }

    fn txt(id: u64, text: &str) -> SrcNode {
        let mut x = n(id, Role::StaticText);
        x.text = Some(Arc::from(text));
        x.local = None;
        x
    }

    fn with_display(mut x: SrcNode, d: &str) -> SrcNode {
        x.attrs.push((web_attrs::DISPLAY, Arc::from(d)));
        x
    }

    fn list(parent: u64, kids: &[u64]) -> DocDelta {
        DocDelta::Children {
            parent: SrcNodeId(parent),
            children: kids.iter().map(|k| SrcNodeId(*k)).collect(),
        }
    }

    /// body(1) -> [h1(2) -> "Title"(3), p(4) -> ["Hello "(5), a(6) -> "link"(7), " tail"(8)]]
    fn page() -> Vec<DocDelta> {
        let mut h1 = with_display(n(2, Role::Heading), "block");
        h1.attrs.push((web_attrs::LEVEL, Arc::from("1")));
        vec![
            DocDelta::Reset {
                root: SrcNodeId(1),
                nav_id: 1,
            },
            DocDelta::Upsert(vec![
                with_display(n(1, Role::DocumentWeb), "block"),
                h1,
                txt(3, "Title"),
                with_display(n(4, Role::Paragraph), "block"),
                txt(5, "Hello "),
                with_display(n(6, Role::Link), "inline"),
                txt(7, "link"),
                txt(8, " tail"),
            ]),
            list(1, &[2, 4]),
            list(2, &[3]),
            list(4, &[5, 6, 8]),
            list(6, &[7]),
            DocDelta::Settled { batch: 1 },
        ]
    }

    #[test]
    fn first_batch_builds_the_tree_with_block_breaks_and_joins() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        let o = t.apply(&mut w, page()).unwrap();
        assert_eq!(o.created, 8);
        assert_eq!(o.join_add.len(), 4, "only real nodes have a transport key");
        assert!(w.publish());
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(d.len(), 8);
            // h1 and p are blocks: a break before each; the link and the tail are inline.
            assert_eq!(d.text_in(0, d.text_len()), "\nTitle\nHello link tail");
        });
        // Every real node is reachable through its src id.
        assert!(t.id_of(SrcNodeId(6)).is_some());
        assert_eq!(t.known(), 8);
        // Joined keys name the right nodes.
        let (_, link_id) = o
            .join_add
            .iter()
            .find(|(l, _)| *l == LocalId(1006))
            .copied()
            .unwrap();
        r.with(|d| assert_eq!(d.node(link_id).map(|v| v.role), Some(Role::Link)));
    }

    #[test]
    fn a_text_edit_and_an_inserted_sibling_are_applied_in_place() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, page()).unwrap();
        w.publish();
        let tail_before = t.id_of(SrcNodeId(8)).unwrap();
        // " tail" becomes " end"; a new block paragraph follows.
        let o = t
            .apply(
                &mut w,
                vec![
                    DocDelta::Upsert(vec![
                        txt(8, " end"),
                        with_display(n(9, Role::Paragraph), "block"),
                        txt(10, "More"),
                    ]),
                    list(1, &[2, 4, 9]),
                    list(9, &[10]),
                    DocDelta::Settled { batch: 2 },
                ],
            )
            .unwrap();
        assert_eq!(o.created, 2);
        w.publish();
        assert_eq!(t.id_of(SrcNodeId(8)), Some(tail_before), "id kept");
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(d.text_in(0, d.text_len()), "\nTitle\nHello link end\nMore");
        });
    }

    #[test]
    fn removal_reaps_the_subtree_and_forgets_its_keys() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, page()).unwrap();
        w.publish();
        let o = t
            .apply(
                &mut w,
                vec![
                    list(4, &[5, 8]),
                    DocDelta::Removed(vec![SrcNodeId(6), SrcNodeId(7)]),
                    DocDelta::Settled { batch: 2 },
                ],
            )
            .unwrap();
        w.publish();
        assert_eq!(o.join_del, vec![LocalId(1006)], "only the link had a key");
        assert!(t.id_of(SrcNodeId(6)).is_none());
        assert!(t.id_of(SrcNodeId(7)).is_none());
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(d.len(), 6);
            assert_eq!(d.detached_len(), 0);
            assert_eq!(d.text_in(0, d.text_len()), "\nTitle\nHello  tail");
        });
    }

    #[test]
    fn a_listed_child_without_an_upsert_is_a_placeholder_filled_later() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        // Progressive build: the first block names node 3 but does not carry it.
        t.apply(
            &mut w,
            vec![
                DocDelta::Reset {
                    root: SrcNodeId(1),
                    nav_id: 1,
                },
                DocDelta::Upsert(vec![n(1, Role::DocumentWeb), n(2, Role::Paragraph)]),
                list(1, &[2, 3]),
                DocDelta::Settled { batch: 1 },
            ],
        )
        .unwrap();
        w.publish();
        let ph = t.id_of(SrcNodeId(3)).unwrap();
        r.with(|d| assert_eq!(d.node(ph).map(|v| v.role), Some(Role::Unknown)));
        // The later block fills it in place and gives it a child.
        t.apply(
            &mut w,
            vec![
                DocDelta::Upsert(vec![n(3, Role::Heading), txt(4, "late")]),
                list(3, &[4]),
                DocDelta::Settled { batch: 2 },
            ],
        )
        .unwrap();
        w.publish();
        assert_eq!(t.id_of(SrcNodeId(3)), Some(ph));
        r.with(|d| {
            assert_eq!(d.node(ph).map(|v| v.role), Some(Role::Heading));
            assert!(d.text_in(0, d.text_len()).ends_with("late"));
            assert!(d.validate().is_ok());
        });
    }

    #[test]
    fn a_reset_replaces_the_tree_in_the_same_arena() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, page()).unwrap();
        w.publish();
        let root = r.with(WebDoc::root).unwrap();
        let old_h = t.id_of(SrcNodeId(2)).unwrap();
        let o = t
            .apply(
                &mut w,
                vec![
                    DocDelta::Reset {
                        root: SrcNodeId(100),
                        nav_id: 2,
                    },
                    DocDelta::Upsert(vec![
                        with_display(n(100, Role::DocumentWeb), "block"),
                        with_display(n(101, Role::Paragraph), "block"),
                        txt(102, "new page"),
                    ]),
                    list(100, &[101]),
                    list(101, &[102]),
                    DocDelta::Settled { batch: 1 },
                ],
            )
            .unwrap();
        assert!(o.reset);
        w.publish();
        r.with(|d| {
            assert_eq!(d.root(), Some(root), "the root node survives");
            assert!(!d.contains(old_h), "old ids are Gone");
            assert_eq!(d.text_in(0, d.text_len()), "\nnew page");
            assert!(d.validate().is_ok());
        });
        assert!(t.id_of(SrcNodeId(2)).is_none());
        assert_eq!(t.id_of(SrcNodeId(100)), Some(root));
        assert!(
            o.join_del.contains(&LocalId(1006)),
            "old keys leave the join"
        );
    }

    /// MI-1: a button whose only descendant is a block with no text (a canvas icon) contributes
    /// nothing but a line break, so it needs its own anchor or arrows skip it.
    #[test]
    fn a_button_holding_only_a_textless_block_keeps_an_anchor() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(
            &mut w,
            vec![
                DocDelta::Reset {
                    root: SrcNodeId(1),
                    nav_id: 1,
                },
                DocDelta::Upsert(vec![
                    n(1, Role::DocumentWeb),
                    with_display(n(2, Role::Button), "inline"),
                    with_display(n(3, Role::Unknown), "block"),
                ]),
                list(1, &[2]),
                list(2, &[3]),
                DocDelta::Settled { batch: 1 },
            ],
        )
        .unwrap();
        w.publish();
        r.with(|d| {
            let text = d.text_in(0, d.text_len());
            assert!(text.contains("\u{FFFC}"), "{text:?}");
        });
    }

    #[test]
    fn controls_without_text_get_a_marker_and_lose_it_with_a_label() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(
            &mut w,
            vec![
                DocDelta::Reset {
                    root: SrcNodeId(1),
                    nav_id: 1,
                },
                DocDelta::Upsert(vec![
                    n(1, Role::DocumentWeb),
                    with_display(n(2, Role::CheckBox), "inline"),
                ]),
                list(1, &[2]),
                DocDelta::Settled { batch: 1 },
            ],
        )
        .unwrap();
        w.publish();
        r.with(|d| assert_eq!(d.text_in(0, d.text_len()), "\u{FFFC}"));
        // The button gains a text child: the marker goes away.
        t.apply(
            &mut w,
            vec![
                DocDelta::Upsert(vec![txt(3, "OK")]),
                list(2, &[3]),
                DocDelta::Settled { batch: 2 },
            ],
        )
        .unwrap();
        w.publish();
        r.with(|d| assert_eq!(d.text_in(0, d.text_len()), "OK"));
        // An unrelated upsert of the checkbox (no text) must not bring the marker back.
        t.apply(
            &mut w,
            vec![
                DocDelta::Upsert(vec![n(2, Role::CheckBox)]),
                DocDelta::Settled { batch: 3 },
            ],
        )
        .unwrap();
        w.publish();
        r.with(|d| assert_eq!(d.text_in(0, d.text_len()), "OK"));
    }

    #[test]
    fn a_batch_the_mirror_rejects_resyncs_the_maps() {
        let (mut w, _r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, page()).unwrap();
        // A list that names a node twice is rejected before it changes anything.
        let e = t.apply(
            &mut w,
            vec![list(4, &[5, 5]), DocDelta::Settled { batch: 2 }],
        );
        assert!(e.is_err());
        assert_eq!(t.stats().rejected, 1);
        assert_eq!(t.known(), 8, "maps rebuilt from the mirror");
        assert!(w.doc().validate().is_ok());
    }

    #[test]
    fn nodes_moved_between_parents_keep_their_ids() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, page()).unwrap();
        w.publish();
        let link = t.id_of(SrcNodeId(6)).unwrap();
        // The link moves from the paragraph to the heading.
        t.apply(
            &mut w,
            vec![
                list(4, &[5, 8]),
                list(2, &[3, 6]),
                DocDelta::Settled { batch: 2 },
            ],
        )
        .unwrap();
        w.publish();
        assert_eq!(t.id_of(SrcNodeId(6)), Some(link));
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(d.parent(link), t.id_of(SrcNodeId(2)));
            assert_eq!(d.text_in(0, d.text_len()), "\nTitlelink\nHello  tail");
        });
    }

    #[test]
    fn break_assignment_follows_block_neighbours() {
        let mut f = vec![
            NodeFlags::NONE,
            NodeFlags::BLOCK,
            NodeFlags::NONE,
            NodeFlags::NONE,
            NodeFlags::BLOCK | NodeFlags::HIDDEN,
            NodeFlags::NONE,
        ];
        assign_breaks(&mut f);
        let bb: Vec<bool> = f
            .iter()
            .map(|x| x.contains(NodeFlags::BREAK_BEFORE))
            .collect();
        assert_eq!(bb, vec![false, true, true, false, false, false]);
    }

    fn chain(n: u64) -> Vec<DocDelta> {
        let mut nodes = vec![n_doc()];
        let mut lists = Vec::new();
        for i in 2..=n {
            nodes.push(n_with(i, Role::Panel));
            lists.push(list(i - 1, &[i]));
        }
        let mut d = vec![
            DocDelta::Reset {
                root: SrcNodeId(1),
                nav_id: 1,
            },
            DocDelta::Upsert(nodes),
        ];
        d.extend(lists);
        d.push(DocDelta::Settled { batch: 1 });
        d
    }

    fn n_doc() -> SrcNode {
        n_with(1, Role::DocumentWeb)
    }

    fn n_with(id: u64, role: Role) -> SrcNode {
        n(id, role)
    }

    #[test]
    fn a_hundred_thousand_deep_chain_is_cut_at_the_depth_cap_without_touching_the_stack() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        // Built on the default test-thread stack: any recursion over the page would overflow.
        t.apply(&mut w, chain(100_000)).unwrap();
        w.publish();
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(
                d.len(),
                MAX_DEPTH as usize + 1,
                "root plus MAX_DEPTH levels"
            );
        });
        assert_eq!(t.stats().truncated, 1);
        // Dropping the writer and reader (deep specs, deep tree) is safe too.
        drop(w);
        drop(r);
    }

    #[test]
    fn a_chain_grown_one_level_per_batch_stops_at_the_cap() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, chain(2)).unwrap();
        for i in 3..(u64::from(MAX_DEPTH) + 40) {
            t.apply(
                &mut w,
                vec![
                    DocDelta::Upsert(vec![n(i, Role::Panel)]),
                    list(i - 1, &[i]),
                    DocDelta::Settled { batch: i },
                ],
            )
            .unwrap();
        }
        w.publish();
        r.with(|d| {
            assert!(d.validate().is_ok());
            assert_eq!(d.len(), MAX_DEPTH as usize + 1);
        });
        assert!(t.stats().truncated > 0);
    }

    #[test]
    fn parent_child_cycles_in_the_source_terminate() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        // 1 -> 2 -> 3 -> 2 (a cycle) and 4 listing itself.
        let batch = vec![
            DocDelta::Reset {
                root: SrcNodeId(1),
                nav_id: 1,
            },
            DocDelta::Upsert(vec![
                n(1, Role::DocumentWeb),
                n(2, Role::Panel),
                n(3, Role::Panel),
                n(4, Role::Panel),
            ]),
            list(1, &[2, 4]),
            list(2, &[3]),
            list(3, &[2]),
            list(4, &[4]),
            DocDelta::Settled { batch: 1 },
        ];
        // Either applied (each list is consumed once) or rejected whole; never a hang or a panic.
        let _ = t.apply(&mut w, batch);
        w.publish();
        r.with(|d| assert!(d.validate().is_ok()));
        // A later batch that closes a cycle through the mirror is rejected by the ops.
        let again = t.apply(&mut w, vec![list(3, &[1]), DocDelta::Settled { batch: 2 }]);
        w.publish();
        r.with(|d| assert!(d.validate().is_ok()));
        let _ = again;
    }

    // ---- M1/M2: the caps hold for moves, fan-out, pending lists and text ----------------

    fn max_depth(d: &WebDoc) -> u32 {
        d.preorder()
            .map(|id| {
                let mut k = 0;
                let mut c = d.parent(id);
                while let Some(p) = c {
                    k += 1;
                    c = d.parent(p);
                }
                k
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn moving_a_subtree_under_a_deep_node_cannot_pass_the_depth_cap() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        let len = u64::from(MAX_DEPTH) - 1;
        let mut nodes = vec![n(1, Role::DocumentWeb)];
        let mut lists = Vec::new();
        for base in [10u64, 100_000] {
            for i in 0..len {
                nodes.push(n(base + i, Role::Panel));
                if i > 0 {
                    lists.push(list(base + i - 1, &[base + i]));
                }
            }
        }
        let mut b = vec![
            DocDelta::Reset {
                root: SrcNodeId(1),
                nav_id: 1,
            },
            DocDelta::Upsert(nodes),
            list(1, &[10, 100_000]),
        ];
        b.extend(lists);
        b.push(DocDelta::Settled { batch: 1 });
        t.apply(&mut w, b).unwrap();
        w.publish();
        let cut_before = t.stats().truncated;
        t.apply(
            &mut w,
            vec![
                list(1, &[10]),
                list(10 + len - 1, &[100_000]),
                DocDelta::Settled { batch: 2 },
            ],
        )
        .unwrap();
        w.publish();
        assert!(
            r.with(max_depth) <= MAX_DEPTH,
            "depth {}",
            r.with(max_depth)
        );
        assert!(t.stats().truncated > cut_before, "the cut is counted");
    }

    #[test]
    fn a_fan_out_beyond_the_child_cap_is_cut_and_counted() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        let k = (MAX_CHILDREN + 25) as u64;
        let mut nodes = vec![n(1, Role::DocumentWeb)];
        nodes.extend((2..2 + k).map(|i| n(i, Role::Panel)));
        let kids: Vec<u64> = (2..2 + k).collect();
        t.apply(
            &mut w,
            vec![
                DocDelta::Reset {
                    root: SrcNodeId(1),
                    nav_id: 1,
                },
                DocDelta::Upsert(nodes),
                list(1, &kids),
                DocDelta::Settled { batch: 1 },
            ],
        )
        .unwrap();
        w.publish();
        assert_eq!(r.with(WebDoc::len), MAX_CHILDREN + 1);
        assert_eq!(t.stats().truncated, 25);
    }

    #[test]
    fn parked_child_lists_are_bounded_by_total_ids() {
        let (mut w, _r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, chain(2)).unwrap();
        let huge: Vec<u64> = (1_000_000..1_000_000 + 1_000_000).collect();
        for b in 0..6 {
            t.apply(
                &mut w,
                vec![
                    list(9_000_000 + b, &huge),
                    DocDelta::Settled { batch: 2 + b },
                ],
            )
            .unwrap();
        }
        let ids: usize = t.deferred.values().map(Vec::len).sum();
        assert!(ids + t.orphans.len() <= MAX_PENDING, "{ids} ids parked");
        assert!(t.stats().orphans_dropped > 0);
    }

    #[test]
    fn node_text_is_clipped_to_the_byte_cap() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        let mut x = n(2, Role::StaticText);
        x.text = Some(Arc::from("é".repeat(MAX_NODE_TEXT_BYTES)));
        t.apply(
            &mut w,
            vec![
                DocDelta::Reset {
                    root: SrcNodeId(1),
                    nav_id: 1,
                },
                DocDelta::Upsert(vec![n(1, Role::DocumentWeb), x]),
                list(1, &[2]),
                DocDelta::Settled { batch: 1 },
            ],
        )
        .unwrap();
        w.publish();
        let len = r.with(|d| d.text_len() as usize);
        assert!(len <= MAX_NODE_TEXT_BYTES, "{len} scalars");
        assert!(t.stats().text_clipped >= 1);
    }

    // ---- re-review N4: the caps hold inside one batch --------------------------------------

    #[test]
    fn one_list_of_new_subtrees_cannot_pass_the_node_cap() {
        let (mut w, r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, chain(2)).unwrap();
        w.publish();
        let kids_n = 60_000u64;
        let per = 9u64;
        let mut nodes = Vec::new();
        let mut b = Vec::new();
        let mut kids = Vec::new();
        for i in 0..kids_n {
            let c = 10_000_000 + i * 100;
            kids.push(c);
            nodes.push(n(c, Role::Panel));
            let g: Vec<u64> = (1..=per).map(|j| c + j).collect();
            for &x in &g {
                nodes.push(n(x, Role::Panel));
            }
            b.push(list(c, &g));
        }
        let mut batch = vec![DocDelta::Upsert(nodes), list(2, &kids)];
        batch.extend(b);
        batch.push(DocDelta::Settled { batch: 2 });
        t.apply(&mut w, batch).unwrap();
        w.publish();
        let len = r.with(WebDoc::len);
        assert!(len <= MAX_DOC_NODES, "cap passed: {len}");
        assert!(t.stats().truncated > 0);
    }

    #[test]
    fn parked_lists_and_orphans_share_one_pending_budget() {
        let (mut w, _r) = channel(1);
        let mut t = Translator::new();
        t.apply(&mut w, chain(2)).unwrap();
        let orphans: Vec<SrcNode> = (0..MAX_PENDING as u64)
            .map(|i| n(50_000_000 + i, Role::Panel))
            .collect();
        let l1: Vec<u64> = (0..MAX_CHILDREN as u64).map(|i| 1_000_000 + i).collect();
        let l2: Vec<u64> = (0..MAX_CHILDREN as u64).map(|i| 2_000_000 + i).collect();
        for b in 0..3u64 {
            let mut batch = vec![list(9_000_000, &l1), list(9_000_001, &l2)];
            if b == 0 {
                batch.insert(0, DocDelta::Upsert(orphans.clone()));
            }
            batch.push(DocDelta::Settled { batch: 2 + b });
            t.apply(&mut w, batch).unwrap();
        }
        let ids: usize = t.deferred.values().map(Vec::len).sum();
        assert!(
            ids + t.orphans.len() <= MAX_PENDING,
            "{ids} + {}",
            t.orphans.len()
        );
    }
}
