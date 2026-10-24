// SPDX-License-Identifier: MIT OR Apache-2.0
//! The stub S0 registers (node table with six class stamps), `FrozenRows` (the immutable per-batch
//! snapshot a plugin reads) and the MQ-2 staleness token (SPEC 2.2, plugin-fusion-math MQ-2).

use std::collections::HashMap;
use std::sync::Arc;

pub type NodeId = u32;
pub type PluginId = u16;
pub type JobId = u64;

/// The six read classes of MQ-2.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
#[repr(u8)]
pub enum Class {
    Struct = 0,
    Core = 1,
    Names = 2,
    Text = 3,
    Attrs = 4,
    Geom = 5,
}

pub const NCLASS: usize = 6;
pub const ALL_CLASSES: [Class; NCLASS] = [
    Class::Struct,
    Class::Core,
    Class::Names,
    Class::Text,
    Class::Attrs,
    Class::Geom,
];

/// Role codes of the stub tree.
pub const ROLE_WINDOW: u16 = 1;
pub const ROLE_LIST: u16 = 2;
pub const ROLE_ROW: u16 = 3;
pub const ROLE_CELL: u16 = 4;
pub const ROLE_LABEL: u16 = 5;
pub const ROLE_BUTTON: u16 = 6;
pub const NROLES: u16 = 7;

pub const STATE_FOCUSED: u64 = 1;

#[derive(Clone, Debug)]
pub struct Node {
    pub role: u16,
    pub states: u64,
    pub name: String,
    pub text: String,
    pub class_attr: String,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    /// `stamp[c]` = value of the writer clock `T` at the last S0 change of class `c`.
    pub stamp: [u64; NCLASS],
    /// Anchor binding generation (`BindGen` of MQ-2).
    pub bind_gen: u64,
}

/// S0 registers: the observed view the plugins read and the stamps the staleness test compares.
pub struct NodeTable {
    pub nodes: Vec<Node>,
    /// Writer clock `T`, bumped on every S0 change.
    pub t: u64,
}

/// SplitMix64: seeded, tiny, good enough for fixtures.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    pub fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl NodeTable {
    /// A window / list / row / cell tree of `n` nodes. Rows have 2 label children and a button.
    pub fn build(n: usize, seed: u64) -> NodeTable {
        let mut rng = Rng(seed ^ 0xA11C_E5EE_D5ED);
        let mut nodes: Vec<Node> = Vec::with_capacity(n.max(8));
        let mk = |role: u16, parent: Option<NodeId>, i: usize, rng: &mut Rng| Node {
            role,
            states: 0,
            name: format!("node-{i}-{:08x}", rng.next() as u32),
            text: format!("text of node {i} with some words {:x}", rng.next() as u32),
            class_attr: format!("c{}", rng.below(8)),
            parent,
            children: Vec::new(),
            stamp: [0; NCLASS],
            bind_gen: 1,
        };
        nodes.push(mk(ROLE_WINDOW, None, 0, &mut rng));
        nodes.push(mk(ROLE_LIST, Some(0), 1, &mut rng));
        nodes[0].children.push(1);
        while nodes.len() < n.max(8) {
            let i = nodes.len();
            let row = i as NodeId;
            nodes.push(mk(ROLE_ROW, Some(1), i, &mut rng));
            nodes[1].children.push(row);
            for role in [ROLE_LABEL, ROLE_LABEL, ROLE_BUTTON] {
                if nodes.len() >= n.max(8) {
                    break;
                }
                let j = nodes.len();
                nodes.push(mk(role, Some(row), j, &mut rng));
                nodes[row as usize].children.push(j as NodeId);
            }
        }
        NodeTable { nodes, t: 1 }
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// S0 change of class `c` on `n`: bump `T`, stamp.
    pub fn touch(&mut self, n: NodeId, c: Class) {
        self.t += 1;
        if let Some(node) = self.nodes.get_mut(n as usize) {
            node.stamp[c as usize] = self.t;
        }
    }

    /// Anchor rebound: bumps `BindGen` of the node.
    pub fn rebind(&mut self, n: NodeId) {
        self.t += 1;
        if let Some(node) = self.nodes.get_mut(n as usize) {
            node.bind_gen += 1;
        }
    }

    /// Focus chain of `focus`: itself, its children, then ancestors up to the root, then siblings
    /// until `max` nodes (the focus chain / active descendant of ADR-0026 2.4.2).
    pub fn focus_chain(&self, focus: NodeId, max: usize, out: &mut Vec<NodeId>) {
        out.clear();
        let push = |out: &mut Vec<NodeId>, n: NodeId| {
            if out.len() < max && !out.contains(&n) {
                out.push(n);
            }
        };
        push(out, focus);
        let Some(f) = self.nodes.get(focus as usize) else {
            return;
        };
        for c in &f.children {
            push(out, *c);
        }
        let mut cur = f.parent;
        while let Some(p) = cur {
            push(out, p);
            cur = self.nodes[p as usize].parent;
        }
        if let Some(p) = f.parent {
            for s in &self.nodes[p as usize].children {
                push(out, *s);
            }
        }
        // Pad with nearby ids so big chains (WQ-1: 64 nodes) are reachable on small trees.
        let mut k = focus;
        while out.len() < max && (k as usize + 1) < self.nodes.len() {
            k += 1;
            push(out, k);
        }
    }
}

/// One row of a frozen snapshot.
#[derive(Clone, Debug)]
pub struct Row {
    pub node: NodeId,
    pub role: u16,
    pub states: u64,
    pub name: Box<str>,
    pub text: Box<str>,
    pub class_attr: Box<str>,
    pub parent: Option<NodeId>,
    pub children: Vec<NodeId>,
    pub bind_gen: u64,
}

/// Immutable snapshot of the observed view for one plugin batch (eager only).
#[derive(Debug)]
pub struct FrozenRows {
    /// Writer clock at the snapshot: the `t` of the staleness token.
    pub t: u64,
    pub rows: Vec<Row>,
    index: HashMap<NodeId, u32>,
}

impl FrozenRows {
    pub fn get(&self, n: NodeId) -> Option<&Row> {
        self.index.get(&n).map(|i| &self.rows[*i as usize])
    }

    pub fn contains(&self, n: NodeId) -> bool {
        self.index.contains_key(&n)
    }

    /// Approximate heap size of the snapshot (M7 reports it at 16 and 64 nodes).
    pub fn approx_bytes(&self) -> usize {
        self.rows
            .iter()
            .map(|r| {
                std::mem::size_of::<Row>()
                    + r.name.len()
                    + r.text.len()
                    + r.class_attr.len()
                    + r.children.len() * 4
            })
            .sum::<usize>()
            + self.index.len() * 16
    }
}

/// Builds `FrozenRows` from the writer's table. The runner calls it on the writer thread (its cost
/// is inside the stall, SPEC 4). The batch nodes, their parents and their children are included.
pub struct FrozenRowsFactory<'a> {
    pub table: &'a NodeTable,
}

impl FrozenRowsFactory<'_> {
    pub fn build(&self, nodes: &[NodeId]) -> Arc<FrozenRows> {
        let mut ids: Vec<NodeId> = Vec::with_capacity(nodes.len() * 4);
        for n in nodes {
            ids.push(*n);
            if let Some(node) = self.table.nodes.get(*n as usize) {
                if let Some(p) = node.parent {
                    ids.push(p);
                }
                ids.extend(node.children.iter().take(8).copied());
            }
        }
        ids.sort_unstable();
        ids.dedup();
        let mut rows = Vec::with_capacity(ids.len());
        let mut index = HashMap::with_capacity(ids.len());
        for id in ids {
            if let Some(n) = self.table.nodes.get(id as usize) {
                index.insert(id, rows.len() as u32);
                rows.push(Row {
                    node: id,
                    role: n.role,
                    states: n.states,
                    name: n.name.as_str().into(),
                    text: n.text.as_str().into(),
                    class_attr: n.class_attr.as_str().into(),
                    parent: n.parent,
                    children: n.children.iter().take(8).copied().collect(),
                    bind_gen: n.bind_gen,
                });
            }
        }
        Arc::new(FrozenRows {
            t: self.table.t,
            rows,
            index,
        })
    }
}

/// Cap on the explicit read set of a token (beyond: scope-wide stamp).
pub const READ_SET_CAP: usize = 256;

/// The host-minted MQ-2 token `(t, R, M, g)`.
#[derive(Clone, Debug, Default)]
pub struct Token {
    pub t: u64,
    /// Explicit read set: node and the bit set of read classes (bit = `Class as u8`).
    pub reads: Vec<(NodeId, u8)>,
    /// Set when the read set exceeded [`READ_SET_CAP`]: the token then covers the whole scope.
    pub scope_wide: Option<u8>,
    /// `g`: bind generation of the call's target nodes at the snapshot, taken from the token.
    pub bind: Vec<(NodeId, u64)>,
}

impl Token {
    pub fn new(t: u64) -> Token {
        Token {
            t,
            ..Token::default()
        }
    }

    pub fn note_read(&mut self, n: NodeId, c: Class) {
        let bit = 1u8 << (c as u8);
        if let Some(mask) = self.scope_wide.as_mut() {
            *mask |= bit;
            return;
        }
        if let Some(e) = self.reads.iter_mut().find(|e| e.0 == n) {
            e.1 |= bit;
            return;
        }
        if self.reads.len() >= READ_SET_CAP {
            let mut m = bit;
            for e in &self.reads {
                m |= e.1;
            }
            self.scope_wide = Some(m);
            self.reads.clear();
        } else {
            self.reads.push((n, bit));
        }
    }

    /// Stale iff some `n in R, c in M` has `stamp[n][c] > t`, or `BindGen(target, now) != g`.
    /// Scope-wide tokens compare against every node of the table for the classes read.
    pub fn is_stale(&self, table: &NodeTable) -> bool {
        for (n, g) in &self.bind {
            match table.nodes.get(*n as usize) {
                Some(node) if node.bind_gen == *g => {}
                _ => return true,
            }
        }
        if let Some(mask) = self.scope_wide {
            return table.nodes.iter().any(|node| {
                ALL_CLASSES
                    .iter()
                    .any(|c| mask & (1 << (*c as u8)) != 0 && node.stamp[*c as usize] > self.t)
            });
        }
        for (n, mask) in &self.reads {
            let Some(node) = table.nodes.get(*n as usize) else {
                return true;
            };
            for c in ALL_CLASSES {
                if mask & (1 << (c as u8)) != 0 && node.stamp[c as usize] > self.t {
                    return true;
                }
            }
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_shape() {
        let t = NodeTable::build(100, 1);
        assert_eq!(t.len(), 100);
        let mut ch = Vec::new();
        t.focus_chain(5, 16, &mut ch);
        assert_eq!(ch.len(), 16);
        assert_eq!(ch[0], 5);
    }

    #[test]
    fn token_staleness_rules() {
        let mut t = NodeTable::build(64, 2);
        let mut tok = Token::new(t.t);
        tok.note_read(3, Class::Names);
        assert!(!tok.is_stale(&t));
        t.touch(3, Class::Text); // class not in M: still fresh
        assert!(!tok.is_stale(&t));
        t.touch(4, Class::Names); // node not in R: still fresh
        assert!(!tok.is_stale(&t));
        t.touch(3, Class::Names);
        assert!(tok.is_stale(&t));
    }

    #[test]
    fn token_bind_gen_and_scope_wide() {
        let mut t = NodeTable::build(600, 3);
        let mut tok = Token::new(t.t);
        tok.bind.push((7, t.nodes[7].bind_gen));
        assert!(!tok.is_stale(&t));
        t.rebind(7);
        assert!(tok.is_stale(&t));
        let mut wide = Token::new(t.t);
        for n in 0..300u32 {
            wide.note_read(n, Class::Core);
        }
        assert!(wide.scope_wide.is_some());
        assert!(!wide.is_stale(&t));
        t.touch(500, Class::Core); // outside the original 256, scope-wide still catches it
        assert!(wide.is_stale(&t));
    }

    #[test]
    fn frozen_rows_include_neighbourhood() {
        let t = NodeTable::build(64, 4);
        let f = FrozenRowsFactory { table: &t };
        let r = f.build(&[5]);
        assert!(r.contains(5));
        assert!(r.get(5).is_some_and(|x| x.parent.is_some()));
        assert!(r.approx_bytes() > 0);
    }
}
