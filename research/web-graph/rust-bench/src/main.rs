//! Replays a navigation trace produced by `bench_navindex.py gen` against two index
//! designs and reports latencies. Every query in the trace carries the expected
//! answer (from the Python reference), so the replay doubles as a test vector check.
//!
//!   cargo run --release -- ../traces/nav100k.trace
//!   cargo run --release -- --adversarial 100000
//!
//! Research code: std only, `expect` on malformed input is acceptable here.

use std::collections::BTreeMap;
use std::time::Instant;

const NIL: u32 = u32::MAX;
const NC: usize = 19;

// ---------------------------------------------------------------------------
// Mirror (arena). Production: slotmap with generational keys + intrusive links.
struct Mirror {
    parent: Vec<u32>,
    children: Vec<Vec<u32>>,
    mask: Vec<u64>,
}

impl Mirror {
    fn ensure(&mut self, v: u32) {
        let v = v as usize;
        if self.parent.len() <= v {
            self.parent.resize(v + 1, NIL);
            self.children.resize(v + 1, Vec::new());
            self.mask.resize(v + 1, 0);
        }
    }
    fn last_descendant(&self, mut v: u32) -> u32 {
        while let Some(&c) = self.children[v as usize].last() {
            v = c;
        }
        v
    }
    fn subtree_preorder(&self, v: u32, out: &mut Vec<u32>) {
        out.clear();
        let mut st = vec![v];
        while let Some(x) = st.pop() {
            out.push(x);
            st.extend(self.children[x as usize].iter().rev());
        }
    }
}

trait NavIndex {
    fn build(m: &Mirror, order: &[u32]) -> Self;
    fn on_insert(&mut self, m: &Mirror, pred: u32, order: &[u32]);
    fn on_delete(&mut self, m: &Mirror, order: &[u32]);
    fn on_mask(&mut self, m: &Mirror, v: u32, old: u64, new: u64);
    fn next(&self, m: &Mirror, c: usize, v: u32) -> Option<u32>;
    fn prev(&self, m: &Mirror, c: usize, v: u32) -> Option<u32>;
    fn kth(&self, m: &Mirror, c: usize, k: usize) -> Option<u32>;
    fn name() -> &'static str;
}

// ---------------------------------------------------------------------------
// Design A: order-maintenance labels (Bender et al. 2002) + per-category BTreeMap.
struct OmIndex {
    label: Vec<u64>,
    order: BTreeMap<u64, u32>,
    cat: Vec<BTreeMap<u64, u32>>,
    relabels: u64,
}

const U: u32 = 62;
const T: f64 = 1.5;

impl OmIndex {
    fn make_room(&mut self, m: &Mirror, lp: u64, k: usize) {
        let mut i = 1u32;
        let (lo, size, cnt) = loop {
            let size = 1u64 << i;
            let lo = lp & !(size - 1);
            let hi = lo.saturating_add(size);
            let cnt = self.order.range(lo..hi).count() + k;
            if (cnt as f64) * T.powi(i as i32) < size as f64 && size / (cnt as u64 + 1) >= 2 {
                break (lo, size, cnt);
            }
            i += 1;
            assert!(i <= U, "label universe exhausted");
        };
        let hi = lo + size;
        let step = size / (cnt as u64 + 1);
        let items: Vec<(u64, u32)> = self.order.range(lo..hi).map(|(&l, &v)| (l, v)).collect();
        for &(l, _) in &items {
            self.order.remove(&l);
        }
        // New labels in rank order, leaving k free slots right after lp.
        let assigned: Vec<(u32, u64, u64)> = items
            .iter()
            .enumerate()
            .map(|(slot, &(l, v))| {
                let s = if l <= lp {
                    slot as u64
                } else {
                    (slot + k) as u64
                };
                (v, l, lo + (s + 1) * step)
            })
            .collect();
        // Remove all old category entries first, then insert new (avoids collisions
        // between an old label of one node and the new label of another).
        for &(v, old, _) in &assigned {
            let mut mk = m.mask[v as usize];
            while mk != 0 {
                let c = mk.trailing_zeros() as usize;
                self.cat[c].remove(&old);
                mk &= mk - 1;
            }
        }
        for &(v, _, new) in &assigned {
            self.label[v as usize] = new;
            self.order.insert(new, v);
            let mut mk = m.mask[v as usize];
            while mk != 0 {
                let c = mk.trailing_zeros() as usize;
                self.cat[c].insert(new, v);
                mk &= mk - 1;
            }
        }
        self.relabels += assigned.len() as u64;
    }
}

impl NavIndex for OmIndex {
    fn name() -> &'static str {
        "om-btree"
    }
    fn build(m: &Mirror, order: &[u32]) -> Self {
        let gap = (1u64 << U) / (order.len() as u64 + 1);
        let mut s = OmIndex {
            label: vec![0; m.parent.len()],
            order: BTreeMap::new(),
            cat: (0..NC).map(|_| BTreeMap::new()).collect(),
            relabels: 0,
        };
        for (i, &v) in order.iter().enumerate() {
            let l = (i as u64 + 1) * gap;
            s.label[v as usize] = l;
            s.order.insert(l, v);
            let mut mk = m.mask[v as usize];
            while mk != 0 {
                let c = mk.trailing_zeros() as usize;
                s.cat[c].insert(l, v);
                mk &= mk - 1;
            }
        }
        s
    }
    fn on_insert(&mut self, m: &Mirror, pred: u32, order: &[u32]) {
        if self.label.len() < m.parent.len() {
            self.label.resize(m.parent.len(), 0);
        }
        let k = order.len();
        let succ = |s: &Self, lp: u64| {
            s.order
                .range(lp + 1..)
                .next()
                .map_or(1u64 << U, |(&l, _)| l)
        };
        let mut lp = self.label[pred as usize];
        let mut ls = succ(self, lp);
        if ls - lp <= k as u64 {
            self.make_room(m, lp, k);
            lp = self.label[pred as usize];
            ls = succ(self, lp);
        }
        let step = (ls - lp) / (k as u64 + 1);
        for (i, &v) in order.iter().enumerate() {
            let l = lp + (i as u64 + 1) * step;
            self.label[v as usize] = l;
            self.order.insert(l, v);
            let mut mk = m.mask[v as usize];
            while mk != 0 {
                let c = mk.trailing_zeros() as usize;
                self.cat[c].insert(l, v);
                mk &= mk - 1;
            }
        }
    }
    fn on_delete(&mut self, m: &Mirror, order: &[u32]) {
        for &v in order {
            let l = self.label[v as usize];
            self.order.remove(&l);
            let mut mk = m.mask[v as usize];
            while mk != 0 {
                let c = mk.trailing_zeros() as usize;
                self.cat[c].remove(&l);
                mk &= mk - 1;
            }
        }
    }
    fn on_mask(&mut self, _m: &Mirror, v: u32, old: u64, new: u64) {
        let l = self.label[v as usize];
        let mut gone = old & !new;
        while gone != 0 {
            self.cat[gone.trailing_zeros() as usize].remove(&l);
            gone &= gone - 1;
        }
        let mut added = new & !old;
        while added != 0 {
            self.cat[added.trailing_zeros() as usize].insert(l, v);
            added &= added - 1;
        }
    }
    fn next(&self, _m: &Mirror, c: usize, v: u32) -> Option<u32> {
        let l = self.label[v as usize];
        self.cat[c].range(l + 1..).next().map(|(_, &x)| x)
    }
    fn prev(&self, _m: &Mirror, c: usize, v: u32) -> Option<u32> {
        let l = self.label[v as usize];
        self.cat[c].range(..l).next_back().map(|(_, &x)| x)
    }
    fn kth(&self, _m: &Mirror, c: usize, k: usize) -> Option<u32> {
        // BTreeMap has no rank: O(k). Reported separately; see report.
        self.cat[c].values().nth(k).copied()
    }
}

// ---------------------------------------------------------------------------
// Design B: implicit treap over preorder, augmented with size, OR-mask, counts.
#[derive(Clone, Copy)]
struct TNode {
    l: u32,
    r: u32,
    p: u32,
    prio: u32,
    size: u32,
    agg: u64,
    cnt: [u32; NC],
}

const TNIL: TNode = TNode {
    l: NIL,
    r: NIL,
    p: NIL,
    prio: 0,
    size: 0,
    agg: 0,
    cnt: [0; NC],
};

struct TreapIndex {
    t: Vec<TNode>,
    root: u32,
    rng: u64,
}

impl TreapIndex {
    fn rand(&mut self) -> u32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 32) as u32
    }
    #[inline]
    fn pull(&mut self, m: &Mirror, v: u32) {
        let n = self.t[v as usize];
        let mk = m.mask[v as usize];
        let mut size = 1;
        let mut agg = mk;
        let mut cnt = [0u32; NC];
        for (c, x) in cnt.iter_mut().enumerate() {
            *x = ((mk >> c) & 1) as u32;
        }
        for ch in [n.l, n.r] {
            if ch != NIL {
                let cn = &self.t[ch as usize];
                size += cn.size;
                agg |= cn.agg;
                for (x, y) in cnt.iter_mut().zip(cn.cnt.iter()) {
                    *x += y;
                }
            }
        }
        let n = &mut self.t[v as usize];
        n.size = size;
        n.agg = agg;
        n.cnt = cnt;
    }
    fn size(&self, v: u32) -> u32 {
        if v == NIL {
            0
        } else {
            self.t[v as usize].size
        }
    }
    fn build_seq(&mut self, m: &Mirror, order: &[u32]) -> u32 {
        if self.t.len() < m.parent.len() {
            self.t.resize(m.parent.len(), TNIL);
        }
        let mut stack: Vec<u32> = Vec::new();
        for &v in order {
            let pr = self.rand();
            self.t[v as usize] = TNode { prio: pr, ..TNIL };
            let mut last = NIL;
            while let Some(&top) = stack.last() {
                if self.t[top as usize].prio < pr {
                    last = top;
                    stack.pop();
                } else {
                    break;
                }
            }
            if last != NIL {
                self.t[v as usize].l = last;
                self.t[last as usize].p = v;
            }
            if let Some(&top) = stack.last() {
                self.t[top as usize].r = v;
                self.t[v as usize].p = top;
            }
            stack.push(v);
        }
        let Some(&root) = stack.first() else {
            return NIL;
        };
        // post-order pull
        let mut post = Vec::with_capacity(order.len());
        let mut st = vec![root];
        while let Some(x) = st.pop() {
            post.push(x);
            let n = self.t[x as usize];
            if n.l != NIL {
                st.push(n.l);
            }
            if n.r != NIL {
                st.push(n.r);
            }
        }
        for &x in post.iter().rev() {
            self.pull(m, x);
        }
        self.t[root as usize].p = NIL;
        root
    }
    fn split(&mut self, m: &Mirror, t: u32, k: u32) -> (u32, u32) {
        if t == NIL {
            return (NIL, NIL);
        }
        let l = self.t[t as usize].l;
        let ls = self.size(l);
        if k <= ls {
            let (a, b) = self.split(m, l, k);
            self.t[t as usize].l = b;
            if b != NIL {
                self.t[b as usize].p = t;
            }
            self.pull(m, t);
            if a != NIL {
                self.t[a as usize].p = NIL;
            }
            self.t[t as usize].p = NIL;
            (a, t)
        } else {
            let r = self.t[t as usize].r;
            let (a, b) = self.split(m, r, k - ls - 1);
            self.t[t as usize].r = a;
            if a != NIL {
                self.t[a as usize].p = t;
            }
            self.pull(m, t);
            if b != NIL {
                self.t[b as usize].p = NIL;
            }
            self.t[t as usize].p = NIL;
            (t, b)
        }
    }
    fn merge(&mut self, m: &Mirror, a: u32, b: u32) -> u32 {
        if a == NIL {
            return b;
        }
        if b == NIL {
            return a;
        }
        if self.t[a as usize].prio > self.t[b as usize].prio {
            let r = self.merge(m, self.t[a as usize].r, b);
            self.t[a as usize].r = r;
            self.t[r as usize].p = a;
            self.pull(m, a);
            self.t[a as usize].p = NIL;
            a
        } else {
            let l = self.merge(m, a, self.t[b as usize].l);
            self.t[b as usize].l = l;
            self.t[l as usize].p = b;
            self.pull(m, b);
            self.t[b as usize].p = NIL;
            b
        }
    }
    fn rank(&self, mut v: u32) -> u32 {
        let mut r = self.size(self.t[v as usize].l);
        loop {
            let p = self.t[v as usize].p;
            if p == NIL {
                return r;
            }
            if self.t[p as usize].r == v {
                r += self.size(self.t[p as usize].l) + 1;
            }
            v = p;
        }
    }
    fn first_in(&self, m: &Mirror, mut t: u32, bit: u64) -> Option<u32> {
        while t != NIL {
            let n = &self.t[t as usize];
            if n.l != NIL && self.t[n.l as usize].agg & bit != 0 {
                t = n.l;
            } else if m.mask[t as usize] & bit != 0 {
                return Some(t);
            } else {
                t = n.r;
            }
        }
        None
    }
    fn last_in(&self, m: &Mirror, mut t: u32, bit: u64) -> Option<u32> {
        while t != NIL {
            let n = &self.t[t as usize];
            if n.r != NIL && self.t[n.r as usize].agg & bit != 0 {
                t = n.r;
            } else if m.mask[t as usize] & bit != 0 {
                return Some(t);
            } else {
                t = n.l;
            }
        }
        None
    }
}

impl NavIndex for TreapIndex {
    fn name() -> &'static str {
        "treap-aug"
    }
    fn build(m: &Mirror, order: &[u32]) -> Self {
        let mut s = TreapIndex {
            t: vec![TNIL; m.parent.len()],
            root: NIL,
            rng: 0x9E37_79B9_7F4A_7C15,
        };
        s.root = s.build_seq(m, order);
        s
    }
    fn on_insert(&mut self, m: &Mirror, pred: u32, order: &[u32]) {
        let pos = self.rank(pred) + 1;
        let sub = self.build_seq(m, order);
        let (a, b) = self.split(m, self.root, pos);
        let ab = self.merge(m, a, sub);
        self.root = self.merge(m, ab, b);
    }
    fn on_delete(&mut self, m: &Mirror, order: &[u32]) {
        let pos = self.rank(order[0]);
        let (a, rest) = self.split(m, self.root, pos);
        let (_, b) = self.split(m, rest, order.len() as u32);
        self.root = self.merge(m, a, b);
    }
    fn on_mask(&mut self, m: &Mirror, mut v: u32, _old: u64, _new: u64) {
        while v != NIL {
            self.pull(m, v);
            v = self.t[v as usize].p;
        }
    }
    fn next(&self, m: &Mirror, c: usize, mut v: u32) -> Option<u32> {
        let bit = 1u64 << c;
        let r = self.t[v as usize].r;
        if r != NIL && self.t[r as usize].agg & bit != 0 {
            return self.first_in(m, r, bit);
        }
        loop {
            let p = self.t[v as usize].p;
            if p == NIL {
                return None;
            }
            if self.t[p as usize].l == v {
                if m.mask[p as usize] & bit != 0 {
                    return Some(p);
                }
                let rp = self.t[p as usize].r;
                if rp != NIL && self.t[rp as usize].agg & bit != 0 {
                    return self.first_in(m, rp, bit);
                }
            }
            v = p;
        }
    }
    fn prev(&self, m: &Mirror, c: usize, mut v: u32) -> Option<u32> {
        let bit = 1u64 << c;
        let l = self.t[v as usize].l;
        if l != NIL && self.t[l as usize].agg & bit != 0 {
            return self.last_in(m, l, bit);
        }
        loop {
            let p = self.t[v as usize].p;
            if p == NIL {
                return None;
            }
            if self.t[p as usize].r == v {
                if m.mask[p as usize] & bit != 0 {
                    return Some(p);
                }
                let lp = self.t[p as usize].l;
                if lp != NIL && self.t[lp as usize].agg & bit != 0 {
                    return self.last_in(m, lp, bit);
                }
            }
            v = p;
        }
    }
    fn kth(&self, m: &Mirror, c: usize, mut k: usize) -> Option<u32> {
        let mut t = self.root;
        while t != NIL {
            let n = &self.t[t as usize];
            let cl = if n.l != NIL {
                self.t[n.l as usize].cnt[c] as usize
            } else {
                0
            };
            if k < cl {
                t = n.l;
                continue;
            }
            k -= cl;
            if (m.mask[t as usize] >> c) & 1 == 1 {
                if k == 0 {
                    return Some(t);
                }
                k -= 1;
            }
            t = n.r;
        }
        None
    }
}

// ---------------------------------------------------------------------------
enum Op {
    Ins {
        p: u32,
        idx: usize,
        nodes: Vec<(u32, i32, u64)>,
    },
    Del(u32),
    Mask(u32, u64),
    Q {
        kind: u8,
        c: usize,
        arg: u32,
        want: i64,
    },
}

fn parse(path: &str) -> (Mirror, Vec<Op>) {
    let s = std::fs::read_to_string(path).expect("read trace");
    let mut lines = s.lines();
    let head: Vec<&str> = lines.next().expect("header").split_whitespace().collect();
    let n: usize = head[1].parse().expect("n");
    let mut m = Mirror {
        parent: Vec::with_capacity(n),
        children: vec![Vec::new(); n],
        mask: Vec::with_capacity(n),
    };
    for v in 0..n {
        let mut it = lines.next().expect("node").split_whitespace();
        let p: i64 = it.next().expect("p").parse().expect("p");
        let mk: u64 = it.next().expect("m").parse().expect("m");
        m.parent.push(if p < 0 { NIL } else { p as u32 });
        m.mask.push(mk);
        if p >= 0 {
            m.children[p as usize].push(v as u32);
        }
    }
    let mut ops = Vec::new();
    for line in lines {
        let f: Vec<&str> = line.split_whitespace().collect();
        let num = |i: usize| -> i64 { f[i].parse().expect("num") };
        match f[0] {
            "I" => {
                let k = num(3) as usize;
                let nodes = (0..k)
                    .map(|j| {
                        (
                            num(4 + 3 * j) as u32,
                            num(5 + 3 * j) as i32,
                            f[6 + 3 * j].parse::<u64>().expect("m"),
                        )
                    })
                    .collect();
                ops.push(Op::Ins {
                    p: num(1) as u32,
                    idx: num(2) as usize,
                    nodes,
                });
            }
            "D" => ops.push(Op::Del(num(1) as u32)),
            "M" => ops.push(Op::Mask(num(1) as u32, f[2].parse().expect("m"))),
            "Q" => ops.push(Op::Q {
                kind: f[1].as_bytes()[0],
                c: num(2) as usize,
                arg: num(3) as u32,
                want: num(4),
            }),
            _ => panic!("bad op {}", f[0]),
        }
    }
    (m, ops)
}

fn pct(v: &mut [f64], q: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).expect("nan"));
    v[((v.len() as f64 - 1.0) * q) as usize]
}

fn replay<I: NavIndex>(m0: &Mirror, ops: &[Op]) {
    let mut m = Mirror {
        parent: m0.parent.clone(),
        children: m0.children.clone(),
        mask: m0.mask.clone(),
    };
    let mut order = Vec::new();
    m.subtree_preorder(0, &mut order);
    let t0 = Instant::now();
    let mut ix = I::build(&m, &order);
    let build_ms = t0.elapsed().as_secs_f64() * 1e3;
    let (mut qn, mut qk, mut ins, mut del, mut msk) = (vec![], vec![], vec![], vec![], vec![]);
    let mut ins_nodes = 0usize;
    let mut del_nodes = 0usize;
    let mut bad = 0usize;
    let mut buf = Vec::new();
    for op in ops {
        match op {
            Op::Ins { p, idx, nodes } => {
                for &(v, _, _) in nodes {
                    m.ensure(v);
                }
                let pred = if *idx == 0 {
                    *p
                } else {
                    m.last_descendant(m.children[*p as usize][idx - 1])
                };
                buf.clear();
                for &(v, lp, mk) in nodes {
                    let par = if lp < 0 { *p } else { nodes[lp as usize].0 };
                    m.parent[v as usize] = par;
                    m.mask[v as usize] = mk;
                    m.children[v as usize].clear();
                    if lp >= 0 {
                        m.children[par as usize].push(v);
                    }
                    buf.push(v);
                }
                m.children[*p as usize].insert(*idx, nodes[0].0);
                let t = Instant::now();
                ix.on_insert(&m, pred, &buf);
                ins.push(t.elapsed().as_secs_f64() * 1e6);
                ins_nodes += nodes.len();
            }
            Op::Del(v) => {
                m.subtree_preorder(*v, &mut buf);
                let t = Instant::now();
                ix.on_delete(&m, &buf);
                del.push(t.elapsed().as_secs_f64() * 1e6);
                del_nodes += buf.len();
                let p = m.parent[*v as usize];
                m.children[p as usize].retain(|&x| x != *v);
                for &x in &buf {
                    m.children[x as usize].clear();
                }
            }
            Op::Mask(v, mk) => {
                let old = m.mask[*v as usize];
                m.mask[*v as usize] = *mk;
                let t = Instant::now();
                ix.on_mask(&m, *v, old, *mk);
                msk.push(t.elapsed().as_secs_f64() * 1e6);
            }
            Op::Q { kind, c, arg, want } => {
                let t = Instant::now();
                let got = match kind {
                    b'n' => ix.next(&m, *c, *arg),
                    b'p' => ix.prev(&m, *c, *arg),
                    _ => ix.kth(&m, *c, *arg as usize),
                };
                let dt = t.elapsed().as_secs_f64() * 1e6;
                if *kind == b'k' {
                    qk.push(dt)
                } else {
                    qn.push(dt)
                }
                if got.map_or(-1, |x| x as i64) != *want {
                    bad += 1;
                }
            }
        }
    }
    let sum = |v: &[f64]| v.iter().sum::<f64>();
    let (si, sd, sm) = (sum(&ins), sum(&del), sum(&msk));
    println!(
        "{:10} build={:7.2}ms | next/prev p50={:.3}us p99={:.3}us | kth p50={:.3}us p99={:.3}us | \
         insert p50={:.1}us p99={:.1}us ({:.3}us/node) | delete p50={:.1}us p99={:.1}us ({:.3}us/node) | \
         mask p50={:.3}us | mismatches={}",
        I::name(),
        build_ms,
        pct(&mut qn, 0.5),
        pct(&mut qn, 0.99),
        pct(&mut qk, 0.5),
        pct(&mut qk, 0.99),
        pct(&mut ins, 0.5),
        pct(&mut ins, 0.99),
        si / ins_nodes.max(1) as f64,
        pct(&mut del, 0.5),
        pct(&mut del, 0.99),
        sd / del_nodes.max(1) as f64,
        pct(&mut msk, 0.5),
        bad
    );
    let _ = sm;
}

/// Adversarial OM workload: n single-node inserts at the same position.
fn adversarial(n: usize) {
    let base = 1000usize;
    let mut m = Mirror {
        parent: vec![NIL],
        children: vec![Vec::new()],
        mask: vec![0],
    };
    for v in 1..base {
        m.ensure(v as u32);
        m.parent[v] = 0;
        m.mask[v] = 1;
        m.children[0].push(v as u32);
    }
    let mut order = Vec::new();
    m.subtree_preorder(0, &mut order);
    let mut om = OmIndex::build(&m, &order);
    let mut tr = TreapIndex::build(&m, &order);
    let t0 = Instant::now();
    let mut times = Vec::with_capacity(n);
    for i in 0..n {
        let v = (base + i) as u32;
        m.ensure(v);
        m.parent[v as usize] = 0;
        m.mask[v as usize] = 1;
        m.children[0].insert(1, v);
        let t = Instant::now();
        om.on_insert(&m, 1, &[v]);
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let tot = t0.elapsed().as_secs_f64();
    // treap on same sequence
    let mut m2 = Mirror {
        parent: m.parent.clone(),
        children: vec![Vec::new(); m.parent.len()],
        mask: m.mask.clone(),
    };
    m2.children[0] = (1..base as u32).collect();
    let t1 = Instant::now();
    for i in 0..n {
        let v = (base + i) as u32;
        m2.children[0].insert(1, v);
        tr.on_insert(&m2, 1, &[v]);
    }
    let tot2 = t1.elapsed().as_secs_f64();
    // verify: order of category 0 equals children order
    let want: Vec<u32> = m.children[0].clone();
    let got: Vec<u32> = om.cat[0].values().copied().collect();
    assert_eq!(want, got, "OM order mismatch");
    let got2: Vec<u32> = (0..want.len())
        .map(|k| tr.kth(&m2, 0, k).expect("kth"))
        .collect();
    assert_eq!(want, got2, "treap order mismatch");
    println!(
        "adversarial same-point inserts n={n}: OM total {:.1} ms, mean {:.2} us, p99 {:.2} us, max {:.0} us, \
         relabelled/insert {:.1} | treap total {:.1} ms",
        tot * 1e3,
        tot * 1e6 / n as f64,
        pct(&mut times, 0.99),
        pct(&mut times, 1.0),
        om.relabels as f64 / n as f64,
        tot2 * 1e3
    );
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--adversarial") {
        let n = args.get(2).map_or(100_000, |s| s.parse().expect("n"));
        adversarial(n);
        return;
    }
    for path in &args[1..] {
        let (m, ops) = parse(path);
        let nq = ops.iter().filter(|o| matches!(o, Op::Q { .. })).count();
        println!(
            "== {path}: n0={} ops={} queries={nq}",
            m.parent.len(),
            ops.len()
        );
        replay::<OmIndex>(&m, &ops);
        replay::<TreapIndex>(&m, &ops);
    }
}
