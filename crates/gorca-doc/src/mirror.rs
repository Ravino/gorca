use std::collections::HashMap;
use std::sync::Arc;

use gorca_model::text::scalar_to_byte;
use gorca_model::{
    AttrMap, DocDelta, DocHint, DocId, DocNodeId, Role, SrcNode, SrcNodeId, StateSet, TextAnchor,
    TextEdit,
};

/// Section 9.4: a snapshot is published after the focus neighbourhood is in, then every 50 ms
/// or 2,000 nodes while building.
pub const SNAPSHOT_EVERY_MS: u64 = 50;
pub const SNAPSHOT_EVERY_NODES: usize = 2_000;

#[derive(Clone, Debug, PartialEq)]
pub struct DocNode {
    pub id: DocNodeId,
    pub src: SrcNodeId,
    pub role: Role,
    pub states: StateSet,
    pub name: Option<Arc<str>>,
    /// Flat text of this node (scalar offsets).
    pub text: Option<Arc<str>>,
    pub attrs: AttrMap,
    pub parent: Option<DocNodeId>,
    pub children: Vec<DocNodeId>,
}

/// Virtual-cursor position: `(DocNodeId, offset)` (IR-7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DocCursor {
    pub node: DocNodeId,
    pub offset: u32,
}

/// What survives a source detach (IR-7).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Bookmark {
    Node(DocCursor),
    /// Relocated by the NVDA-style bookmark rule. TBD.
    Anchor(TextAnchor),
}

/// Immutable view published to readers. Cloned from the mirror at publish time.
/// TBD: structural sharing (persistent map) once documents reach 100k nodes (M-15).
#[derive(Clone, Debug)]
pub struct DocSnapshot {
    pub doc: DocId,
    pub nav_id: u64,
    pub revision: u64,
    pub root: Option<DocNodeId>,
    nodes: HashMap<DocNodeId, Arc<DocNode>>,
}

impl DocSnapshot {
    pub fn node(&self, id: DocNodeId) -> Option<&Arc<DocNode>> {
        self.nodes.get(&id)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum ApplyOutcome {
    Applied,
    /// A mutation batch ended; the caller should publish a snapshot.
    Settled(u64),
    Hint(DocHint),
    /// The delta referenced a node the mirror does not know. The caller counts it and may
    /// request a `Subtree` resync.
    UnknownNode(SrcNodeId),
    /// A text edit fell outside the node's text.
    TextRangeInvalid(SrcNodeId),
    /// `DocNodeId` space exhausted for this `nav_id`; the document must be rebuilt.
    IdSpaceExhausted,
}

#[derive(Debug)]
pub struct DocumentMirror {
    doc: DocId,
    nav_id: u64,
    revision: u64,
    next_id: u32,
    root: Option<DocNodeId>,
    by_src: HashMap<SrcNodeId, DocNodeId>,
    nodes: HashMap<DocNodeId, DocNode>,
}

impl DocumentMirror {
    pub fn new(doc: DocId) -> Self {
        Self {
            doc,
            nav_id: 0,
            revision: 0,
            next_id: 0,
            root: None,
            by_src: HashMap::new(),
            nodes: HashMap::new(),
        }
    }

    pub fn nav_id(&self) -> u64 {
        self.nav_id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// O(1) join target lookup (IR-2).
    pub fn doc_node(&self, src: SrcNodeId) -> Option<DocNodeId> {
        self.by_src.get(&src).copied()
    }

    pub fn node(&self, id: DocNodeId) -> Option<&DocNode> {
        self.nodes.get(&id)
    }

    fn alloc(&mut self, src: SrcNodeId) -> Option<DocNodeId> {
        if let Some(id) = self.by_src.get(&src) {
            return Some(*id);
        }
        let id = DocNodeId(self.next_id);
        self.next_id = self.next_id.checked_add(1)?;
        self.by_src.insert(src, id);
        self.nodes.insert(
            id,
            DocNode {
                id,
                src,
                role: Role::Unknown,
                states: StateSet::empty(),
                name: None,
                text: None,
                attrs: AttrMap::new(),
                parent: None,
                children: Vec::new(),
            },
        );
        Some(id)
    }

    pub fn apply(&mut self, delta: DocDelta) -> ApplyOutcome {
        self.revision += 1;
        match delta {
            DocDelta::Reset { root, nav_id } => {
                self.nodes.clear();
                self.by_src.clear();
                self.next_id = 0;
                self.nav_id = nav_id;
                match self.alloc(root) {
                    Some(id) => {
                        self.root = Some(id);
                        ApplyOutcome::Applied
                    }
                    None => ApplyOutcome::IdSpaceExhausted,
                }
            }
            DocDelta::Upsert(nodes) => self.upsert(nodes),
            DocDelta::Children { parent, children } => self.set_children(parent, &children),
            DocDelta::Text { node, edit } => self.edit_text(node, edit),
            DocDelta::Removed(ids) => {
                for src in ids {
                    if let Some(id) = self.by_src.get(&src).copied() {
                        self.detach_from_parent(id);
                        self.remove_subtree(id);
                    }
                }
                ApplyOutcome::Applied
            }
            DocDelta::Settled { batch } => ApplyOutcome::Settled(batch),
            DocDelta::Hint(h) => ApplyOutcome::Hint(h),
        }
    }

    fn upsert(&mut self, nodes: Vec<SrcNode>) -> ApplyOutcome {
        for n in nodes {
            let Some(id) = self.alloc(n.id) else {
                return ApplyOutcome::IdSpaceExhausted;
            };
            if let Some(d) = self.nodes.get_mut(&id) {
                d.role = n.role;
                d.states = n.states;
                d.name = n.name;
                d.text = n.text;
                d.attrs = n.attrs;
            }
        }
        ApplyOutcome::Applied
    }

    /// Keyed ordered-forest diff for one parent (SL-4): the new list is complete.
    fn set_children(&mut self, parent: SrcNodeId, children: &[SrcNodeId]) -> ApplyOutcome {
        let Some(pid) = self.by_src.get(&parent).copied() else {
            return ApplyOutcome::UnknownNode(parent);
        };
        let mut new_ids = Vec::with_capacity(children.len());
        for c in children {
            let Some(id) = self.alloc(*c) else {
                return ApplyOutcome::IdSpaceExhausted;
            };
            new_ids.push(id);
        }
        let old = self
            .nodes
            .get(&pid)
            .map(|p| p.children.clone())
            .unwrap_or_default();
        for gone in old.iter().filter(|o| !new_ids.contains(o)) {
            // Re-parented elsewhere in the meantime: leave it alone.
            if self.nodes.get(gone).and_then(|n| n.parent) == Some(pid) {
                self.remove_subtree(*gone);
            }
        }
        for id in &new_ids {
            // A node moving from another parent leaves that parent's list.
            let prev = self.nodes.get(id).and_then(|n| n.parent);
            if let Some(prev) = prev.filter(|p| *p != pid)
                && let Some(p) = self.nodes.get_mut(&prev)
            {
                p.children.retain(|c| c != id);
            }
            if let Some(n) = self.nodes.get_mut(id) {
                n.parent = Some(pid);
            }
        }
        if let Some(p) = self.nodes.get_mut(&pid) {
            p.children = new_ids;
        }
        ApplyOutcome::Applied
    }

    fn edit_text(&mut self, src: SrcNodeId, edit: TextEdit) -> ApplyOutcome {
        let Some(id) = self.by_src.get(&src).copied() else {
            return ApplyOutcome::UnknownNode(src);
        };
        let Some(node) = self.nodes.get_mut(&id) else {
            return ApplyOutcome::UnknownNode(src);
        };
        match edit {
            TextEdit::Replace(t) => node.text = Some(t),
            TextEdit::Splice { range, text } => {
                let cur = node.text.as_deref().unwrap_or("");
                let (Some(a), Some(b)) = (
                    scalar_to_byte(cur, range.start as usize),
                    scalar_to_byte(cur, range.end as usize),
                ) else {
                    return ApplyOutcome::TextRangeInvalid(src);
                };
                if a > b {
                    return ApplyOutcome::TextRangeInvalid(src);
                }
                let mut s = String::with_capacity(cur.len() - (b - a) + text.len());
                s.push_str(&cur[..a]);
                s.push_str(&text);
                s.push_str(&cur[b..]);
                node.text = Some(Arc::from(s));
            }
        }
        ApplyOutcome::Applied
    }

    fn detach_from_parent(&mut self, id: DocNodeId) {
        let parent = self.nodes.get(&id).and_then(|n| n.parent);
        if let Some(p) = parent.and_then(|p| self.nodes.get_mut(&p)) {
            p.children.retain(|c| *c != id);
        }
    }

    fn remove_subtree(&mut self, id: DocNodeId) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.remove(&n) {
                self.by_src.remove(&node.src);
                stack.extend(node.children);
                if self.root == Some(n) {
                    self.root = None;
                }
            }
        }
    }

    /// Publish an immutable snapshot (one atomic publish per batch).
    pub fn snapshot(&self) -> Arc<DocSnapshot> {
        Arc::new(DocSnapshot {
            doc: self.doc,
            nav_id: self.nav_id,
            revision: self.revision,
            root: self.root,
            nodes: self
                .nodes
                .iter()
                .map(|(k, v)| (*k, Arc::new(v.clone())))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn src(n: u64, role: Role, text: Option<&str>) -> SrcNode {
        SrcNode {
            id: SrcNodeId(n),
            role,
            states: StateSet::empty(),
            name: None,
            text: text.map(Arc::from),
            attrs: AttrMap::new(),
        }
    }

    fn build() -> DocumentMirror {
        let mut m = DocumentMirror::new(DocId(1));
        m.apply(DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 7,
        });
        m.apply(DocDelta::Upsert(vec![
            src(1, Role::Document, None),
            src(2, Role::Heading, Some("Title")),
            src(3, Role::Paragraph, Some("Body")),
        ]));
        m.apply(DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec![SrcNodeId(2), SrcNodeId(3)],
        });
        m
    }

    #[test]
    fn build_and_join_lookup() {
        let m = build();
        assert_eq!(m.nav_id(), 7);
        assert_eq!(m.len(), 3);
        let h = m.doc_node(SrcNodeId(2)).unwrap();
        assert_eq!(m.node(h).unwrap().role, Role::Heading);
        let root = m.doc_node(SrcNodeId(1)).unwrap();
        assert_eq!(m.node(root).unwrap().children.len(), 2);
        assert_eq!(m.node(h).unwrap().parent, Some(root));
        assert_eq!(m.doc_node(SrcNodeId(99)), None);
    }

    #[test]
    fn dropped_child_is_removed_with_its_subtree() {
        let mut m = build();
        m.apply(DocDelta::Upsert(vec![src(4, Role::Link, Some("x"))]));
        m.apply(DocDelta::Children {
            parent: SrcNodeId(3),
            children: smallvec![SrcNodeId(4)],
        });
        assert_eq!(m.len(), 4);
        m.apply(DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec![SrcNodeId(2)],
        });
        assert_eq!(m.len(), 2);
        assert_eq!(m.doc_node(SrcNodeId(3)), None);
        assert_eq!(m.doc_node(SrcNodeId(4)), None);
    }

    #[test]
    fn moved_child_survives_the_old_parents_diff() {
        let mut m = build();
        m.apply(DocDelta::Children {
            parent: SrcNodeId(3),
            children: smallvec![SrcNodeId(2)],
        });
        m.apply(DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec![SrcNodeId(3)],
        });
        assert!(m.doc_node(SrcNodeId(2)).is_some());
        let p = m.doc_node(SrcNodeId(3)).unwrap();
        assert_eq!(m.node(p).unwrap().children.len(), 1);
    }

    #[test]
    fn children_of_unknown_parent_is_reported() {
        let mut m = build();
        let out = m.apply(DocDelta::Children {
            parent: SrcNodeId(50),
            children: smallvec![],
        });
        assert_eq!(out, ApplyOutcome::UnknownNode(SrcNodeId(50)));
    }

    #[test]
    fn text_splice_uses_scalar_offsets() {
        let mut m = build();
        m.apply(DocDelta::Text {
            node: SrcNodeId(2),
            edit: TextEdit::Replace(Arc::from("a\u{1F600}c")),
        });
        let out = m.apply(DocDelta::Text {
            node: SrcNodeId(2),
            edit: TextEdit::Splice {
                range: 1..2,
                text: Arc::from("b"),
            },
        });
        assert_eq!(out, ApplyOutcome::Applied);
        let id = m.doc_node(SrcNodeId(2)).unwrap();
        assert_eq!(m.node(id).unwrap().text.as_deref(), Some("abc"));
        let bad = m.apply(DocDelta::Text {
            node: SrcNodeId(2),
            edit: TextEdit::Splice {
                range: 2..9,
                text: Arc::from("x"),
            },
        });
        assert_eq!(bad, ApplyOutcome::TextRangeInvalid(SrcNodeId(2)));
    }

    #[test]
    fn reset_starts_a_new_navigation_with_fresh_ids() {
        let mut m = build();
        m.apply(DocDelta::Reset {
            root: SrcNodeId(100),
            nav_id: 8,
        });
        assert_eq!(m.len(), 1);
        assert_eq!(m.nav_id(), 8);
        assert_eq!(m.doc_node(SrcNodeId(2)), None);
    }

    #[test]
    fn removed_detaches_from_parent_and_snapshot_is_immutable() {
        let mut m = build();
        let before = m.snapshot();
        m.apply(DocDelta::Removed(vec![SrcNodeId(2)]));
        let after = m.snapshot();
        assert_eq!(before.len(), 3);
        assert_eq!(after.len(), 2);
        assert!(after.revision > before.revision);
        let root = after.root.unwrap();
        assert_eq!(after.node(root).unwrap().children.len(), 1);
    }

    #[test]
    fn settled_asks_for_a_publish() {
        let mut m = build();
        assert_eq!(
            m.apply(DocDelta::Settled { batch: 4 }),
            ApplyOutcome::Settled(4)
        );
    }
}
