// SPDX-License-Identifier: GPL-3.0-or-later
//! Workload model: node construction, the tree shape, the reader command, the writer op mix.

use std::fmt::Write as _;
use std::sync::Arc;

use crate::store::{Key, NONE, Node, Reader, Session, Writer, splitmix};

pub const FANOUT: u64 = 8;
/// Reserved key for the visibility probe.
pub const PROBE: Key = u64::MAX - 1;
const HOT: usize = 64;

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(splitmix(seed) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        ((u128::from(self.next()) * u128::from(n.max(1))) >> 64) as u64
    }
}

fn name_of(i: Key, ver: u32) -> Box<str> {
    let mut s = String::with_capacity(20);
    let _ = write!(s, "item {i} v{}", ver % 100);
    s.into_boxed_str()
}

/// Static tree node `i` of an `n`-node k-ary tree (children 8i+1 ..= 8i+8).
pub fn mk_node(n: u64, i: Key, ver: u32) -> Arc<Node> {
    let first = i * FANOUT + 1;
    let children: Vec<Key> = (first..first + FANOUT).take_while(|c| *c < n).collect();
    Arc::new(Node {
        parent: if i == 0 { NONE } else { (i - 1) / FANOUT },
        states: splitmix(i ^ u64::from(ver)),
        version: ver,
        role: (i % 40) as u16,
        name: name_of(i, ver),
        children: children.into_boxed_slice(),
    })
}

/// A leaf added after the initial build.
pub fn mk_leaf(i: Key, parent: Key, ver: u32) -> Arc<Node> {
    Arc::new(Node {
        parent,
        states: splitmix(i ^ u64::from(ver)),
        version: ver,
        role: (i % 40) as u16,
        name: name_of(i, ver),
        children: Box::default(),
    })
}

/// A reordered copy of an internal node (children rotated).
fn mk_reordered(base: &Node, ver: u32) -> Arc<Node> {
    let mut children = base.children.to_vec();
    if children.len() > 1 {
        children.rotate_left(1);
    }
    Arc::new(Node {
        parent: base.parent,
        states: splitmix(base.states ^ u64::from(ver)),
        version: ver,
        role: base.role,
        name: base.name.clone(),
        children: children.into_boxed_slice(),
    })
}

/// Fill a store with the `n`-node tree through its writer.
pub fn build<W: Writer>(w: &mut W, n: u64) {
    for i in 0..n {
        w.put(i, mk_node(n, i, 0));
        if i % 65_536 == 65_535 {
            w.publish();
        }
    }
    w.publish();
}

/// One reader command: the focused node, its parent, a sibling through the parent's child list,
/// and its first child. Returns a checksum so the work cannot be optimised away.
pub struct ReadGen {
    rng: Rng,
    n: u64,
    hot: [Key; HOT],
}

impl ReadGen {
    pub fn new(seed: u64, n: u64) -> Self {
        let mut rng = Rng::new(seed);
        let mut hot = [0; HOT];
        for h in &mut hot {
            *h = rng.below(n);
        }
        Self { rng, n, hot }
    }

    pub fn pick(&mut self) -> Key {
        if self.rng.next() & 1 == 0 {
            self.hot[self.rng.below(HOT as u64) as usize]
        } else {
            self.rng.below(self.n)
        }
    }

    pub fn command<R: Reader>(&mut self, r: &mut R) -> u64 {
        let k = self.pick();
        let pick = self.rng.next();
        let s = r.session();
        let mut sum = 0u64;
        if let Some(node) = s.get(k) {
            sum = sum.wrapping_add(node.states);
            if let Some(f) = node.children.first() {
                if let Some(c) = s.get(*f) {
                    sum = sum.wrapping_add(u64::from(c.role));
                }
            }
            if node.parent != NONE {
                if let Some(p) = s.get(node.parent) {
                    sum = sum.wrapping_add(p.states);
                    if !p.children.is_empty() {
                        let sib = p.children[(pick % p.children.len() as u64) as usize];
                        if let Some(sn) = s.get(sib) {
                            sum = sum.wrapping_add(u64::from(sn.version));
                        }
                    }
                }
            }
        }
        sum
    }
}

/// Writer op generator: 70 % property change, 10 % insert (node plus parent summary), 10 %
/// remove (node plus parent summary), 10 % children reorder. Counts store mutations.
pub struct WriteGen {
    rng: Rng,
    n: u64,
    ver: u32,
    next_dyn: Key,
    dynamic: Vec<(Key, Key)>,
    pub mutations: u64,
}

impl WriteGen {
    pub fn new(seed: u64, n: u64) -> Self {
        Self {
            rng: Rng::new(seed),
            n,
            ver: 1,
            next_dyn: n,
            dynamic: Vec::new(),
            mutations: 0,
        }
    }

    fn internal(&mut self) -> Key {
        self.rng.below((self.n / FANOUT).max(1))
    }

    pub fn op<W: Writer>(&mut self, w: &mut W) {
        self.ver = self.ver.wrapping_add(1);
        let ver = self.ver;
        match self.rng.below(100) {
            0..70 => {
                let k = self.rng.below(self.n);
                w.put(k, mk_node(self.n, k, ver));
                self.mutations += 1;
            }
            70..80 => {
                let p = self.internal();
                let k = self.next_dyn;
                self.next_dyn += 1;
                w.put(k, mk_leaf(k, p, ver));
                w.put(p, mk_node(self.n, p, ver));
                self.dynamic.push((k, p));
                self.mutations += 2;
            }
            80..90 => {
                if self.dynamic.is_empty() {
                    let k = self.rng.below(self.n);
                    w.put(k, mk_node(self.n, k, ver));
                    self.mutations += 1;
                } else {
                    let i = self.rng.below(self.dynamic.len() as u64) as usize;
                    let (k, p) = self.dynamic.swap_remove(i);
                    w.remove(k);
                    w.put(p, mk_node(self.n, p, ver));
                    self.mutations += 2;
                }
            }
            _ => {
                let p = self.internal();
                let base = mk_node(self.n, p, ver);
                w.put(p, mk_reordered(&base, ver));
                self.mutations += 1;
            }
        }
    }
}

// ---------------------------------------------------------------- host / process helpers

pub fn read_file(p: &str) -> String {
    std::fs::read_to_string(p).unwrap_or_default()
}

/// Resident set size in bytes.
pub fn rss_bytes() -> u64 {
    let s = read_file("/proc/self/statm");
    s.split_whitespace()
        .nth(1)
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        * 4096
}

/// CPU seconds (user + system) of the process or, with `thread`, of the calling thread.
pub fn cpu_secs(thread: bool) -> f64 {
    let s = read_file(if thread {
        "/proc/thread-self/stat"
    } else {
        "/proc/self/stat"
    });
    // Fields after the parenthesised comm: state is field 3; utime/stime are fields 14 and 15.
    let after = s.rsplit_once(')').map_or("", |(_, r)| r);
    let f: Vec<&str> = after.split_whitespace().collect();
    let t = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    (t(11) + t(12)) / 100.0
}

pub fn loadavg() -> f64 {
    read_file("/proc/loadavg")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}
