// SPDX-License-Identifier: GPL-3.0-or-later
//! A minimal mirror that applies `DocDelta`s with the same semantics as `gorca-doc`
//! (`Upsert` replaces node data, `Children` is a complete ordered list that drops absent
//! children, `Removed` drops a subtree). The harness uses it instead of `gorca-doc` so that it
//! measures the source alone and does not move when the arena is rewritten.

use std::collections::HashMap;
use std::sync::Arc;

use gorca_doc_source_api::web_attrs;
use gorca_model::{DocDelta, Role, SrcNode, SrcNodeId, TextEdit};

#[derive(Clone, Debug)]
pub struct ONode {
    pub role: Role,
    pub name: Option<Arc<str>>,
    pub tag: Option<Arc<str>>,
    pub dom_id: Option<Arc<str>>,
    pub text: Option<Arc<str>>,
    pub children: Vec<SrcNodeId>,
    pub parent: Option<SrcNodeId>,
}

#[derive(Debug, Default)]
pub struct Oracle {
    pub nodes: HashMap<SrcNodeId, ONode>,
    pub root: Option<SrcNodeId>,
    pub deltas: u64,
    pub resets: u64,
    pub unknown_parent: u64,
}

fn attr(n: &SrcNode, k: gorca_model::AttrKey) -> Option<Arc<str>> {
    n.attrs
        .iter()
        .find(|(a, _)| *a == k)
        .map(|(_, v)| Arc::clone(v))
}

impl Oracle {
    pub fn apply(&mut self, d: DocDelta) {
        self.deltas += 1;
        match d {
            DocDelta::Reset { root, .. } => {
                self.nodes.clear();
                self.root = Some(root);
                self.resets += 1;
            }
            DocDelta::Upsert(ns) => {
                for n in ns {
                    let e = self.nodes.entry(n.id).or_insert_with(|| ONode {
                        role: Role::Unknown,
                        name: None,
                        tag: None,
                        dom_id: None,
                        text: None,
                        children: Vec::new(),
                        parent: None,
                    });
                    e.role = n.role;
                    e.tag = attr(&n, web_attrs::TAG);
                    e.dom_id = attr(&n, web_attrs::DOM_ID);
                    e.name.clone_from(&n.name);
                    e.text.clone_from(&n.text);
                }
            }
            DocDelta::Children { parent, children } => {
                if !self.nodes.contains_key(&parent) {
                    self.unknown_parent += 1;
                    return;
                }
                let old = self
                    .nodes
                    .get(&parent)
                    .map(|n| n.children.clone())
                    .unwrap_or_default();
                for gone in old.iter().filter(|o| !children.contains(o)) {
                    if self.nodes.get(gone).and_then(|n| n.parent) == Some(parent) {
                        self.remove_subtree(*gone);
                    }
                }
                for c in &children {
                    let prev = self.nodes.get(c).and_then(|n| n.parent);
                    if let Some(p) = prev.filter(|p| *p != parent)
                        && let Some(pn) = self.nodes.get_mut(&p)
                    {
                        pn.children.retain(|x| x != c);
                    }
                    if let Some(n) = self.nodes.get_mut(c) {
                        n.parent = Some(parent);
                    }
                }
                if let Some(p) = self.nodes.get_mut(&parent) {
                    p.children = children.to_vec();
                }
            }
            DocDelta::Text { node, edit } => {
                if let (Some(n), TextEdit::Replace(t)) = (self.nodes.get_mut(&node), edit) {
                    n.text = Some(t);
                }
            }
            DocDelta::Removed(ids) => {
                for id in ids {
                    if let Some(p) = self.nodes.get(&id).and_then(|n| n.parent)
                        && let Some(pn) = self.nodes.get_mut(&p)
                    {
                        pn.children.retain(|c| *c != id);
                    }
                    self.remove_subtree(id);
                }
            }
            DocDelta::Settled { .. } | DocDelta::Hint(_) => {}
        }
    }

    fn remove_subtree(&mut self, id: SrcNodeId) {
        let mut stack = vec![id];
        while let Some(n) = stack.pop() {
            if let Some(node) = self.nodes.remove(&n) {
                stack.extend(node.children);
            }
        }
    }

    /// `(role, name, tag)` of every node reachable from the root, in DFS pre-order.
    pub fn signature(&self) -> Signature {
        let mut out = Vec::with_capacity(self.nodes.len());
        let Some(root) = self.root else { return out };
        let mut stack = vec![root];
        while let Some(id) = stack.pop() {
            let Some(n) = self.nodes.get(&id) else {
                continue;
            };
            out.push((n.role, n.name.clone(), n.tag.clone()));
            for c in n.children.iter().rev() {
                stack.push(*c);
            }
        }
        out
    }

    /// DOM id -> logical id, for nodes that expose a DOM id.
    pub fn dom_ids(&self) -> HashMap<Arc<str>, SrcNodeId> {
        self.nodes
            .iter()
            .filter_map(|(id, n)| n.dom_id.clone().map(|d| (d, *id)))
            .collect()
    }

    /// Name -> logical id for nodes whose accessible name starts with `prefix`.
    pub fn names_with_prefix(&self, prefix: &str) -> HashMap<Arc<str>, SrcNodeId> {
        self.nodes
            .iter()
            .filter_map(|(id, n)| match &n.name {
                Some(nm) if nm.starts_with(prefix) => Some((Arc::clone(nm), *id)),
                _ => None,
            })
            .collect()
    }

    pub fn reachable(&self) -> usize {
        self.signature().len()
    }
}

pub type Signature = Vec<(Role, Option<Arc<str>>, Option<Arc<str>>)>;

/// Index of the first difference between two signatures, if any.
pub fn first_diff(a: &Signature, b: &Signature) -> Option<usize> {
    if let Some(i) = a.iter().zip(b).position(|(x, y)| x != y) {
        return Some(i);
    }
    (a.len() != b.len()).then(|| a.len().min(b.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::smallvec;

    fn n(id: u64, name: &str) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role: Role::Panel,
            states: gorca_model::StateSet::empty(),
            name: Some(name.into()),
            text: None,
            attrs: smallvec![(web_attrs::DOM_ID, Arc::from(format!("d{id}").as_str()))],
            local: None,
            rec: None,
        }
    }

    #[test]
    fn apply_children_drops_absent_nodes() {
        let mut o = Oracle::default();
        o.apply(DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        });
        o.apply(DocDelta::Upsert(vec![n(1, "r"), n(2, "a"), n(3, "b")]));
        o.apply(DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec![SrcNodeId(2), SrcNodeId(3)],
        });
        assert_eq!(o.reachable(), 3);
        o.apply(DocDelta::Children {
            parent: SrcNodeId(1),
            children: smallvec![SrcNodeId(3)],
        });
        assert_eq!(o.reachable(), 2);
        assert!(!o.nodes.contains_key(&SrcNodeId(2)));
        assert_eq!(o.dom_ids().len(), 2);
    }
}
