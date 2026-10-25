// SPDX-License-Identifier: GPL-3.0-or-later
//! Property tests: random op sequences against a naive tree model, checking structure,
//! navigation, summaries, flat text and publication after every step. proptest is not
//! vendored, so these are seeded loops (failures print the seed and step).

#![allow(
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::ignored_unit_patterns,
    clippy::type_complexity
)]

use std::collections::BTreeSet;
use std::sync::Arc;

use gorca_doc::web::synth::Rng;
use gorca_doc::web::{
    Cat, ChildSpec, DocError, DocNodeId, DocOp, DocRead, DocWriter, NodeFlags, NodeSpec,
    PropsPatch, RANKED, ReadUnit, SubtreeSpec, WebDoc, align_by_key, cats_of, channel,
    word_range_in,
};
use gorca_model::Role;
use gorca_model::sem::SemRole;

#[derive(Clone)]
struct MNode {
    id: DocNodeId,
    parent: Option<usize>,
    children: Vec<usize>,
    role: SemRole,
    level: u8,
    flags: NodeFlags,
    sig: u64,
    text: Option<String>,
    alive: bool,
}

impl MNode {
    fn cats(&self) -> Cat {
        cats_of(self.role, self.level, self.flags)
    }

    fn contribution(&self) -> Vec<char> {
        if self.flags.contains(NodeFlags::HIDDEN) {
            return Vec::new();
        }
        let mut v: Vec<char> = Vec::new();
        if self.flags.contains(NodeFlags::BREAK_BEFORE) {
            v.push('\n');
        }
        v.extend(self.text.as_deref().unwrap_or("").chars());
        if self.flags.contains(NodeFlags::BREAK_AFTER) {
            v.push('\n');
        }
        v
    }
}

#[derive(Default)]
struct Model {
    nodes: Vec<MNode>,
    root: Option<usize>,
    detached: BTreeSet<usize>,
}

const ROLES: [SemRole; 14] = [
    SemRole::Heading,
    SemRole::Link,
    SemRole::Paragraph,
    SemRole::StaticText,
    SemRole::List,
    SemRole::ListItem,
    SemRole::Table,
    SemRole::Button,
    SemRole::TextBox,
    SemRole::Region,
    SemRole::Img,
    SemRole::Generic,
    SemRole::Checkbox,
    SemRole::Alert,
];

const TEXTS: [&str; 7] = ["", "a", "hello world", "x\ny", "\n", "one, two", "  sp  "];

fn rand_node(rng: &mut Rng) -> NodeSpec {
    let role = ROLES[rng.below(ROLES.len())];
    let mut flags = NodeFlags::NONE;
    for f in [
        NodeFlags::HIDDEN,
        NodeFlags::VISITED,
        NodeFlags::REGION,
        NodeFlags::ARTICLE,
        NodeFlags::LIVE,
        NodeFlags::BREAK_AFTER,
        NodeFlags::BREAK_BEFORE,
    ] {
        if rng.below(6) == 0 {
            flags = flags | f;
        }
    }
    let mut n = NodeSpec::new(role)
        .level(1 + rng.below(6) as u8)
        .flags(flags)
        .sig(rng.below(5) as u64);
    if rng.below(2) == 0 {
        n = n.text(TEXTS[rng.below(TEXTS.len())]);
    }
    n
}

fn rand_spec(rng: &mut Rng, depth: u32) -> SubtreeSpec {
    let kids = if depth == 0 { 0 } else { rng.below(4) };
    SubtreeSpec::with_children(
        rand_node(rng),
        (0..kids).map(|_| rand_spec(rng, depth - 1)).collect(),
    )
}

impl Model {
    fn add_spec(&mut self, spec: &SubtreeSpec, parent: Option<usize>, out: &mut Vec<usize>) {
        let idx = self.nodes.len();
        self.nodes.push(MNode {
            id: DocNodeId::from_u64(0),
            parent,
            children: Vec::new(),
            role: spec.node.role,
            level: spec.node.level,
            flags: spec.node.flags,
            sig: spec.node.sig,
            text: spec.node.text.as_deref().map(str::to_owned),
            alive: true,
        });
        out.push(idx);
        if let Some(p) = parent {
            self.nodes[p].children.push(idx);
        }
        for c in &spec.children {
            self.add_spec(c, Some(idx), out);
        }
    }

    fn insert_at(
        &mut self,
        parent: usize,
        before: Option<usize>,
        spec: &SubtreeSpec,
    ) -> Vec<usize> {
        let mut out = Vec::new();
        self.add_spec(spec, Some(parent), &mut out);
        // add_spec appended at the end; move to the requested slot.
        let top = out[0];
        self.nodes[parent].children.pop();
        let pos = before
            .and_then(|b| self.nodes[parent].children.iter().position(|c| *c == b))
            .unwrap_or(self.nodes[parent].children.len());
        self.nodes[parent].children.insert(pos, top);
        out
    }

    fn preorder_from(&self, from: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut stack = vec![from];
        while let Some(x) = stack.pop() {
            out.push(x);
            for c in self.nodes[x].children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    fn attached(&self) -> Vec<usize> {
        self.root.map_or_else(Vec::new, |r| self.preorder_from(r))
    }

    fn is_ancestor_or_self(&self, a: usize, mut d: usize) -> bool {
        loop {
            if a == d {
                return true;
            }
            match self.nodes[d].parent {
                Some(p) => d = p,
                None => return false,
            }
        }
    }

    fn detach(&mut self, x: usize) {
        if let Some(p) = self.nodes[x].parent {
            self.nodes[p].children.retain(|c| *c != x);
        }
        self.nodes[x].parent = None;
    }

    fn flat(&self) -> (Vec<char>, Vec<(usize, usize)>) {
        let mut text = Vec::new();
        let mut spans = Vec::new();
        for x in self.attached() {
            let c = self.nodes[x].contribution();
            spans.push((text.len(), c.len()));
            text.extend(c);
        }
        (text, spans)
    }
}

const PROBES: [Cat; 12] = [
    Cat::HEADING,
    Cat::HEADING_2,
    Cat::LINK,
    Cat::FORM_FIELD,
    Cat::BUTTON,
    Cat::ANY_LANDMARK,
    Cat::TABLE,
    Cat::LIST_ITEM,
    Cat::TEXT_RUN,
    Cat::LIVE_REGION,
    Cat::ARTICLE,
    Cat(Cat::HEADING_1.0 | Cat::HEADING_3.0),
];

struct Harness {
    model: Model,
    w: DocWriter,
    r: DocRead,
    rng: Rng,
    seed: u64,
    step: usize,
    counts: std::collections::BTreeMap<&'static str, usize>,
    max_nodes: usize,
}

fn op_name(op: &DocOp) -> &'static str {
    match op {
        DocOp::InsertSubtree { .. } => "insert",
        DocOp::RemoveSubtree { .. } => "remove",
        DocOp::AttachSubtree { .. } => "attach",
        DocOp::MoveSubtree { .. } => "move",
        DocOp::ReorderChildren { .. } => "reorder",
        DocOp::SyncChildren { .. } => "sync",
        DocOp::SetText { .. } => "set_text",
        DocOp::SetProps { .. } => "set_props",
        DocOp::Reap { .. } => "reap",
    }
}

impl Harness {
    fn ctx(&self) -> String {
        format!("seed {} step {}", self.seed, self.step)
    }

    fn id(&self, i: usize) -> DocNodeId {
        self.model.nodes[i].id
    }

    fn assign(&mut self, idxs: &[usize], ids: &[DocNodeId]) {
        assert_eq!(idxs.len(), ids.len(), "{}: created count", self.ctx());
        for (i, id) in idxs.iter().zip(ids) {
            self.model.nodes[*i].id = *id;
        }
    }

    fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        if v.is_empty() {
            None
        } else {
            Some(v[self.rng.below(v.len())])
        }
    }

    fn step_op(&mut self) {
        let attached = self.model.attached();
        let non_root: Vec<usize> = attached
            .iter()
            .copied()
            .filter(|x| Some(*x) != self.model.root)
            .collect();
        let detached: Vec<usize> = self.model.detached.iter().copied().collect();
        let choice = self.rng.below(100);
        match choice {
            0..=29 => self.op_insert(&attached),
            30..=39 => {
                if let Some(x) = self.pick(&non_root) {
                    self.exec(&DocOp::RemoveSubtree { id: self.id(x) }, |m, _| {
                        m.detach(x);
                        m.detached.insert(x);
                        Vec::new()
                    });
                }
            }
            40..=47 => {
                if let (Some(x), Some(p)) = (self.pick(&detached), self.pick(&attached)) {
                    let before = self.rand_before(p);
                    let op = DocOp::AttachSubtree {
                        id: self.id(x),
                        parent: self.id(p),
                        before: before.map(|b| self.id(b)),
                    };
                    self.exec(&op, |m, _| {
                        m.detached.remove(&x);
                        m.nodes[x].parent = Some(p);
                        let pos = before
                            .and_then(|b| m.nodes[p].children.iter().position(|c| *c == b))
                            .unwrap_or(m.nodes[p].children.len());
                        m.nodes[p].children.insert(pos, x);
                        Vec::new()
                    });
                }
            }
            48..=59 => self.op_move(&non_root, &attached),
            60..=66 => self.op_reorder(&attached),
            67..=79 => self.op_sync(&attached),
            80..=87 => {
                let all: Vec<usize> = (0..self.model.nodes.len())
                    .filter(|i| self.model.nodes[*i].alive)
                    .collect();
                if let Some(x) = self.pick(&all) {
                    let t = if self.rng.below(4) == 0 {
                        None
                    } else {
                        Some(TEXTS[self.rng.below(TEXTS.len())])
                    };
                    let op = DocOp::SetText {
                        id: self.id(x),
                        text: t.map(Arc::from),
                    };
                    self.exec(&op, |m, _| {
                        m.nodes[x].text = t.map(str::to_owned);
                        Vec::new()
                    });
                }
            }
            88..=95 => {
                let all: Vec<usize> = (0..self.model.nodes.len())
                    .filter(|i| self.model.nodes[*i].alive)
                    .collect();
                if let Some(x) = self.pick(&all) {
                    let spec = rand_node(&mut self.rng);
                    let patch = PropsPatch {
                        role: Some(spec.role),
                        level: Some(spec.level),
                        flags: Some(spec.flags),
                        sig: Some(spec.sig),
                        name: None,
                        data: None,
                    };
                    self.exec(
                        &DocOp::SetProps {
                            id: self.id(x),
                            patch,
                        },
                        |m, _| {
                            let n = &mut m.nodes[x];
                            n.role = spec.role;
                            n.level = spec.level;
                            n.flags = spec.flags;
                            n.sig = spec.sig;
                            Vec::new()
                        },
                    );
                }
            }
            _ => self.op_reap(&detached),
        }
    }

    fn rand_before(&mut self, p: usize) -> Option<usize> {
        let kids = self.model.nodes[p].children.clone();
        if kids.is_empty() || self.rng.below(3) == 0 {
            None
        } else {
            self.pick(&kids)
        }
    }

    /// Apply `op` to the writer, then the model closure; the closure returns created model
    /// indices (in expected order). Checks everything afterwards.
    fn exec(&mut self, op: &DocOp, f: impl FnOnce(&mut Model, ()) -> Vec<usize>) {
        let out = self.w.apply(op.clone());
        let out = match out {
            Ok(o) => o,
            Err(e) => panic!("{}: op {:?} rejected: {e}", self.ctx(), op),
        };
        *self.counts.entry(op_name(op)).or_default() += 1;
        let created = f(&mut self.model, ());
        self.max_nodes = self.max_nodes.max(self.model.attached().len());
        self.assign(&created, &out.created);
        self.check();
    }

    fn op_insert(&mut self, attached: &[usize]) {
        let spec = Arc::new(rand_spec(&mut self.rng, 2));
        if attached.is_empty() {
            let op = DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::clone(&spec),
            };
            self.exec(&op, |m, ()| {
                let mut out = Vec::new();
                // The root must be a plain node: a Document.
                m.add_spec(&spec, None, &mut out);
                m.root = Some(out[0]);
                out
            });
            return;
        }
        let Some(p) = self.pick(attached) else { return };
        let before = self.rand_before(p);
        let op = DocOp::InsertSubtree {
            parent: Some(self.id(p)),
            before: before.map(|b| self.id(b)),
            subtree: Arc::clone(&spec),
        };
        self.exec(&op, |m, ()| m.insert_at(p, before, &spec));
    }

    fn op_move(&mut self, non_root: &[usize], attached: &[usize]) {
        let (Some(x), Some(p)) = (self.pick(non_root), self.pick(attached)) else {
            return;
        };
        let before = self.rand_before(p);
        let op = DocOp::MoveSubtree {
            id: self.id(x),
            parent: self.id(p),
            before: before.map(|b| self.id(b)),
        };
        if self.model.is_ancestor_or_self(x, p) {
            let preorder: Vec<DocNodeId> = self.w.doc().preorder().collect();
            let r = self.w.apply(op);
            assert_eq!(r, Err(DocError::Cycle(self.id(x))), "{}", self.ctx());
            assert_eq!(preorder, self.w.doc().preorder().collect::<Vec<_>>());
            return;
        }
        self.exec(&op, |m, ()| {
            if before == Some(x) {
                return Vec::new();
            }
            m.detach(x);
            m.nodes[x].parent = Some(p);
            let pos = before
                .and_then(|b| m.nodes[p].children.iter().position(|c| *c == b))
                .unwrap_or(m.nodes[p].children.len());
            m.nodes[p].children.insert(pos, x);
            Vec::new()
        });
    }

    fn shuffle(&mut self, v: &mut [usize]) {
        let swaps = if self.rng.below(3) == 0 {
            v.len()
        } else {
            1 + self.rng.below(3)
        };
        for _ in 0..swaps {
            if v.len() > 1 {
                let (a, b) = (self.rng.below(v.len()), self.rng.below(v.len()));
                v.swap(a, b);
            }
        }
    }

    fn op_reorder(&mut self, attached: &[usize]) {
        let Some(p) = self.pick(attached) else { return };
        let mut order = self.model.nodes[p].children.clone();
        self.shuffle(&mut order);
        let op = DocOp::ReorderChildren {
            parent: self.id(p),
            order: order.iter().map(|i| self.id(*i)).collect(),
        };
        self.exec(&op, |m, ()| {
            m.nodes[p].children = order;
            Vec::new()
        });
    }

    fn op_sync(&mut self, attached: &[usize]) {
        let Some(p) = self.pick(attached) else { return };
        let cur = self.model.nodes[p].children.clone();
        let mut keep: Vec<usize> = cur
            .iter()
            .copied()
            .filter(|_| self.rng.below(10) < 7)
            .collect();
        // Hoist grandchildren (and sometimes great-grandchildren) out of their parents.
        for c in &cur {
            for g in self.model.nodes[*c].children.clone() {
                if self.rng.below(5) == 0 && !keep.contains(&g) {
                    keep.push(g);
                    for gg in self.model.nodes[g].children.clone() {
                        if self.rng.below(3) == 0 && !keep.contains(&gg) {
                            keep.push(gg);
                        }
                    }
                }
            }
        }
        // Defunct subtrees brought back.
        for d in self.model.detached.clone() {
            if self.rng.below(4) == 0 {
                keep.push(d);
            }
        }
        // Existing nodes that stayed in `cur` keep their order mostly.
        let mut items: Vec<Result<usize, Arc<SubtreeSpec>>> = keep.iter().map(|i| Ok(*i)).collect();
        for _ in 0..self.rng.below(3) {
            items.push(Err(Arc::new(rand_spec(&mut self.rng, 1))));
        }
        // Order: sort existing current children by their old index, then perturb.
        items.sort_by_key(|it| match it {
            Ok(i) => cur.iter().position(|c| c == i).unwrap_or(usize::MAX),
            Err(_) => usize::MAX,
        });
        let swaps = if self.rng.below(3) == 0 {
            items.len()
        } else {
            self.rng.below(3)
        };
        for _ in 0..swaps {
            if items.len() > 1 {
                let (a, b) = (self.rng.below(items.len()), self.rng.below(items.len()));
                items.swap(a, b);
            }
        }
        let desired: Vec<ChildSpec> = items
            .iter()
            .map(|it| match it {
                Ok(i) => ChildSpec::Existing(self.id(*i)),
                Err(s) => ChildSpec::New(Arc::clone(s)),
            })
            .collect();
        let op = DocOp::SyncChildren {
            parent: self.id(p),
            desired,
        };
        self.exec(&op, |m, ()| {
            let kept: BTreeSet<usize> = items
                .iter()
                .filter_map(|it| it.as_ref().ok().copied())
                .collect();
            for c in &cur {
                if !kept.contains(c) {
                    m.detach(*c);
                    m.detached.insert(*c);
                }
            }
            let mut created = Vec::new();
            let mut children = Vec::new();
            for it in &items {
                match it {
                    Ok(i) => {
                        m.detach(*i);
                        m.detached.remove(i);
                        m.nodes[*i].parent = Some(p);
                        children.push(*i);
                    }
                    Err(spec) => {
                        let mut out = Vec::new();
                        m.add_spec(spec, Some(p), &mut out);
                        // add_spec pushed onto p's children; we rebuild the list below.
                        m.nodes[p].children.pop();
                        children.push(out[0]);
                        created.extend(out);
                    }
                }
            }
            m.nodes[p].children = children;
            created
        });
    }

    fn op_reap(&mut self, detached: &[usize]) {
        if self.rng.below(3) == 0 {
            let dead: Vec<usize> = self
                .model
                .detached
                .iter()
                .flat_map(|d| self.model.preorder_from(*d))
                .collect();
            let ids: Vec<DocNodeId> = dead.iter().map(|i| self.id(*i)).collect();
            self.exec(&DocOp::Reap { id: None }, |m, ()| {
                for i in &dead {
                    m.nodes[*i].alive = false;
                }
                m.detached.clear();
                Vec::new()
            });
            for id in ids {
                assert!(
                    !self.w.doc().contains(id),
                    "{}: reaped id still live",
                    self.ctx()
                );
            }
        } else if let Some(d) = self.pick(detached) {
            let dead = self.model.preorder_from(d);
            let ids: Vec<DocNodeId> = dead.iter().map(|i| self.id(*i)).collect();
            self.exec(
                &DocOp::Reap {
                    id: Some(self.id(d)),
                },
                |m, ()| {
                    for i in &dead {
                        m.nodes[*i].alive = false;
                    }
                    m.detached.remove(&d);
                    Vec::new()
                },
            );
            for id in ids {
                assert!(
                    !self.w.doc().contains(id),
                    "{}: reaped id still live",
                    self.ctx()
                );
            }
        }
    }

    // ------------------------------------------------------------------ checks

    fn check(&mut self) {
        let ctx = self.ctx();
        if self.rng.below(4) == 0 {
            self.w.publish();
            self.check_doc(&self.r.snapshot(), &ctx, true);
        }
        let back = self.w.doc().clone();
        self.check_doc(&back, &ctx, false);
        self.step += 1;
    }

    fn check_doc(&mut self, d: &WebDoc, ctx: &str, published: bool) {
        if published {
            // The published copy is only guaranteed to equal the model right after a publish
            // with no ops since; `check` calls it exactly then.
            assert_eq!(self.w.pending_ops(), 0);
        }
        if let Err(e) = d.validate() {
            panic!("{ctx}: invariant broken: {e}");
        }
        let attached = self.model.attached();
        let ids: Vec<DocNodeId> = attached.iter().map(|i| self.model.nodes[*i].id).collect();
        assert_eq!(d.preorder().collect::<Vec<_>>(), ids, "{ctx}: preorder");
        assert_eq!(d.len(), attached.len(), "{ctx}: len");
        for x in self
            .model
            .detached
            .iter()
            .flat_map(|x| self.model.preorder_from(*x))
        {
            assert!(d.contains(self.model.nodes[x].id), "{ctx}: detached live");
            assert!(
                !d.is_attached(self.model.nodes[x].id),
                "{ctx}: detached attached"
            );
        }
        assert_eq!(d.detached_len(), self.model.detached.len(), "{ctx}");
        if attached.is_empty() {
            return;
        }
        // Parent/children structure.
        for &i in &attached {
            let id = self.model.nodes[i].id;
            let kids: Vec<DocNodeId> = self.model.nodes[i]
                .children
                .iter()
                .map(|c| self.model.nodes[*c].id)
                .collect();
            assert_eq!(d.children(id).collect::<Vec<_>>(), kids, "{ctx}: children");
            assert_eq!(
                d.parent(id),
                self.model.nodes[i].parent.map(|p| self.model.nodes[p].id),
                "{ctx}: parent"
            );
            assert_eq!(
                d.node(id).map(|n| n.cats),
                Some(self.model.nodes[i].cats()),
                "{ctx}: cats"
            );
        }
        // Navigation.
        for _ in 0..12 {
            let k = self.rng.below(attached.len());
            let from = attached[k];
            let fid = self.model.nodes[from].id;
            let c = PROBES[self.rng.below(PROBES.len())];
            let exp_next = attached[k + 1..]
                .iter()
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            let exp_prev = attached[..k]
                .iter()
                .rev()
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            assert_eq!(
                d.next(fid, c),
                exp_next.map(|x| self.model.nodes[*x].id),
                "{ctx}: next {c:?}"
            );
            assert_eq!(
                d.prev(fid, c),
                exp_prev.map(|x| self.model.nodes[*x].id),
                "{ctx}: prev {c:?}"
            );
            // Scoped.
            let scope = attached[self.rng.below(attached.len())];
            let sub = self.model.preorder_from(scope);
            let in_scope = |x: &&usize| sub.contains(x);
            let sn = attached[k + 1..]
                .iter()
                .filter(in_scope)
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            let sp = attached[..k]
                .iter()
                .rev()
                .filter(in_scope)
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            let sid = self.model.nodes[scope].id;
            assert_eq!(
                d.next_in(fid, c, sid),
                sn.map(|x| self.model.nodes[*x].id),
                "{ctx}: next_in"
            );
            assert_eq!(
                d.prev_in(fid, c, sid),
                sp.map(|x| self.model.nodes[*x].id),
                "{ctx}: prev_in"
            );
            // Landmark.
            let mut lm = self.model.nodes[from].parent;
            while let Some(l) = lm {
                if self.model.nodes[l].cats().intersects(Cat::ANY_LANDMARK) {
                    break;
                }
                lm = self.model.nodes[l].parent;
            }
            assert_eq!(
                d.landmark_of(fid),
                lm.map(|l| self.model.nodes[l].id),
                "{ctx}: landmark_of"
            );
            let ln = match lm {
                Some(l) => {
                    let sub = self.model.preorder_from(l);
                    attached[k + 1..]
                        .iter()
                        .filter(|x| sub.contains(x))
                        .find(|x| self.model.nodes[**x].cats().intersects(c))
                        .copied()
                }
                None => exp_next.copied(),
            };
            assert_eq!(
                d.next_in_landmark(fid, c),
                ln.map(|x| self.model.nodes[x].id),
                "{ctx}: next_in_landmark"
            );
        }
        // first_of / last_of
        for c in PROBES {
            let f = attached
                .iter()
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            let l = attached
                .iter()
                .rev()
                .find(|x| self.model.nodes[**x].cats().intersects(c));
            assert_eq!(
                d.first_of(c),
                f.map(|x| self.model.nodes[*x].id),
                "{ctx}: first_of"
            );
            assert_eq!(
                d.last_of(c),
                l.map(|x| self.model.nodes[*x].id),
                "{ctx}: last_of"
            );
        }
        // Ranked categories.
        for c in RANKED {
            let matching: Vec<usize> = attached
                .iter()
                .copied()
                .filter(|x| self.model.nodes[*x].cats().intersects(c))
                .collect();
            assert_eq!(
                d.count(c),
                Some(matching.len() as u32),
                "{ctx}: count {c:?}"
            );
            for (k, x) in matching.iter().enumerate() {
                if k < 6 || self.rng.below(8) == 0 {
                    let id = self.model.nodes[*x].id;
                    assert_eq!(d.kth(c, k as u32), Some(id), "{ctx}: kth");
                    assert_eq!(d.rank(id, c), Some(k as u32), "{ctx}: rank");
                }
            }
            assert_eq!(d.kth(c, matching.len() as u32), None);
        }
        // Order and positions.
        for _ in 0..6 {
            let (a, b) = (
                self.rng.below(attached.len()),
                self.rng.below(attached.len()),
            );
            let (ia, ib) = (
                self.model.nodes[attached[a]].id,
                self.model.nodes[attached[b]].id,
            );
            assert_eq!(d.cmp_order(ia, ib), Some(a.cmp(&b)), "{ctx}: cmp_order");
            assert_eq!(d.position(ia), Some(a as u32));
            assert_eq!(d.at_position(a as u32), Some(ia));
        }
        self.check_text(d, ctx);
    }

    fn check_text(&mut self, d: &WebDoc, ctx: &str) {
        let (flat, spans) = self.model.flat();
        let attached = self.model.attached();
        let total = flat.len() as u32;
        assert_eq!(d.text_len(), total, "{ctx}: text_len");
        assert_eq!(
            d.text_in(0, total),
            flat.iter().collect::<String>(),
            "{ctx}: text"
        );
        let starts: Vec<usize> = std::iter::once(0)
            .chain(
                flat.iter()
                    .enumerate()
                    .filter(|(_, c)| **c == '\n')
                    .map(|(i, _)| i + 1),
            )
            .collect();
        assert_eq!(d.line_count() as usize, starts.len(), "{ctx}: line_count");
        for (k, s) in starts.iter().enumerate() {
            assert_eq!(
                d.line_start(k as u32),
                Some(*s as u32),
                "{ctx}: line_start {k}"
            );
        }
        assert_eq!(d.line_start(starts.len() as u32), None);
        for (k, (i, (s, _))) in attached.iter().zip(&spans).enumerate() {
            if k % 3 == 0 || self.rng.below(4) == 0 {
                assert_eq!(
                    d.offset_of(self.model.nodes[*i].id),
                    Some(*s as u32),
                    "{ctx}: offset_of"
                );
            }
        }
        for _ in 0..16 {
            let off = self.rng.below(flat.len() + 2);
            let exp = attached
                .iter()
                .zip(&spans)
                .find(|(_, (s, l))| *l > 0 && off >= *s && off < s + l);
            let got = d.at_offset(off as u32);
            assert_eq!(
                got,
                exp.map(|(i, (s, _))| (self.model.nodes[*i].id, (off - s) as u32)),
                "{ctx}: at_offset {off}"
            );
            let line = flat[..off.min(flat.len())]
                .iter()
                .filter(|c| **c == '\n')
                .count();
            assert_eq!(
                d.line_of(off.min(flat.len()) as u32),
                line as u32,
                "{ctx}: line_of"
            );
            if off <= flat.len() {
                let ls = starts[line];
                let le = flat[ls..]
                    .iter()
                    .position(|c| *c == '\n')
                    .map_or(flat.len(), |p| ls + p);
                assert_eq!(
                    d.line_range(off as u32),
                    (ls as u32, le as u32),
                    "{ctx}: line_range {off}"
                );
                assert_eq!(
                    d.unit_range(ReadUnit::Line, off as u32),
                    Some((ls as u32, le as u32))
                );
            }
            let (a, b) = (
                self.rng.below(flat.len() + 1),
                self.rng.below(flat.len() + 1),
            );
            let (a, b) = (a.min(b), a.max(b));
            assert_eq!(
                d.text_in(a as u32, b as u32),
                flat[a..b].iter().collect::<String>(),
                "{ctx}: text_in"
            );
            if off < flat.len() {
                let line_chars = &flat[starts[line]
                    ..flat[starts[line]..]
                        .iter()
                        .position(|c| *c == '\n')
                        .map_or(flat.len(), |p| starts[line] + p)];
                let ls = starts[line];
                let want = if line_chars.is_empty() {
                    (ls, ls)
                } else {
                    let (x, y) = word_range_in(line_chars, off - ls);
                    (ls + x, ls + y)
                };
                assert_eq!(
                    d.unit_range(ReadUnit::Word, off as u32),
                    Some((want.0 as u32, want.1 as u32)),
                    "{ctx}: word {off}"
                );
                assert_eq!(
                    d.unit_range(ReadUnit::Char, off as u32),
                    Some((off as u32, off as u32 + 1))
                );
            } else {
                assert_eq!(d.unit_range(ReadUnit::Char, off as u32), None);
                assert_eq!(d.unit_range(ReadUnit::Word, off as u32), None);
            }
        }
        // Walk lines and chars forward and back.
        let mut lines = Vec::new();
        let mut cur = d.unit_range(ReadUnit::Line, 0);
        while let Some((s, e)) = cur {
            lines.push((s, e));
            cur = d.next_unit(ReadUnit::Line, s);
        }
        assert_eq!(lines.len(), starts.len(), "{ctx}: line walk");
        let mut back = Vec::new();
        let mut cur = d.unit_range(ReadUnit::Line, total);
        while let Some((s, e)) = cur {
            back.push((s, e));
            cur = d.prev_unit(ReadUnit::Line, s);
        }
        back.reverse();
        assert_eq!(back, lines, "{ctx}: line walk back");
        if total > 0 {
            let mut words = 0usize;
            let mut cover = 0u32;
            let mut cur = d.unit_range(ReadUnit::Word, 0);
            while let Some((s, e)) = cur {
                words += 1;
                assert!(e >= s);
                assert!(
                    s == cover || s == cover + 1,
                    "{ctx}: word gap {s} after {cover}"
                );
                cover = e;
                cur = d.next_unit(ReadUnit::Word, s);
                assert!(
                    words <= flat.len() + 2,
                    "{ctx}: word walk does not terminate"
                );
            }
            let mut n = 0u32;
            let mut cur = d.unit_range(ReadUnit::Char, 0);
            while let Some((s, _)) = cur {
                n += 1;
                cur = d.next_unit(ReadUnit::Char, s);
            }
            assert_eq!(n, total);
            let mut words_back = 0usize;
            let mut cur = d.unit_range(ReadUnit::Word, total - 1);
            while let Some((s, _)) = cur {
                words_back += 1;
                cur = d.prev_unit(ReadUnit::Word, s);
                assert!(
                    words_back <= flat.len() + 2,
                    "{ctx}: word back does not terminate"
                );
            }
            assert_eq!(words_back, words, "{ctx}: word walk back vs forward");
        }
    }
}

fn run_seed(seed: u64, steps: usize) -> (std::collections::BTreeMap<&'static str, usize>, usize) {
    let (w, r) = channel(seed ^ 0xABCD);
    let mut h = Harness {
        model: Model::default(),
        w,
        r,
        rng: Rng::new(seed),
        seed,
        step: 0,
        counts: std::collections::BTreeMap::new(),
        max_nodes: 0,
    };
    // Bootstrap with a root so that most ops have a target.
    let root = SubtreeSpec::with_children(
        NodeSpec::new(Role::Document),
        (0..3).map(|_| rand_spec(&mut h.rng, 2)).collect(),
    );
    let op = DocOp::InsertSubtree {
        parent: None,
        before: None,
        subtree: Arc::new(root.clone()),
    };
    h.exec(&op, |m, ()| {
        let mut out = Vec::new();
        m.add_spec(&root, None, &mut out);
        m.root = Some(out[0]);
        out
    });
    for _ in 0..steps {
        h.step_op();
    }
    // Final publish: the reader copy must equal the model and the writer copy.
    h.w.publish();
    let pub_ids: Vec<DocNodeId> = h.r.with(|d| d.preorder().collect());
    let back_ids: Vec<DocNodeId> = h.w.doc().preorder().collect();
    assert_eq!(pub_ids, back_ids, "seed {seed}: copies diverged");
    let live = h.r.snapshot();
    assert_eq!(live.validate(), Ok(()), "seed {seed}");
    (h.counts, h.max_nodes)
}

#[test]
fn random_ops_match_naive_model() {
    let mut total: std::collections::BTreeMap<&'static str, usize> =
        std::collections::BTreeMap::new();
    let mut biggest = 0;
    for seed in 1..=60 {
        let (c, m) = run_seed(seed, 120);
        biggest = biggest.max(m);
        for (k, v) in c {
            *total.entry(k).or_default() += v;
        }
    }
    // Every op kind must actually run, and documents must get big enough to be interesting.
    for k in [
        "insert",
        "remove",
        "attach",
        "move",
        "reorder",
        "sync",
        "set_text",
        "set_props",
        "reap",
    ] {
        assert!(
            total.get(k).copied().unwrap_or(0) >= 50,
            "op {k} under-exercised: {total:?}"
        );
    }
    assert!(biggest >= 60, "documents too small: {biggest}");
}

#[test]
fn longer_sequences_match_naive_model() {
    for seed in 1000..1006 {
        run_seed(seed, 700);
    }
}

#[test]
fn id_stays_with_content_across_detach_and_reattach() {
    // SPA churn, ADR-0003 IL-R3/R4: remove x N then add x N keeps the logical ids.
    let (mut w, r) = channel(5);
    let mk = |role, sig| SubtreeSpec::leaf(NodeSpec::new(role).sig(sig));
    let doc = SubtreeSpec::with_children(
        NodeSpec::new(Role::Document),
        vec![mk(Role::Heading, 1), mk(Role::Link, 2), mk(Role::Button, 3)],
    );
    let out = w.apply(DocOp::InsertSubtree {
        parent: None,
        before: None,
        subtree: Arc::new(doc),
    });
    let ids = out.map(|o| o.created).unwrap_or_default();
    assert_eq!(ids.len(), 4);
    let root = ids[0];
    w.publish();
    let (old, kids): (Vec<u64>, Vec<DocNodeId>) = {
        let d = w.doc();
        let kids: Vec<DocNodeId> = d.children(root).collect();
        (
            kids.iter()
                .map(|c| d.node(*c).map_or(0, |n| n.sig))
                .collect(),
            kids,
        )
    };
    // The framework re-renders: children come back in another order, one is new.
    let new_keys = [3u64, 2, 9, 1];
    let align = align_by_key(&old, &new_keys);
    let desired: Vec<ChildSpec> = new_keys
        .iter()
        .zip(&align)
        .map(|(k, a)| match a {
            Some(i) => ChildSpec::Existing(kids[*i]),
            None => ChildSpec::New(Arc::new(mk(Role::Paragraph, *k))),
        })
        .collect();
    assert!(
        w.apply(DocOp::SyncChildren {
            parent: root,
            desired
        })
        .is_ok()
    );
    w.publish();
    let after: Vec<DocNodeId> = r.with(|d| d.children(root).collect());
    assert_eq!(after.len(), 4);
    assert_eq!(after[0], ids[3]);
    assert_eq!(after[1], ids[2]);
    assert_eq!(after[3], ids[1]);
    // Route swap: everything removed, then the heading is found among the defunct and restored.
    assert!(
        w.apply(DocOp::SyncChildren {
            parent: root,
            desired: vec![]
        })
        .is_ok()
    );
    let found = w.doc().find_detached(1);
    assert_eq!(found, Some(ids[1]));
    assert!(w.doc().node(ids[1]).is_some());
    assert!(!w.doc().is_attached(ids[1]));
    assert!(
        w.apply(DocOp::AttachSubtree {
            id: ids[1],
            parent: root,
            before: None
        })
        .is_ok()
    );
    assert!(w.doc().is_attached(ids[1]));
    // Reaping frees slots; ids go stale and reused slots get a new generation.
    assert!(w.apply(DocOp::Reap { id: None }).is_ok());
    assert!(!w.doc().contains(ids[2]));
    let again = w.apply(DocOp::InsertSubtree {
        parent: Some(root),
        before: None,
        subtree: Arc::new(mk(Role::Heading, 7)),
    });
    let nid = again.map(|o| o.created[0]).ok();
    assert!(nid.is_some_and(|n| !ids.contains(&n)));
    assert!(w.doc().validate().is_ok());
}

#[test]
fn rejected_ops_do_not_mutate() {
    let (mut w, _r) = channel(2);
    let leaf = |role| Arc::new(SubtreeSpec::leaf(NodeSpec::new(role)));
    assert_eq!(
        w.apply(DocOp::InsertSubtree {
            parent: Some(DocNodeId::from_u64(0)),
            before: None,
            subtree: leaf(Role::Link)
        }),
        Err(DocError::NoRoot)
    );
    let root = w
        .apply(DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: leaf(Role::Document),
        })
        .map(|o| o.created[0]);
    let Ok(root) = root else {
        panic!("root insert failed")
    };
    assert_eq!(
        w.apply(DocOp::InsertSubtree {
            parent: None,
            before: None,
            subtree: leaf(Role::Document)
        }),
        Err(DocError::RootExists)
    );
    assert_eq!(
        w.apply(DocOp::RemoveSubtree { id: root }),
        Err(DocError::RootImmutable)
    );
    let a = w
        .apply(DocOp::InsertSubtree {
            parent: Some(root),
            before: None,
            subtree: leaf(Role::Link),
        })
        .map(|o| o.created[0]);
    let Ok(a) = a else { panic!("insert failed") };
    assert_eq!(
        w.apply(DocOp::Reap { id: Some(a) }),
        Err(DocError::NotDetached(a))
    );
    assert_eq!(
        w.apply(DocOp::MoveSubtree {
            id: root,
            parent: a,
            before: None
        }),
        Err(DocError::RootImmutable)
    );
    let stale = DocNodeId::from_u64(a.to_u64() + (1 << 32));
    assert_eq!(
        w.apply(DocOp::SetText {
            id: stale,
            text: None
        }),
        Err(DocError::Stale(stale))
    );
    assert_eq!(
        w.apply(DocOp::SyncChildren {
            parent: root,
            desired: vec![ChildSpec::Existing(a), ChildSpec::Existing(a)]
        }),
        Err(DocError::Duplicate(a))
    );
    assert_eq!(w.doc().len(), 2);
    assert!(w.doc().validate().is_ok());
}

#[test]
fn deep_chain_does_not_overflow_the_stack() {
    // 20k-deep chain: iterative builders must cope (tree depth is unbounded on the web).
    let mut spec = SubtreeSpec::leaf(NodeSpec::new(Role::StaticText).text("leaf"));
    for _ in 0..20_000 {
        spec = SubtreeSpec::with_children(NodeSpec::new(Role::Panel), vec![spec]);
    }
    let mut d = WebDoc::new(1);
    let child = std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || {
            let r = d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(spec),
            });
            assert!(r.is_ok());
            assert_eq!(d.len(), 20_001);
            assert_eq!(d.validate(), Ok(()));
            assert_eq!(d.text_in(0, 4), "leaf");
        });
    assert!(child.is_ok_and(|h| h.join().is_ok()));
}
