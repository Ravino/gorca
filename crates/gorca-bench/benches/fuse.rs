// SPDX-License-Identifier: GPL-3.0-or-later
//! M22a-B6: the B-FUSE bench (ADR-0022 M-11, ADR-0024 section 2.12, spec section 6).
//!
//! Drives `gorca_fuse::Ivm` (the incremental writer engine, B2) with a 100k-node mirror, 100
//! patches and a fake secondary writing facts at 5k/s, and reports p50/p95/p99 (never a mean
//! alone) with the host load and governor. Scenarios (select with `M22A_ONLY=a,b`, default all):
//!
//! * `mem`      100k-node build, RSS increase and carrier sizes (spec gate: <= 25 MB, 128 B entry);
//! * `storm`    mixed events at 10k/25k/50k per second (real-time paced), per-event and per-1-ms
//!   batch cost of `apply + run + publish`, backlog, lag (gate M-11: p99 <= 2 us/event);
//! * `focus`    a focus change with ~100k dirty nodes (gate: synchronous part <= 0.5 ms);
//! * `global`   `bump_global` synchronous part, sweep drain, priority-event latency during it;
//! * `aging`    background aging under a priority storm (no starvation);
//! * `anchors`  per-key anchor re-resolution cost (S-3), flat in the number of cached anchors;
//! * `churn`    lifecycle churn (add / remove 200-node subtrees), leak check by RSS.
//!
//! Run: `M22A_STAMP=<id> M22A_CPU=<core> cargo bench -p gorca-bench --bench fuse`.
//! Results: `bench/m22a/results/<stamp>-fuse.json`. Single-threaded by design: the writer is one
//! actor; the fake secondary is interleaved on the same thread, so its cost is *included*.
#![allow(
    unsafe_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::too_many_lines,
    clippy::similar_names
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::mem::size_of;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_fuse::{
    AnchorId, Budget, FactIn, Grant, Input, Ivm, Neighbours, NoQuirks, NodeIdx, NodeRegs, PatchHit,
    PatchSource, Payload, PrimarySlot, Profile, Rules, S0Node, SelectorReads, SourceInfo,
    SourceState, Val, Write,
};
use gorca_model::fact::{Basis, SourceId};
use gorca_model::sem::{
    Avail, Check, Expand, Group, Orientation, SemEntry, SemRole, SourceClass, TrustTier,
};

// ---- small utilities ------------------------------------------------------------------------

/// Share of events that hit the 2000-node hot set (default 70; `M22A_HOT_PCT=100` shows the
/// cache-warm floor, `0` the fully cold case).
fn hot_pct() -> usize {
    static P: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *P.get_or_init(|| {
        std::env::var("M22A_HOT_PCT")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(70)
    })
}

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pct(&mut self, p: usize) -> bool {
        self.below(100) < p
    }
}

fn nid(x: usize) -> NodeIdx {
    NodeIdx(u32::try_from(x).unwrap())
}

fn read_first(path: &str) -> String {
    std::fs::read_to_string(path).map_or_else(|_| "?".into(), |s| s.trim().to_owned())
}

fn host_label() -> String {
    format!(
        "load={} governor={} cpus={} cpu_pin={} mem_pressure_some={}",
        read_first("/proc/loadavg"),
        read_first("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        std::thread::available_parallelism().map_or(0, usize::from),
        std::env::var("M22A_CPU").unwrap_or_else(|_| "none".into()),
        read_first("/proc/pressure/memory")
            .lines()
            .next()
            .unwrap_or("?")
    )
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s
        .split_whitespace()
        .nth(1)
        .and_then(|x| x.parse().ok())
        .unwrap_or(0);
    pages * 4
}

fn trim_rss_kb() -> u64 {
    // SAFETY: malloc_trim has no preconditions.
    unsafe {
        libc::malloc_trim(0);
    }
    rss_kb()
}

fn pin_cpu() {
    if let Ok(c) = std::env::var("M22A_CPU")
        && let Ok(c) = c.parse::<usize>()
    {
        // SAFETY: plain sched_setaffinity on the calling thread with a zeroed, then set, cpu_set_t.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            libc::CPU_SET(c, &mut set);
            libc::sched_setaffinity(0, size_of::<libc::cpu_set_t>(), &raw const set);
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Pct {
    n: usize,
    p50: f64,
    p95: f64,
    p99: f64,
    p999: f64,
    max: f64,
    mean: f64,
}

fn pct(mut v: Vec<f64>) -> Pct {
    if v.is_empty() {
        return Pct::default();
    }
    v.sort_by(f64::total_cmp);
    let q = |p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
    Pct {
        n: v.len(),
        p50: q(0.5),
        p95: q(0.95),
        p99: q(0.99),
        p999: q(0.999),
        max: v[v.len() - 1],
        mean: v.iter().sum::<f64>() / v.len() as f64,
    }
}

impl Pct {
    fn show(&self, unit: &str) -> String {
        format!(
            "n={} p50 {:.2} p95 {:.2} p99 {:.2} p99.9 {:.2} max {:.2} mean {:.2} {unit}",
            self.n, self.p50, self.p95, self.p99, self.p999, self.max, self.mean
        )
    }
    fn json(&self) -> String {
        format!(
            "{{\"n\":{},\"p50\":{:.3},\"p95\":{:.3},\"p99\":{:.3},\"p999\":{:.3},\"max\":{:.3},\"mean\":{:.3}}}",
            self.n, self.p50, self.p95, self.p99, self.p999, self.max, self.mean
        )
    }
}

/// Collects `"key": raw_json` pairs and a gate table.
#[derive(Default)]
struct Report {
    kv: Vec<(String, String)>,
    gates: Vec<(String, String, bool)>,
}

impl Report {
    fn put(&mut self, k: &str, raw: String) {
        self.kv.push((k.to_owned(), raw));
    }
    fn gate(&mut self, name: &str, measured: String, pass: bool) {
        eprintln!(
            "GATE {name}: {measured} => {}",
            if pass { "PASS" } else { "FAIL" }
        );
        self.gates.push((name.to_owned(), measured, pass));
    }
    fn json(&self, host: &str) -> String {
        let mut s = format!("{{\n  \"host\": \"{host}\",\n");
        for (k, v) in &self.kv {
            let _ = writeln!(s, "  \"{k}\": {v},");
        }
        s.push_str("  \"gates\": [");
        for (i, (n, m, p)) in self.gates.iter().enumerate() {
            let _ = write!(
                s,
                "{}\n    {{\"gate\":\"{n}\",\"measured\":\"{m}\",\"pass\":{p}}}",
                if i == 0 { "" } else { "," }
            );
        }
        s.push_str("\n  ]\n}\n");
        s
    }
}

// ---- patches: 100 rules, indexed by role --------------------------------------------------------

struct Rule {
    parent_role: Option<SemRole>,
    only_unnamed: bool,
    write: Write,
    spec: u32,
    ext: Arc<str>,
    id: Arc<str>,
}

struct Patches {
    by_role: HashMap<u16, Vec<Rule>>,
    wide_reads: bool,
}

impl Patches {
    fn new(n: usize, wide_reads: bool) -> Patches {
        let roles = ROLES;
        let mut by_role: HashMap<u16, Vec<Rule>> = HashMap::new();
        let mut r = Rng::new(7);
        for i in 0..n {
            let role = roles[i % roles.len()];
            let (group_write, spec) = match i % 3 {
                0 => (Write::Val(Val::Level(u8::try_from(i % 5 + 1).unwrap())), 1),
                1 => (Write::Val(Val::Orientation(Orientation::Vertical)), 2),
                _ => (Write::Val(Val::Avail(Avail::Enabled)), 1),
            };
            by_role.entry(role.code()).or_default().push(Rule {
                parent_role: r.pct(30).then(|| roles[r.below(roles.len())]),
                only_unnamed: r.pct(30),
                write: group_write,
                spec,
                ext: Arc::from(format!("org.bench.ext{}", i % 4).as_str()),
                id: Arc::from(format!("p{i}").as_str()),
            });
        }
        Patches {
            by_role,
            wide_reads,
        }
    }
}

impl PatchSource for Patches {
    fn hits(&self, node: &S0Node, nb: &Neighbours<'_>, out: &mut Vec<PatchHit>) {
        let Some(role) = node.role() else { return };
        let Some(rules) = self.by_role.get(&role.code()) else {
            return;
        };
        for r in rules {
            if let Some(pr) = r.parent_role
                && nb.parent.and_then(|p| p.role()) != Some(pr)
            {
                continue;
            }
            if r.only_unnamed
                && matches!(&node.get(Group::Name).slot, gorca_fuse::Slot::Val(Val::Name(n)) if !n.is_empty())
            {
                continue;
            }
            out.push(PatchHit {
                group: r.write.group(),
                write: Some(r.write.clone()),
                shown: r.write.slot(),
                spec: r.spec,
                ext: Arc::clone(&r.ext),
                rule: Arc::clone(&r.id),
            });
        }
    }

    fn writes(&self) -> gorca_model::sem::GroupMask {
        // The rules above only write level, orientation and availability.
        gorca_model::sem::GroupMask::EMPTY
            .with(Group::Level)
            .with(Group::Orientation)
            .with(Group::Avail)
    }

    fn reads(&self) -> SelectorReads {
        if self.wide_reads {
            SelectorReads::ALL
        } else {
            SelectorReads {
                parent: gorca_model::sem::GroupMask::EMPTY.with(Group::Role),
                children: gorca_model::sem::GroupMask::EMPTY,
            }
        }
    }
}

// ---- the world -----------------------------------------------------------------------------------

const ROLES: [SemRole; 8] = [
    SemRole::Generic,
    SemRole::Button,
    SemRole::Checkbox,
    SemRole::Link,
    SemRole::Heading,
    SemRole::ListItem,
    SemRole::Img,
    SemRole::Group,
];

type Engine = Ivm<NoQuirks, Patches>;

fn sid(slot: u32) -> SourceId {
    SourceId {
        slot,
        generation: 0,
    }
}

const BUS: u32 = 2;
const OCR: u32 = 3;
const FANOUT: usize = 5;

struct World {
    e: Engine,
    n: usize,
    seq: u64,
    clock: u64,
    rng: Rng,
    names: Vec<Arc<str>>,
    hot: Vec<usize>,
    focus_leaf: usize,
    /// Node indices that carry an HTML id (`d{i}`), for Dom anchors.
    dom_nodes: Vec<usize>,
    /// Next `vis_rev` to send per node (kept here so inputs are monotonic).
    vis: Vec<u32>,
}

fn parent_of(i: usize) -> Option<usize> {
    (i > 0).then(|| (i - 1) / FANOUT)
}

fn engine(wide: bool, patches: usize) -> Engine {
    let mut profile = Profile::new(&[PrimarySlot::P0, PrimarySlot::P1], Rules::aria());
    profile.default_role = SemRole::Generic;
    profile.inexpressible = gorca_model::sem::GroupMask::EMPTY
        .with(Group::Description)
        .with(Group::Live)
        .with(Group::Orientation);
    let mut e = Ivm::new(profile, NoQuirks, Patches::new(patches, wide));
    for (slot, name, class, tier, ko) in [
        (
            BUS,
            "bus",
            SourceClass::Secondary,
            TrustTier::T3Untrusted,
            0,
        ),
        (
            OCR,
            "ocr",
            SourceClass::Inferred,
            TrustTier::T2FirstParty,
            0,
        ),
    ] {
        e.set_source(SourceInfo {
            id: sid(slot),
            name: Arc::from(name),
            class,
            tier,
            kind_order: ko,
            grant: Grant::standard(),
        })
        .unwrap();
        e.set_source_state(sid(slot), 1, SourceState::Active);
    }
    e
}

/// The realistic world (default): a node's state read writes about 14 groups, most of them
/// `Absent` (a real AT-SPI node reports every state group, not three or four). `M22A_WORLD=lean`
/// restores the original 3-4 groups per node (the B6 world).
fn rich_world() -> bool {
    static P: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *P.get_or_init(|| !std::env::var("M22A_WORLD").is_ok_and(|s| s == "lean"))
}

fn init_inputs(i: usize, seq: u64, names: &[Arc<str>], rng: &mut Rng, out: &mut Vec<Input>) {
    init_inputs_in(rich_world(), i, seq, names, rng, out);
}

fn init_inputs_in(
    rich: bool,
    i: usize,
    seq: u64,
    names: &[Arc<str>],
    rng: &mut Rng,
    out: &mut Vec<Input>,
) {
    if rich {
        init_rich(i, seq, names, rng, out);
    } else {
        init_lean(i, seq, names, rng, out);
    }
}

fn init_rich(i: usize, seq: u64, names: &[Arc<str>], rng: &mut Rng, out: &mut Vec<Input>) {
    let n = nid(i);
    let role = ROLES[i % ROLES.len()];
    out.push(Input::Life {
        node: n,
        src: PrimarySlot::P0,
        seq,
        alive: true,
    });
    let mut w = |write| {
        out.push(Input::Obs {
            node: n,
            src: PrimarySlot::P0,
            seq: seq + 1,
            write,
        });
    };
    w(Write::Val(Val::Role(role)));
    if rng.pct(80) {
        w(Write::Val(Val::Name(Arc::clone(&names[i % names.len()]))));
    } else {
        w(Write::Absent(Group::Name));
    }
    w(Write::Val(Val::Avail(Avail::Enabled)));
    // The states that apply to the role carry a value; the rest are reported `Absent`.
    match role {
        SemRole::Checkbox => w(Write::Val(Val::Check(Check::Off))),
        _ => w(Write::Absent(Group::Check)),
    }
    match role {
        SemRole::Heading => w(Write::Val(Val::Level(u8::try_from(i % 6 + 1).unwrap()))),
        _ => w(Write::Absent(Group::Level)),
    }
    match role {
        SemRole::Button => w(Write::Val(Val::Expand(Expand::Collapsed))),
        _ => w(Write::Absent(Group::Expand)),
    }
    for g in [
        Group::Description,
        Group::Press,
        Group::Select,
        Group::Current,
        Group::Invalid,
        Group::Popup,
        Group::Flags,
        Group::Orientation,
        Group::Live,
    ] {
        w(Write::Absent(g));
    }
    // About 5 % of nodes report every group.
    if rng.pct(5) {
        for g in [Group::Sort, Group::Grid, Group::SetPos] {
            w(Write::Absent(g));
        }
    }
}

fn init_lean(i: usize, seq: u64, names: &[Arc<str>], rng: &mut Rng, out: &mut Vec<Input>) {
    let n = nid(i);
    let role = ROLES[i % ROLES.len()];
    out.push(Input::Life {
        node: n,
        src: PrimarySlot::P0,
        seq,
        alive: true,
    });
    let w = |write| Input::Obs {
        node: n,
        src: PrimarySlot::P0,
        seq: seq + 1,
        write,
    };
    out.push(w(Write::Val(Val::Role(role))));
    // 20 % of nodes have no name: ocr facts and name-from-content have something to fill.
    if rng.pct(80) {
        out.push(w(Write::Val(Val::Name(Arc::clone(
            &names[i % names.len()],
        )))));
    } else {
        out.push(w(Write::Absent(Group::Name)));
    }
    out.push(w(Write::Val(Val::Avail(Avail::Enabled))));
    match role {
        SemRole::Checkbox => out.push(w(Write::Val(Val::Check(Check::Off)))),
        SemRole::Heading => out.push(w(Write::Val(Val::Level(u8::try_from(i % 6 + 1).unwrap())))),
        SemRole::Button => out.push(w(Write::Val(Val::Expand(Expand::Collapsed)))),
        _ => {}
    }
}

impl World {
    /// Build a populated engine with `n` nodes (parents first) and fuse everything.
    fn build(n: usize, patches: usize, wide: bool) -> World {
        World::build_in(rich_world(), n, patches, wide)
    }

    fn build_in(rich: bool, n: usize, patches: usize, wide: bool) -> World {
        let names: Vec<Arc<str>> = (0..256)
            .map(|i| Arc::from(format!("Label number {i}").as_str()))
            .collect();
        let mut rng = Rng::new(42);
        let mut e = engine(wide, patches);
        let mut buf = Vec::with_capacity(8192);
        let mut dom_nodes = Vec::new();
        for i in 0..n {
            e.add_node(nid(i), parent_of(i).map(nid)).unwrap();
            init_inputs_in(rich, i, 10, &names, &mut rng, &mut buf);
            if i % 5 == 0 {
                buf.push(Input::Attr {
                    node: nid(i),
                    seq: 12,
                    id: Some(Arc::from(format!("d{i}").as_str())),
                });
                dom_nodes.push(i);
            }
            if buf.len() >= 8000 {
                e.apply(std::mem::take(&mut buf));
                e.run(Budget::UNLIMITED);
            }
        }
        e.apply(buf);
        e.run(Budget::UNLIMITED);
        let mut hot = Vec::new();
        for _ in 0..2000 {
            hot.push(rng.below(n));
        }
        World {
            e,
            n,
            seq: 1000,
            clock: 0,
            rng,
            names,
            hot,
            focus_leaf: 0,
            dom_nodes,
            vis: vec![0; n],
        }
    }

    fn pick(&mut self) -> usize {
        if self.rng.pct(hot_pct()) {
            self.hot[self.rng.below(self.hot.len())]
        } else {
            self.rng.below(self.n)
        }
    }

    fn obs(&mut self, node: usize, write: Write) -> Input {
        self.seq += 1;
        Input::Obs {
            node: nid(node),
            src: PrimarySlot::P0,
            seq: self.seq,
            write,
        }
    }

    /// One mixed primary event (the storm mix).
    fn primary_event(&mut self) -> Input {
        let node = self.pick();
        let r = self.rng.below(100);
        let role = ROLES[node % ROLES.len()];
        match r {
            0..=44 => {
                let w = match role {
                    SemRole::Checkbox => Write::Val(Val::Check(if self.rng.pct(50) {
                        Check::On
                    } else {
                        Check::Off
                    })),
                    SemRole::Button => Write::Val(Val::Expand(if self.rng.pct(50) {
                        Expand::Expanded
                    } else {
                        Expand::Collapsed
                    })),
                    _ => Write::Val(Val::Avail(if self.rng.pct(50) {
                        Avail::Enabled
                    } else {
                        Avail::Disabled
                    })),
                };
                self.obs(node, w)
            }
            45..=64 => {
                let nm = Arc::clone(&self.names[self.rng.below(self.names.len())]);
                self.obs(node, Write::Val(Val::Name(nm)))
            }
            65..=74 => {
                // A no-op rewrite: the same value again with a newer sequence (free dedupe).
                self.obs(node, Write::Val(Val::Avail(Avail::Enabled)))
            }
            75..=89 => {
                self.vis[node] += 1;
                Input::Vis {
                    node: nid(node),
                    rev: self.vis[node],
                }
            }
            _ => {
                let o = if self.rng.pct(50) {
                    Orientation::Horizontal
                } else {
                    Orientation::Vertical
                };
                self.obs(node, Write::Val(Val::Orientation(o)))
            }
        }
    }

    /// One fact of the fake secondary (a `bus` description or an `ocr` name), stamped from the
    /// engine's own state the way a writer stamps it.
    fn fact(&mut self) -> Input {
        self.seq += 1;
        let use_dom = self.rng.pct(20) && !self.dom_nodes.is_empty();
        let (node, anchor) = if use_dom {
            let i = self.dom_nodes[self.rng.below(self.dom_nodes.len())];
            (
                i,
                AnchorId::Dom {
                    id: Arc::from(format!("d{i}").as_str()),
                    count: 1,
                    frame: 0,
                    shadow: 0,
                },
            )
        } else {
            let i = self.pick();
            (i, AnchorId::Node(nid(i)))
        };
        let res = self.e.resolve_anchor(&anchor);
        let (src, group, payload, ttl) = if self.rng.pct(60) {
            let t = Arc::clone(&self.names[self.rng.below(self.names.len())]);
            (
                BUS,
                Group::Description,
                Payload::Sem(Write::Val(Val::Description(t))),
                None,
            )
        } else {
            let t = Arc::clone(&self.names[self.rng.below(self.names.len())]);
            (
                OCR,
                Group::Name,
                Payload::Sem(Write::Val(Val::Name(t))),
                Some(60_000),
            )
        };
        Input::Fact(Box::new(FactIn {
            source: sid(src),
            anchor,
            group: group.group_code(),
            seq: self.seq,
            payload,
            p_milli: 990,
            basis: Basis::VisRev(self.e.vis_rev(nid(node)) + 1),
            bind_gen: res.generation,
            admitted_at_ms: self.e.clock(),
            ttl_ms: ttl,
        }))
    }

    #[allow(clippy::unused_self)]
    fn chain_of(&self, mut x: usize) -> Vec<NodeIdx> {
        let mut c = vec![nid(x)];
        while let Some(p) = parent_of(x) {
            c.push(nid(p));
            x = p;
        }
        c
    }

    fn set_focus(&mut self) -> (usize, usize) {
        let leaf = self.pick();
        self.focus_leaf = leaf;
        let chain = self.chain_of(leaf);
        let vis: Vec<NodeIdx> = (0..200).map(|k| nid((leaf + k) % self.n)).collect();
        self.e.set_focus_chain(&chain);
        self.e.set_visible(&vis);
        (chain.len(), vis.len())
    }
}

#[inline]
fn spin_until(t: Instant) {
    while Instant::now() < t {
        std::hint::spin_loop();
    }
}

fn us(d: Duration) -> f64 {
    d.as_secs_f64() * 1e6
}

// ---- scenario: memory -----------------------------------------------------------------------------

fn scenario_mem(rep: &mut Report) {
    eprintln!("\n== mem: 100k-node build, lean (B6 world) and realistic (14 groups/node) ==");
    let mut json = String::new();
    let mut rich_mb = 0.0f64;
    let mut rich_entries = 0.0f64;
    for (label, rich) in [("lean", false), ("realistic", true)] {
        let base = trim_rss_kb();
        let t = Instant::now();
        let w = World::build_in(rich, 100_000, 100, false);
        let build = t.elapsed();
        let after = trim_rss_kb();
        let delta_mb = (after.saturating_sub(base)) as f64 / 1024.0;
        // Entries alone: fresh copies, to separate the 128 B entries from the rest of the engine.
        let before_copy = trim_rss_kb();
        let copies: Vec<Arc<SemEntry>> = (0..w.n)
            .filter_map(|i| w.e.entry_unmasked(nid(i)))
            .map(|a| Arc::new(SemEntry::clone(a)))
            .collect();
        let entries_mb = trim_rss_kb().saturating_sub(before_copy) as f64 / 1024.0;
        black_box(&copies);
        drop(copies);
        let fused = (0..w.n)
            .filter(|i| w.e.entry_unmasked(nid(*i)).is_some())
            .count();
        let st = w.e.stats();
        eprintln!(
            "{label:>9}: build {:.0} ms ({:.2} us/node), fused {fused}, RSS +{delta_mb:.1} MB engine, entries alone +{entries_mb:.1} MB ({:.0} B/node)",
            build.as_secs_f64() * 1e3,
            us(build) / w.n as f64,
            delta_mb * 1024.0 * 1024.0 / w.n as f64
        );
        eprintln!(
            "           stats: node record {} B, resident S0 {} nodes / {} B, S0 computes {}, neighbour hits {}, fusions {}",
            st.node_record_bytes,
            st.s0_resident_nodes,
            st.s0_resident_bytes,
            st.s0_computes,
            st.s0_neighbour_hits,
            st.fusions
        );
        let _ = write!(
            json,
            "{}\"{label}\":{{\"nodes\":{},\"build_ms\":{:.1},\"rss_increase_mb_engine\":{:.2},\"rss_increase_mb_entries_only\":{:.2},\"node_record_b\":{},\"s0_resident_bytes\":{},\"fused\":{}}}",
            if json.is_empty() { "" } else { "," },
            w.n,
            build.as_secs_f64() * 1e3,
            delta_mb,
            entries_mb,
            st.node_record_bytes,
            st.s0_resident_bytes,
            fused
        );
        if rich {
            rich_mb = delta_mb;
            rich_entries = entries_mb;
        } else {
            rep.gate(
                "info legacy M-9 25 MB on the lean world (superseded by M-9')",
                format!("+{delta_mb:.1} MB (entries alone +{entries_mb:.1} MB)"),
                true,
            );
        }
    }
    eprintln!(
        "sizes: SemEntry {} B, NodeRegs {} B, S0Node {} B, Input {} B, FactIn {} B",
        size_of::<SemEntry>(),
        size_of::<NodeRegs>(),
        size_of::<S0Node>(),
        size_of::<Input>(),
        size_of::<FactIn>()
    );
    rep.put("mem", format!("{{{json}}}"));
    rep.gate(
        "M-9 SemEntry size <= 160 B (x32 = 128 B)",
        format!("{} B", size_of::<SemEntry>()),
        size_of::<SemEntry>() <= 160 && size_of::<SemEntry>() == 128,
    );
    rep.gate(
        "M-9' engine RSS increase <= 64 MB per 100k nodes on the realistic world (14 groups/node)",
        format!("+{rich_mb:.1} MB (entries alone +{rich_entries:.1} MB)"),
        rich_mb <= 64.0,
    );
}

// ---- scenario: storm -----------------------------------------------------------------------------

struct StormOut {
    per_event_ns: Pct,
    /// Event time divided by the fusions it caused (per-event mode).
    per_fusion_ns: Pct,
    /// Time of events that caused exactly one fusion (no cascade).
    single_fusion_ns: Pct,
    batch_ns_per_event: Pct,
    batch_total_us: Pct,
    lag_batches: usize,
    batches: usize,
    pending_max: usize,
    pending_end: usize,
    events: usize,
    util: f64,
    focus_us: Pct,
    tick_us: Pct,
    fuse_per_event: f64,
    cutoff_share: f64,
    publish_per_event: f64,
}

/// Real-time paced storm. `batched`: one apply+run per 1 ms coalescer tick; otherwise one per
/// event (per-event tail, the strict reading of "p99 <= 2 us/event").
fn storm(w: &mut World, rate: usize, secs: f64, batched: bool, facts_rate: usize) -> StormOut {
    let tick = if batched {
        Duration::from_millis(1)
    } else {
        Duration::from_secs_f64(1.0 / rate as f64)
    };
    let (per_tick, facts_per_tick) = if batched {
        (rate / 1000, facts_rate / 1000)
    } else {
        (1, 0)
    };
    let mut fact_acc = 0.0f64;
    let fact_per_event = facts_rate as f64 / rate as f64;
    let total_ticks = (secs / tick.as_secs_f64()) as usize;
    let mut per_event = Vec::with_capacity(total_ticks);
    let mut per_fusion = Vec::with_capacity(total_ticks);
    let mut single_fusion = Vec::with_capacity(total_ticks);
    let mut batch_pe = Vec::new();
    let mut batch_tot = Vec::new();
    let mut focus = Vec::new();
    let mut tick_series = Vec::new();
    let mut lag = 0usize;
    let mut pending_max = 0usize;
    let mut busy = Duration::ZERO;
    let mut events = 0usize;
    let (mut fuses, mut cutoffs, mut pubs) = (0usize, 0usize, 0usize);
    w.set_focus();
    let clock0 = w.clock;
    let start = Instant::now();
    let mut next = start;
    let mut batch: Vec<Input> = Vec::with_capacity(per_tick + facts_per_tick + 2);
    for k in 0..total_ticks {
        next += tick;
        spin_until(next);
        // Generate the inputs (untimed: they come from the connector / the secondary).
        batch.clear();
        w.clock = clock0 + start.elapsed().as_millis() as u64;
        if batched {
            for _ in 0..per_tick {
                batch.push(w.primary_event());
            }
            for _ in 0..facts_per_tick {
                batch.push(w.fact());
            }
        } else {
            batch.push(w.primary_event());
            fact_acc += fact_per_event;
            while fact_acc >= 1.0 {
                fact_acc -= 1.0;
                batch.push(w.fact());
            }
        }
        // The logical clock advances once per coalescer batch (per 1000 events in per-event mode);
        // timed apart because the TTL scan makes it a different kind of cost (see `tick`).
        if batched || k % 1000 == 0 {
            let tt = Instant::now();
            w.e.apply([Input::Tick(w.clock)]);
            let d = tt.elapsed();
            tick_series.push(us(d));
            busy += d;
        }
        let n_in = batch.len();
        let t0 = Instant::now();
        let ar = w.e.apply(batch.drain(..));
        let rr =
            w.e.run(Budget::until(Instant::now() + Duration::from_millis(2)));
        let publ = rr.published.len();
        let fc = rr.fuse_calls;
        fuses += rr.fuse_calls;
        cutoffs += rr.cutoff;
        pubs += publ;
        drop(rr.published);
        let dt = t0.elapsed();
        black_box((ar.applied, publ));
        busy += dt;
        events += n_in;
        pending_max = pending_max.max(rr.pending);
        if batched {
            batch_pe.push(dt.as_nanos() as f64 / n_in as f64);
            batch_tot.push(us(dt));
            if dt > tick {
                lag += 1;
            }
        } else {
            per_event.push(dt.as_nanos() as f64);
            per_fusion.push(dt.as_nanos() as f64 / fc.max(1) as f64);
            if fc == 1 {
                single_fusion.push(dt.as_nanos() as f64);
            }
            if dt > Duration::from_micros(50) {
                lag += 1;
            }
        }
        // The focus moves every 100 ms (a keystroke-ish rate); timed apart.
        if k % (100_000 / tick.as_micros().max(1) as usize) == 0 && k > 0 {
            let t = Instant::now();
            w.set_focus();
            let r = w.e.run(Budget::calls(16));
            focus.push(us(t.elapsed()));
            black_box(r.fuse_calls);
        }
    }
    // Drain what is left to report the end backlog, then settle.
    let pending_end = w.e.pending();
    w.e.run(Budget::UNLIMITED);
    StormOut {
        per_event_ns: pct(per_event),
        per_fusion_ns: pct(per_fusion),
        single_fusion_ns: pct(single_fusion),
        batch_ns_per_event: pct(batch_pe),
        batch_total_us: pct(batch_tot),
        lag_batches: lag,
        batches: total_ticks,
        pending_max,
        pending_end,
        events,
        util: busy.as_secs_f64() / start.elapsed().as_secs_f64(),
        focus_us: pct(focus),
        tick_us: pct(tick_series),
        fuse_per_event: fuses as f64 / events as f64,
        cutoff_share: cutoffs as f64 / fuses.max(1) as f64,
        publish_per_event: pubs as f64 / events as f64,
    }
}

fn scenario_storm(rep: &mut Report, secs: f64) {
    eprintln!("\n== storm: mixed events, 100k nodes, 100 patches, secondary 5k facts/s ==");
    let mut json = String::from("[");
    let mut worst_pe = 0.0f64;
    let mut worst_batch = 0.0f64;
    let mut worst_lag = 0usize;
    for (i, rate) in [10_000usize, 25_000, 50_000].into_iter().enumerate() {
        for batched in [false, true] {
            let mut w = World::build(100_000, 100, false);
            let r = storm(&mut w, rate, secs, batched, 5_000);
            eprintln!(
                "rate {rate}/s {:7}: events {} util {:.1} % lag {} / {}  pending max {} end {}",
                if batched { "batched" } else { "per-ev" },
                r.events,
                r.util * 100.0,
                r.lag_batches,
                r.batches,
                r.pending_max,
                r.pending_end
            );
            if batched {
                eprintln!(
                    "    per-event cost (batch avg): {}",
                    r.batch_ns_per_event.show("ns")
                );
                eprintln!(
                    "    batch total:                {}",
                    r.batch_total_us.show("us")
                );
                worst_batch = worst_batch.max(r.batch_ns_per_event.p99);
            } else {
                eprintln!(
                    "    per-event cost (apply+run+publish): {}",
                    r.per_event_ns.show("ns")
                );
                eprintln!(
                    "    per-fusion cost (event time / fusions): {}",
                    r.per_fusion_ns.show("ns")
                );
                eprintln!(
                    "    events with exactly one fusion:        {}",
                    r.single_fusion_ns.show("ns")
                );
                worst_pe = worst_pe.max(r.per_event_ns.p99);
            }
            eprintln!("    focus change (set + run 16): {}", r.focus_us.show("us"));
            eprintln!(
                "    Input::Tick (apart, TTL scan):  {}",
                r.tick_us.show("us")
            );
            eprintln!(
                "    fusions/event {:.2}, early-cutoff share {:.0} %, publishes/event {:.2}",
                r.fuse_per_event,
                r.cutoff_share * 100.0,
                r.publish_per_event
            );
            if batched {
                worst_lag = worst_lag.max(r.lag_batches);
            }
            let _ = write!(
                json,
                "{}{{\"rate\":{rate},\"mode\":\"{}\",\"events\":{},\"util\":{:.4},\"lag_batches\":{},\"batches\":{},\"pending_max\":{},\"pending_end\":{},\"per_event_ns\":{},\"batch_ns_per_event\":{},\"batch_total_us\":{},\"focus_us\":{},\"tick_us\":{},\"fuse_per_event\":{:.3},\"cutoff_share\":{:.3},\"publish_per_event\":{:.3}}}",
                if i == 0 && !batched { "" } else { "," },
                if batched { "batched-1ms" } else { "per-event" },
                r.events,
                r.util,
                r.lag_batches,
                r.batches,
                r.pending_max,
                r.pending_end,
                r.per_event_ns.json(),
                r.batch_ns_per_event.json(),
                r.batch_total_us.json(),
                r.focus_us.json(),
                r.tick_us.json(),
                r.fuse_per_event,
                r.cutoff_share,
                r.publish_per_event
            );
        }
    }
    json.push(']');
    rep.put("storm", json);
    rep.gate(
        "M-11 fusion+publish p99 <= 2 us/event, per-event (worst of 10k/25k/50k)",
        format!("{:.2} us", worst_pe / 1e3),
        worst_pe <= 2_000.0,
    );
    rep.gate(
        "M-11 fusion+publish p99 <= 2 us/event, 1 ms coalescer batches (worst rate)",
        format!("{:.2} us", worst_batch / 1e3),
        worst_batch <= 2_000.0,
    );
    rep.gate(
        "M-11 no ingress lag: 1 ms coalescer batches whose apply+run exceeds the 1 ms tick (worst rate, 6 s each)",
        format!("{worst_lag} batches over the tick"),
        worst_lag == 0,
    );
}

fn scenario_attrib(rep: &mut Report, secs: f64) {
    eprintln!("\n== attrib: where the per-event cost comes from (25k/s, per-event) ==");
    let mut json = String::from("[");
    let mut first = true;
    for (patches, facts) in [(0usize, 0usize), (100, 0), (0, 5_000), (100, 5_000)] {
        let mut w = World::build(100_000, patches, false);
        let r = storm(&mut w, 25_000, secs, false, facts);
        eprintln!(
            "patches {patches:>3} facts/s {facts:>4}: {}  fusions/event {:.2} cutoff {:.0} %",
            r.per_event_ns.show("ns"),
            r.fuse_per_event,
            r.cutoff_share * 100.0
        );
        eprintln!(
            "                          per-fusion: {}  | single-fusion events: {}",
            r.per_fusion_ns.show("ns"),
            r.single_fusion_ns.show("ns")
        );
        let _ = write!(
            json,
            "{}{{\"patches\":{patches},\"facts_per_s\":{facts},\"per_event_ns\":{}}}",
            if first { "" } else { "," },
            r.per_event_ns.json()
        );
        first = false;
    }
    json.push(']');
    rep.put("attrib", json);
}

fn scenario_tick(rep: &mut Report) {
    eprintln!("\n== tick: cost of Input::Tick against the number of stored facts ==");
    let mut json = String::from("[");
    let mut first = true;
    let mut costs = Vec::new();
    for facts in [0usize, 1_000, 10_000, 50_000] {
        let mut w = World::build(100_000, 0, false);
        let mut inputs = Vec::with_capacity(facts);
        for _ in 0..facts {
            let f = w.fact();
            inputs.push(f);
        }
        w.e.apply(inputs);
        w.e.run(Budget::UNLIMITED);
        let mut v = Vec::new();
        for _ in 0..200u64 {
            w.clock += 1;
            let t = Instant::now();
            w.e.apply([Input::Tick(w.clock)]);
            v.push(us(t.elapsed()));
        }
        let p = pct(v);
        eprintln!("{facts:>6} facts stored: {}", p.show("us"));
        costs.push(p.p50);
        let _ = write!(
            json,
            "{}{{\"facts\":{facts},\"tick_us\":{}}}",
            if first { "" } else { "," },
            p.json()
        );
        first = false;
    }
    json.push(']');
    rep.put("tick", json);
    rep.gate(
        "Input::Tick cost independent of stored facts (50k vs 0, p50 ratio <= 3 and p50 <= 20 us)",
        format!(
            "p50 {:.1} us @0, {:.1} us @1k, {:.1} us @10k, {:.1} us @50k",
            costs[0], costs[1], costs[2], costs[3]
        ),
        costs[3] <= 20.0 && costs[3] <= costs[0].max(1.0) * 3.0,
    );
}

// ---- scenario: focus change with 100k dirty nodes --------------------------------------------------

fn scenario_focus(rep: &mut Report) {
    eprintln!("\n== focus: focus change with ~100k dirty nodes ==");
    let mut w = World::build(100_000, 100, false);
    // Dirty (nearly) every node: a fresh name for all of them, applied but not run.
    let dirty_all = |w: &mut World| {
        let mut inputs = Vec::with_capacity(100_000);
        for i in 0..w.n {
            let nm = Arc::clone(&w.names[(i + w.seq as usize) % w.names.len()]);
            inputs.push(w.obs(i, Write::Val(Val::Name(nm))));
        }
        w.e.apply(inputs);
    };
    let t = Instant::now();
    dirty_all(&mut w);
    eprintln!(
        "dirtied all: apply {:.0} ms, pending {}",
        t.elapsed().as_secs_f64() * 1e3,
        w.e.pending()
    );
    let (mut set_only, mut chain_ready, mut sync200) = (Vec::new(), Vec::new(), Vec::new());
    let mut prio_work = Vec::new();
    let mut chain_calls: Vec<f64> = Vec::new();
    let mut chain_all_fused = true;
    let reps: usize = std::env::var("M22A_FOCUS_REPS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(600);
    for _ in 0..reps {
        let leaf = w.pick();
        let chain = w.chain_of(leaf);
        let vis: Vec<NodeIdx> = (0..200).map(|k| nid((leaf + k) % w.n)).collect();
        // Make sure every chain node is dirty with a value that differs from the published one,
        // so "fused" is observable as "published" (untimed).
        let mut fresh = Vec::with_capacity(chain.len());
        for c in &chain {
            let nm: Arc<str> = Arc::from(format!("chain{}", w.seq).as_str());
            fresh.push(w.obs(c.0 as usize, Write::Val(Val::Name(nm))));
        }
        w.e.apply(fresh);
        let pending_before = w.e.pending();
        let t0 = Instant::now();
        w.e.set_focus_chain(&chain);
        w.e.set_visible(&vis);
        let t1 = Instant::now();
        // (b) the focus chain itself fused: 16-fusion runs (a single-fusion run would be spent on
        // the aged backlog: aging may take one fusion per run) until every chain node was
        // published (the priority class is ordered deepest first, so visible-window nodes below
        // the focus may go first: `calls` counts them).
        // Generic and group nodes cannot be named (repair R masks it): they are fused but
        // never publish a change here, so only the nameable chain nodes are awaited.
        // "Fused" = no longer dirty (a name patch may override the fresh observed name, in which
        // case fusing legitimately publishes nothing); every chain node must get there.
        let need: Vec<NodeIdx> = chain
            .iter()
            .copied()
            .filter(|c| w.e.is_dirty(*c))
            .collect();
        let mut calls = 0usize;
        let mut need_left = need.len();
        while need_left > 0 && calls < 1500 {
            let r = w.e.run(Budget::calls(16));
            if r.fuse_calls == 0 {
                break;
            }
            calls += r.fuse_calls;
            need_left = need.iter().filter(|c| w.e.is_dirty(**c)).count();
        }
        let mut need = need;
        need.retain(|c| w.e.is_dirty(*c));
        let t2 = Instant::now();
        chain_calls.push(calls as f64);
        if !need.is_empty() {
            chain_all_fused = false;
        }
        // (c) the whole bounded synchronous drain (max_sync_drain = 200 fusions).
        let r2 = w.e.run(Budget::calls(200));
        let t3 = Instant::now();
        black_box(r2.fuse_calls);
        set_only.push(us(t1 - t0));
        chain_ready.push(us(t2 - t0));
        sync200.push(us(t3 - t0));
        prio_work.push(w.e.priority_work() as f64);
        // Re-dirty what the runs consumed, untimed, to keep ~100k dirty.
        let consumed = pending_before.saturating_sub(w.e.pending());
        let mut inputs = Vec::with_capacity(consumed + 8);
        for _ in 0..consumed {
            let i = w.rng.below(w.n);
            let nm = Arc::clone(&w.names[(i + w.seq as usize) % w.names.len()]);
            inputs.push(w.obs(i, Write::Val(Val::Name(nm))));
        }
        w.e.apply(inputs);
    }
    let (a, b, c, p) = (
        pct(set_only),
        pct(chain_ready),
        pct(sync200),
        pct(prio_work),
    );
    let cc = pct(chain_calls);
    eprintln!("pending at end {}", w.e.pending());
    eprintln!(
        "(a) set_focus_chain+set_visible (priority set recompute): {}",
        a.show("us")
    );
    eprintln!(
        "(b) (a) + every focus-chain node fused and published:     {}",
        b.show("us")
    );
    eprintln!(
        "    fusions until the whole chain was published (chain has ~8 nodes): {}",
        cc.show("fusions")
    );
    eprintln!(
        "(c) (b) + max_sync_drain 200 fusions:                      {}",
        c.show("us")
    );
    eprintln!("priority set work (nodes visited): {}", p.show("nodes"));
    rep.put(
        "focus",
        format!(
            "{{\"dirty_nodes\":100000,\"set_only_us\":{},\"chain_ready_us\":{},\"sync_drain_200_us\":{},\"prio_work\":{},\"fusions_until_chain_published\":{},\"chain_all_fused\":{}}}",
            a.json(),
            b.json(),
            c.json(),
            p.json(),
            cc.json(),
            chain_all_fused
        ),
    );
    rep.gate(
        "focus change @100k dirty: synchronous part (set_focus_chain + set_visible) p99 <= 0.5 ms",
        format!("p99 {:.1} us (p50 {:.1}, p95 {:.1})", a.p99, a.p50, a.p95),
        a.p99 <= 500.0,
    );
    rep.gate(
        "focus change @100k dirty: focus chain fused and published within 0.5 ms (p99)",
        format!(
            "p99 {:.1} us (p50 {:.1}); needed p50 {:.0} / p99 {:.0} fusions for ~6 nameable chain nodes; all published: {chain_all_fused}",
            b.p99, b.p50, cc.p50, cc.p99
        ),
        b.p99 <= 500.0 && chain_all_fused,
    );
    rep.gate(
        "info: sync drain (chain + 200 fusions) p99 <= 0.5 ms",
        format!("p99 {:.1} us", c.p99),
        c.p99 <= 500.0,
    );
}

// ---- scenario: bump_global -----------------------------------------------------------------------

fn scenario_global(rep: &mut Report) {
    eprintln!("\n== global: bump_global sync part and sweep ==");
    let mut w = World::build(100_000, 100, false);
    w.set_focus();
    w.e.run(Budget::UNLIMITED);
    let (mut sync, mut runs_us, mut sweep_ms, mut evt_us) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut evt_small_us: Vec<f64> = Vec::new();
    let mut evt_root_us: Vec<f64> = Vec::new();
    let mut max_run = 0.0f64;
    let mut sweep_runs = Vec::new();
    for _ in 0..12 {
        let t = Instant::now();
        let p = w.e.bump_global();
        let d = t.elapsed();
        black_box(p.len());
        sync.push(us(d));
        // Sweep to completion in 2 ms budgets; every 10th run a focus-chain event must come
        // through in that very run (priority class first).
        let t_sweep = Instant::now();
        let mut n_runs = 0usize;
        while w.e.pending() > 0 || w.e.sweep_remaining() > 0 {
            if n_runs % 10 == 5 {
                // The same kind of event three ways: focused leaf with the normal 2 ms deadline,
                // focused leaf with a 32-fusion budget, and the (shallowest) chain root with 32.
                for (node, small, series) in [
                    (w.focus_leaf, false, 0usize),
                    (w.focus_leaf, true, 1),
                    (0usize, true, 2),
                ] {
                    let nm: Arc<str> = Arc::from(format!("evt{}", w.seq).as_str());
                    let inp = w.obs(node, Write::Val(Val::Name(nm)));
                    let te = Instant::now();
                    w.e.apply([inp]);
                    let budget = if small {
                        Budget::calls(32)
                    } else {
                        Budget::until(Instant::now() + Duration::from_millis(2))
                    };
                    // Incremental publication: the time is to the moment the node's result is
                    // handed to the sink, not to the end of the (possibly 2 ms) run.
                    let mut first: Option<Instant> = None;
                    let r = w.e.run_with(budget, &mut |p| {
                        if p.node == nid(node) && first.is_none() {
                            first = Some(Instant::now());
                        }
                    });
                    black_box(r.fuse_calls);
                    let v = first.map_or(f64::NAN, |t| us(t.duration_since(te)));
                    match series {
                        0 => evt_us.push(v),
                        1 => evt_small_us.push(v),
                        _ => evt_root_us.push(v),
                    }
                }
            }
            let t = Instant::now();
            let r =
                w.e.run(Budget::until(Instant::now() + Duration::from_millis(2)));
            let d = us(t.elapsed());
            runs_us.push(d);
            max_run = max_run.max(d);
            black_box(r.fuse_calls);
            n_runs += 1;
        }
        sweep_runs.push(n_runs as f64);
        sweep_ms.push(t_sweep.elapsed().as_secs_f64() * 1e3);
    }
    let missed = evt_us.iter().filter(|x| x.is_nan()).count();
    let missed_small = evt_small_us.iter().filter(|x| x.is_nan()).count();
    let missed_root = evt_root_us.iter().filter(|x| x.is_nan()).count();
    evt_us.retain(|x| !x.is_nan());
    evt_small_us.retain(|x| !x.is_nan());
    evt_root_us.retain(|x| !x.is_nan());
    let n_evt = evt_us.len() + missed;
    let small = pct(evt_small_us);
    let root = pct(evt_root_us);
    let (s, r, sw, ev, nr) = (
        pct(sync),
        pct(runs_us),
        pct(sweep_ms),
        pct(evt_us),
        pct(sweep_runs),
    );
    eprintln!("bump_global sync part:                {}", s.show("us"));
    eprintln!("sweep run duration (2 ms deadline):   {}", r.show("us"));
    eprintln!("sweep total (100k nodes):             {}", sw.show("ms"));
    eprintln!("runs per sweep:                       {}", nr.show("runs"));
    eprintln!(
        "priority event during sweep -> visible via run_with sink, run(until 2 ms): {} (missed: {missed})",
        ev.show("us")
    );
    eprintln!(
        "focused-leaf event during sweep, run(calls 32): {} (missed {missed_small} of {n_evt})",
        small.show("us")
    );
    eprintln!(
        "chain-root event during sweep, run(calls 32):    {} (missed {missed_root} of {n_evt})",
        root.show("us")
    );
    rep.put(
        "global",
        format!(
            "{{\"sync_us\":{},\"sweep_run_us\":{},\"sweep_total_ms\":{},\"runs_per_sweep\":{},\"prio_event_us\":{},\"leaf_event_small_budget_us\":{},\"root_event_small_budget_us\":{},\"missed_leaf_deadline\":{missed},\"missed_leaf_small\":{missed_small},\"missed_root_small\":{missed_root},\"events_per_series\":{n_evt}}}",
            s.json(),
            r.json(),
            sw.json(),
            nr.json(),
            ev.json(),
            small.json(),
            root.json()
        ),
    );
    rep.gate(
        "bump_global sync part @100k nodes p99 <= 0.5 ms",
        format!("p99 {:.1} us", s.p99),
        s.p99 <= 500.0,
    );
    rep.gate(
        "bump_global sweep: focused-leaf event published by the run that follows it (2 ms deadline)",
        format!("{missed} of {n_evt} missed, time to the sink p99 {:.1} us (p50 {:.1}); run() would return only after 2 ms", ev.p99, ev.p50),
        missed == 0,
    );
    rep.gate(
        "bump_global sweep: focus-chain event published within a 32-fusion run (leaf / chain root)",
        format!("leaf {missed_small} of {n_evt} missed, root {missed_root} of {n_evt} missed"),
        missed_small == 0 && missed_root == 0,
    );
}

// ---- scenario: aging --------------------------------------------------------------------------------

fn scenario_aging(rep: &mut Report) {
    eprintln!("\n== aging: background work under a priority storm ==");
    let mut w = World::build(100_000, 100, false);
    w.set_focus();
    w.e.run(Budget::UNLIMITED);
    // Backlog: every node dirty at tick 0, then 100 fusions per run while 80 priority-node events
    // arrive per tick: priority work alone fills the budget.
    let mut inputs = Vec::with_capacity(100_000);
    for i in 0..w.n {
        let nm = Arc::clone(&w.names[(i * 7) % w.names.len()]);
        inputs.push(w.obs(i, Write::Val(Val::Name(nm))));
    }
    w.e.apply(inputs);
    let backlog0 = w.e.pending();
    let prio_nodes: Vec<usize> = (0..200).collect(); // visible window = nodes 0..200 below
    let vis: Vec<NodeIdx> = prio_nodes.iter().map(|&i| nid(i)).collect();
    w.e.set_visible(&vis);
    w.e.set_focus_chain(&[nid(0)]);
    let budget = 100usize;
    let mut fresh_birth: HashMap<NodeIdx, usize> = HashMap::new();
    let mut fresh_lat: Vec<f64> = Vec::new();
    let mut served_per_tick: Vec<f64> = Vec::new();
    let mut tick = 0usize;
    let mut prev_pending = w.e.pending();
    let mut max_tick_wait_no_progress = 0usize;
    let mut stall = 0usize;
    while tick < 6_000 && w.e.pending() > 0 {
        let mut ev = Vec::with_capacity(90);
        for _ in 0..80 {
            // Priority events: distinct visible nodes, fresh value.
            let i = prio_nodes[w.rng.below(prio_nodes.len())];
            let nm = Arc::clone(&w.names[w.rng.below(w.names.len())]);
            ev.push(w.obs(i, Write::Val(Val::Name(nm))));
        }
        // Five fresh events on random non-priority nodes per tick: their latency in ticks.
        for _ in 0..5 {
            let i = 1000 + w.rng.below(w.n - 1000);
            let nm: Arc<str> = Arc::from(format!("fresh{}", w.seq).as_str());
            ev.push(w.obs(i, Write::Val(Val::Name(nm))));
            fresh_birth.entry(nid(i)).or_insert(tick);
        }
        w.e.apply(ev);
        let r = w.e.run(Budget::calls(budget));
        let mut served = 0usize;
        for p in &r.published {
            if p.node.0 >= 1000 {
                served += 1;
                if let Some(b) = fresh_birth.remove(&p.node) {
                    fresh_lat.push((tick - b) as f64);
                }
            }
        }
        served_per_tick.push(served as f64);
        let pend = w.e.pending();
        if pend >= prev_pending {
            stall += 1;
            max_tick_wait_no_progress = max_tick_wait_no_progress.max(stall);
        } else {
            stall = 0;
        }
        prev_pending = pend;
        tick += 1;
        if tick.is_multiple_of(1000) {
            eprintln!("  tick {tick}: pending {}", w.e.pending());
        }
    }
    let fl = pct(fresh_lat);
    let sv = pct(served_per_tick);
    let drained = backlog0.saturating_sub(w.e.pending());
    eprintln!(
        "backlog {backlog0}: drained {drained} in {tick} ticks (budget {budget}/run, 80 prio events/tick); pending now {}",
        w.e.pending()
    );
    eprintln!("non-priority nodes published per run: {}", sv.show("nodes"));
    eprintln!(
        "fresh non-priority event latency (ticks, behind the backlog): {}",
        fl.show("ticks")
    );
    eprintln!("longest run of ticks without net progress: {max_tick_wait_no_progress}");
    // Steady state: no backlog, the same storm; a fresh non-priority event waits how long?
    w.e.run(Budget::UNLIMITED);
    let mut fresh_birth: HashMap<NodeIdx, usize> = HashMap::new();
    let mut lat2 = Vec::new();
    for t in 0..2_000usize {
        let mut ev = Vec::new();
        for _ in 0..80 {
            let i = prio_nodes[w.rng.below(prio_nodes.len())];
            let nm = Arc::clone(&w.names[w.rng.below(w.names.len())]);
            ev.push(w.obs(i, Write::Val(Val::Name(nm))));
        }
        for _ in 0..5 {
            let i = 1000 + w.rng.below(w.n - 1000);
            let nm: Arc<str> = Arc::from(format!("fresh{}", w.seq).as_str());
            ev.push(w.obs(i, Write::Val(Val::Name(nm))));
            fresh_birth.entry(nid(i)).or_insert(t);
        }
        w.e.apply(ev);
        let r = w.e.run(Budget::calls(budget));
        for p in &r.published {
            if let Some(b) = fresh_birth.remove(&p.node) {
                lat2.push((t - b) as f64);
            }
        }
    }
    let l2 = pct(lat2);
    eprintln!(
        "steady state (no backlog) fresh non-priority latency (ticks): {}",
        l2.show("ticks")
    );
    let min_share = sv.p50;
    rep.put(
        "aging",
        format!(
            "{{\"backlog\":{backlog0},\"budget\":{budget},\"ticks\":{tick},\"drained\":{drained},\"pending_end\":{},\"served_per_run\":{},\"fresh_latency_ticks_backlog\":{},\"fresh_latency_ticks_steady\":{},\"max_stall_ticks\":{max_tick_wait_no_progress}}}",
            w.e.pending(),
            sv.json(),
            fl.json(),
            l2.json()
        ),
    );
    rep.gate(
        "aging: backlog drains under a saturating priority storm (non-priority share >= 1 node/run, quarter of budget = 25)",
        format!(
            "drained {drained}/{backlog0} in {tick} ticks; median {min_share:.0} non-priority nodes/run; longest no-progress {max_tick_wait_no_progress} ticks"
        ),
        drained == backlog0 || (min_share >= 20.0 && max_tick_wait_no_progress < 10),
    );
    rep.gate(
        "aging: steady-state fresh non-priority event published within 3 ticks (p99)",
        format!("p99 {:.0} ticks (p50 {:.0})", l2.p99, l2.p50),
        l2.p99 <= 3.0,
    );
}

// ---- scenario: anchors (S-3) -----------------------------------------------------------------------------

fn dom_anchor(i: usize, count: u16) -> AnchorId {
    AnchorId::Dom {
        id: Arc::from(format!("d{i}").as_str()),
        count,
        frame: 0,
        shadow: 0,
    }
}

fn ocr_fact(e: &Engine, seq: u64, anchor: AnchorId, node: usize, text: &Arc<str>) -> Input {
    let res = e.resolve_anchor(&anchor);
    Input::Fact(Box::new(FactIn {
        source: sid(OCR),
        anchor,
        group: Group::Name.group_code(),
        seq,
        payload: Payload::Sem(Write::Val(Val::Name(Arc::clone(text)))),
        p_milli: 990,
        basis: Basis::VisRev(e.vis_rev(nid(node)) + 1),
        bind_gen: res.generation,
        admitted_at_ms: e.clock(),
        ttl_ms: Some(3_600_000),
    }))
}

fn scenario_anchors(rep: &mut Report) {
    eprintln!("\n== anchors: per-key re-resolution cost (S-3) ==");
    let mut json = String::from("[");
    let mut worst = 0.0f64;
    let mut costs = Vec::new();
    for (ai, n_anchors) in [2_000usize, 20_000].into_iter().enumerate() {
        let mut w = World::build(100_000, 0, false);
        let text: Arc<str> = Arc::from("icon text");
        // `n_anchors` Dom anchors, one per holder node d{5k}.
        let holders: Vec<usize> = w.dom_nodes.iter().copied().take(n_anchors).collect();
        let mut seq = 5_000_000u64;
        let mut inputs = Vec::new();
        for &h in &holders {
            seq += 1;
            inputs.push(ocr_fact(&w.e, seq, dom_anchor(h, 1), h, &text));
        }
        w.e.apply(inputs);
        w.e.run(Budget::UNLIMITED);
        // (1) Move ids: batches of 100 holders change their HTML id (old id loses, new id gains).
        let mut per_key = Vec::new();
        let mut batch_us = Vec::new();
        for round in 0..40 {
            let base = round * 100;
            let mut inp = Vec::new();
            for &h in holders.iter().skip(base).take(100) {
                seq += 1;
                inp.push(Input::Attr {
                    node: nid(h),
                    seq,
                    id: Some(Arc::from(format!("moved{h}").as_str())),
                });
            }
            let r0 = w.e.rebind_count();
            let t = Instant::now();
            w.e.apply(inp);
            let d = t.elapsed();
            let rb = w.e.rebind_count() - r0;
            let t2 = Instant::now();
            w.e.run(Budget::UNLIMITED);
            let drun = t2.elapsed();
            if rb > 0 {
                per_key.push(d.as_nanos() as f64 / rb as f64);
            }
            batch_us.push(us(d + drun));
        }
        let pk = pct(per_key);
        let bu = pct(batch_us);
        eprintln!(
            "{n_anchors:>6} anchors, 100 id moves/batch: apply cost per re-resolved key {}   batch (apply+run) {}",
            pk.show("ns"),
            bu.show("us")
        );
        worst = worst.max(pk.p99);
        costs.push(pk.p50);
        // (2) A hot id: F duplicate-count anchors on one holder; one Attr change rebinds them all.
        let mut fan = String::from("[");
        for (fi, f) in [1usize, 16, 64, 256].into_iter().enumerate() {
            let h = holders[holders.len() - 1 - fi];
            let mut inp = Vec::new();
            for c in 0..f {
                seq += 1;
                inp.push(ocr_fact(
                    &w.e,
                    seq,
                    dom_anchor(h, u16::try_from(c + 1).unwrap()),
                    h,
                    &text,
                ));
            }
            w.e.apply(inp);
            w.e.run(Budget::UNLIMITED);
            let mut ns = Vec::new();
            let mut tot_us = Vec::new();
            for k in 0..30 {
                seq += 1;
                let r0 = w.e.rebind_count();
                let t = Instant::now();
                w.e.apply([Input::Attr {
                    node: nid(h),
                    seq,
                    id: Some(Arc::from(if k % 2 == 0 {
                        "tmpX"
                    } else {
                        format!("d{h}").leak()
                    })),
                }]);
                let d = t.elapsed();
                let rb = w.e.rebind_count() - r0;
                w.e.run(Budget::UNLIMITED);
                if rb > 0 {
                    ns.push(d.as_nanos() as f64 / rb as f64);
                    tot_us.push(us(d));
                }
            }
            let (a, b) = (pct(ns), pct(tot_us));
            eprintln!(
                "{n_anchors:>6} anchors, one id with {f:>3} anchors flips: per key {}  per flip {}",
                a.show("ns"),
                b.show("us")
            );
            let _ = write!(
                fan,
                "{}{{\"fanout\":{f},\"per_key_ns\":{},\"per_flip_us\":{}}}",
                if fi == 0 { "" } else { "," },
                a.json(),
                b.json()
            );
            worst = worst.max(a.p99);
        }
        fan.push(']');
        let _ = write!(
            json,
            "{}{{\"anchors\":{n_anchors},\"per_key_ns\":{},\"batch_us\":{},\"hot_id\":{fan}}}",
            if ai == 0 { "" } else { "," },
            pk.json(),
            bu.json()
        );
    }
    json.push(']');
    rep.put("anchors", json);
    let flat = costs.len() == 2 && costs[1] <= costs[0] * 2.0;
    rep.gate(
        "S-3 re-resolution cost per key independent of cached anchor count (20k vs 2k, p50 ratio <= 2)",
        format!(
            "p50 {:.0} ns @2k, {:.0} ns @20k; worst p99 {:.0} ns/key",
            costs[0], costs[1], worst
        ),
        flat,
    );
}

// ---- scenario: lifecycle churn --------------------------------------------------------------------------------

fn scenario_churn(rep: &mut Report) {
    eprintln!("\n== churn: add/remove 200-node subtrees ==");
    let mut w = World::build(100_000, 100, false);
    w.set_focus();
    w.e.run(Budget::UNLIMITED);
    let text: Arc<str> = Arc::from("churn");
    let base_rss = trim_rss_kb();
    let (mut add_ns, mut rm_ns, mut cyc_us) = (Vec::new(), Vec::new(), Vec::new());
    let mut rss_marks = Vec::new();
    let mut seq = 9_000_000u64;
    let mut buf = Vec::new();
    let cycles = 600usize;
    for c in 0..cycles {
        let parent = w.rng.below(100_000 - 1);
        let root = 100_000usize; // reuse one id range: slots recycle like a real writer's
        let t0 = Instant::now();
        for k in 0..200usize {
            let i = root + k;
            let p = if k == 0 {
                nid(parent)
            } else {
                nid(root + (k - 1) / 4)
            };
            w.e.add_node(nid(i), Some(p)).unwrap();
            buf.clear();
            init_inputs(i, seq + 10, &w.names, &mut w.rng, &mut buf);
            if k % 10 == 0 {
                buf.push(ocr_fact(
                    &w.e,
                    seq + 500 + k as u64,
                    AnchorId::Node(nid(i)),
                    i,
                    &text,
                ));
            }
            w.e.apply(buf.drain(..));
        }
        w.e.run(Budget::UNLIMITED);
        let t1 = Instant::now();
        let pubd = w.e.remove_subtree(nid(root)).unwrap();
        w.e.run(Budget::UNLIMITED);
        let t2 = Instant::now();
        black_box(pubd.len());
        seq += 1000;
        add_ns.push((t1 - t0).as_nanos() as f64 / 200.0);
        rm_ns.push((t2 - t1).as_nanos() as f64 / 200.0);
        cyc_us.push(us(t2 - t0));
        if c % 100 == 99 {
            rss_marks.push(trim_rss_kb());
        }
    }
    let ok = w.e.verify().is_ok();
    let (a, r, cy) = (pct(add_ns), pct(rm_ns), pct(cyc_us));
    let growth_mb = rss_marks
        .last()
        .copied()
        .unwrap_or(base_rss)
        .saturating_sub(base_rss) as f64
        / 1024.0;
    eprintln!("add (per node, incl. inputs + fuse):   {}", a.show("ns"));
    eprintln!("remove subtree (per node, incl. run):  {}", r.show("ns"));
    eprintln!("whole cycle (200 nodes):               {}", cy.show("us"));
    eprintln!(
        "RSS marks every 100 cycles (kB): {rss_marks:?}, base {base_rss}; growth {growth_mb:.2} MB; verify {ok}"
    );
    rep.put(
        "churn",
        format!(
            "{{\"cycles\":{cycles},\"add_ns_per_node\":{},\"remove_ns_per_node\":{},\"cycle_us\":{},\"rss_base_kb\":{base_rss},\"rss_marks_kb\":{:?},\"verify_ok\":{ok}}}",
            a.json(),
            r.json(),
            cy.json(),
            rss_marks
        ),
    );
    rep.gate(
        "churn: engine consistent (verify) and RSS growth over 600 add/remove cycles <= 2 MB",
        format!("growth {growth_mb:.2} MB, verify {ok}"),
        ok && growth_mb <= 2.0,
    );
}

// ---- main ---------------------------------------------------------------------------------------------------

fn main() {
    pin_cpu();
    let only: Option<Vec<String>> = std::env::var("M22A_ONLY")
        .ok()
        .map(|s| s.split(',').map(str::to_owned).collect());
    let want = |n: &str| only.as_ref().is_none_or(|o| o.iter().any(|x| x == n));
    let secs: f64 = std::env::var("M22A_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(6.0);
    let stamp = std::env::var("M22A_STAMP").unwrap_or_else(|_| "adhoc".into());
    let host = host_label();
    eprintln!("fuse bench host: {host}");
    let mut rep = Report::default();
    rep.put("secs_per_storm_run", format!("{secs}"));
    if want("mem") {
        scenario_mem(&mut rep);
    }
    if want("storm") {
        scenario_storm(&mut rep, secs);
    }
    if want("tick") {
        scenario_tick(&mut rep);
    }
    if want("attrib") {
        scenario_attrib(&mut rep, secs);
    }
    if want("focus") {
        scenario_focus(&mut rep);
    }
    if want("global") {
        scenario_global(&mut rep);
    }
    if want("aging") {
        scenario_aging(&mut rep);
    }
    if want("anchors") {
        scenario_anchors(&mut rep);
    }
    if want("churn") {
        scenario_churn(&mut rep);
    }
    let host_end = host_label();
    rep.put("host_end", format!("\"{host_end}\""));
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/m22a/results");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{stamp}-fuse.json"));
    if std::fs::write(&path, rep.json(&host)).is_ok() {
        eprintln!("\nwrote {}", path.display());
    }
    eprintln!("\n== gate summary ==");
    for (n, m, p) in &rep.gates {
        eprintln!("{} | {n} | {m}", if *p { "PASS" } else { "FAIL" });
    }
}
