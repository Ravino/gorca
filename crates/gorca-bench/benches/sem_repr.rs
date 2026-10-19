// SPDX-License-Identifier: GPL-3.0-or-later
//! M22a-A4: representation micro-bench for ADR-0022 section 3, alternative A (the typed
//! `SemEntry`, layout x32, 128 B) against alternative C (a generic property bag
//! `PropId -> PropValue`). Math report section 10.1 already measured full-entry copy costs; this
//! adds the **compose** cost (reading the fields a presentation rule reads and emitting tokens),
//! the CoW update + early-cutoff compare, build cost and heap footprint on a 100k-node page.
//!
//! C variants (best realistic cases, not strawmen):
//! * `C-sorted`: `Vec<(u16, PropValue)>` sorted by id, binary-search lookup (smallest bag);
//! * `C-hash`:   `HashMap<u16, PropValue>` (std SipHash; the "obvious" bag).
//!
//! Reported per scenario: p50/p95/p99 of per-node cost over batches of 256 nodes
//! (`bench/m22a/results/<stamp>-sem_repr.json`, host load and governor recorded), then criterion
//! means for the headline compose case. Run: `M22A_STAMP=<id> cargo bench -p gorca-bench --bench
//! sem_repr`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]

use std::collections::HashMap;
use std::fmt::Write as _;
use std::hint::black_box;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Instant;

use criterion::Criterion;
use gorca_model::event::Politeness;
use gorca_model::sem::{
    Avail, Check, Expand, Live, Orientation, PosSrc, Relevant, SemEntry, SemFlags, SemRole,
    SemText, SetPos,
};

// ---- C: generic property bag ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
enum PropValue {
    U8(u8),
    U32(u32),
    Str(Arc<str>),
    Pos(u32, u32, u8),
    Live(u8, bool, u8),
}

const P_ROLE: u16 = 0;
const P_NAME: u16 = 1;
const P_DESC: u16 = 2;
const P_CHECK: u16 = 3;
const P_EXPAND: u16 = 5;
const P_AVAIL: u16 = 10;
const P_FLAGS: u16 = 11;
const P_LEVEL: u16 = 12;
const P_POS: u16 = 13;
const P_LIVE: u16 = 15;
const P_ORIENT: u16 = 16;

#[derive(Clone, Debug, PartialEq, Eq)]
struct BagSorted(Vec<(u16, PropValue)>);

#[derive(Clone, Debug, PartialEq, Eq)]
struct BagHash(HashMap<u16, PropValue>);

trait Bag {
    fn get(&self, id: u16) -> Option<&PropValue>;
}

impl Bag for BagSorted {
    #[inline]
    fn get(&self, id: u16) -> Option<&PropValue> {
        self.0
            .binary_search_by_key(&id, |(k, _)| *k)
            .ok()
            .map(|i| &self.0[i].1)
    }
}

impl Bag for BagHash {
    #[inline]
    fn get(&self, id: u16) -> Option<&PropValue> {
        self.0.get(&id)
    }
}

// ---- node populations ------------------------------------------------------------------------

#[derive(Copy, Clone, Debug)]
enum Kind {
    Plain,
    Form,
    Heading,
    ListItem,
    Rich,
}

const KINDS: [Kind; 5] = [
    Kind::Plain,
    Kind::Form,
    Kind::Heading,
    Kind::ListItem,
    Kind::Rich,
];

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Plain => "plain",
            Kind::Form => "form",
            Kind::Heading => "heading",
            Kind::ListItem => "listitem",
            Kind::Rich => "rich",
        }
    }
}

/// The neutral description of a node; both representations are built from it.
#[derive(Clone)]
struct Spec {
    role: SemRole,
    name: Option<Arc<str>>,
    check: Option<Check>,
    expand: Option<Expand>,
    avail: Option<Avail>,
    level: u8,
    pos: Option<(u32, u32)>,
    flags: SemFlags,
    desc: Option<Arc<str>>,
    live: bool,
    orient: Option<Orientation>,
}

fn spec(kind: Kind, i: usize) -> Spec {
    let name: Arc<str> = Arc::from(format!("Node {i} label").as_str());
    let mut s = Spec {
        role: SemRole::Generic,
        name: Some(name),
        check: None,
        expand: None,
        avail: None,
        level: 0,
        pos: None,
        flags: SemFlags::empty(),
        desc: None,
        live: false,
        orient: None,
    };
    match kind {
        Kind::Plain => s.role = SemRole::Link,
        Kind::Form => {
            s.role = SemRole::Checkbox;
            s.check = Some(if i.is_multiple_of(2) {
                Check::On
            } else {
                Check::Off
            });
            s.avail = Some(Avail::Enabled);
            s.flags = SemFlags::REQUIRED;
        }
        Kind::Heading => {
            s.role = SemRole::Heading;
            s.level = u8::try_from(i % 6 + 1).unwrap();
        }
        Kind::ListItem => {
            s.role = SemRole::ListItem;
            s.pos = Some((u32::try_from(i % 40 + 1).unwrap(), 40));
        }
        Kind::Rich => {
            s.role = SemRole::Button;
            s.check = Some(Check::Off);
            s.expand = Some(Expand::Collapsed);
            s.avail = Some(Avail::Enabled);
            s.level = 2;
            s.pos = Some((3, 9));
            s.flags = SemFlags::REQUIRED | SemFlags::READ_ONLY;
            s.desc = Some(Arc::from("Opens the settings dialog"));
            s.live = true;
            s.orient = Some(Orientation::Horizontal);
        }
    }
    s
}

fn build_a(s: &Spec) -> Arc<SemEntry> {
    let mut e = SemEntry::default();
    e.sem.role = s.role;
    e.name.clone_from(&s.name);
    if let Some(c) = s.check {
        e.sem.check = c;
    }
    if let Some(x) = s.expand {
        e.sem.expand = x;
    }
    if let Some(a) = s.avail {
        e.sem.avail = a;
    }
    e.sem.level = s.level;
    e.sem.flags = s.flags;
    if let Some((p, n)) = s.pos {
        e.sem.pos = Some(SetPos {
            pos: p,
            size: NonZeroU32::new(n),
            src: PosSrc::Author,
        });
    }
    if let Some(d) = &s.desc {
        e.sem.text = Some(Arc::new(SemText {
            description: Some(Arc::clone(d)),
            ..SemText::default()
        }));
    }
    if s.live {
        e.sem.live = Some(Live {
            politeness: Politeness::Polite,
            atomic: false,
            relevant: Relevant::empty(),
            root: false,
        });
    }
    if let Some(o) = s.orient {
        e.sem.orientation = o;
    }
    Arc::new(e)
}

fn props(s: &Spec) -> Vec<(u16, PropValue)> {
    let mut v = vec![(P_ROLE, PropValue::U32(u32::from(s.role.code())))];
    if let Some(n) = &s.name {
        v.push((P_NAME, PropValue::Str(Arc::clone(n))));
    }
    if let Some(d) = &s.desc {
        v.push((P_DESC, PropValue::Str(Arc::clone(d))));
    }
    if let Some(c) = s.check {
        v.push((P_CHECK, PropValue::U8(c.code())));
    }
    if let Some(x) = s.expand {
        v.push((P_EXPAND, PropValue::U8(x.code())));
    }
    if let Some(a) = s.avail {
        v.push((P_AVAIL, PropValue::U8(a.code())));
    }
    if !s.flags.is_empty() {
        v.push((P_FLAGS, PropValue::U32(s.flags.bits())));
    }
    if s.level != 0 {
        v.push((P_LEVEL, PropValue::U8(s.level)));
    }
    if let Some((p, n)) = s.pos {
        v.push((P_POS, PropValue::Pos(p, n, 0)));
    }
    if s.live {
        v.push((P_LIVE, PropValue::Live(1, false, 0)));
    }
    if let Some(o) = s.orient {
        v.push((P_ORIENT, PropValue::U8(o.code())));
    }
    v.sort_by_key(|(k, _)| *k);
    v
}

fn build_cs(s: &Spec) -> Arc<BagSorted> {
    Arc::new(BagSorted(props(s)))
}

fn build_ch(s: &Spec) -> Arc<BagHash> {
    Arc::new(BagHash(props(s).into_iter().collect()))
}

// ---- compose: read what a presentation rule reads, emit tokens ---------------------------------

#[derive(Copy, Clone)]
enum Tok {
    Role(u16),
    Text(usize),
    Code(u8, u8),
    Num(u8, u32, u32),
}

const CHECKABLE: u16 = 1;
const EXPANDABLE: u16 = 2;
const LEVELED: u16 = 4;
const POSITIONED: u16 = 8;

/// Role applicability, shared by both representations (cheap table; not under test).
fn role_rules(role: u16) -> u16 {
    match role {
        r if r == SemRole::Checkbox.code() => CHECKABLE,
        r if r == SemRole::Button.code() => CHECKABLE | EXPANDABLE | LEVELED | POSITIONED,
        r if r == SemRole::Heading.code() => LEVELED,
        r if r == SemRole::ListItem.code() => POSITIONED | LEVELED,
        _ => 0,
    }
}

fn compose_a(e: &SemEntry, out: &mut Vec<Tok>) {
    out.clear();
    let role = e.sem.role.code();
    let rules = role_rules(role);
    out.push(Tok::Role(role));
    if let Some(n) = &e.name {
        out.push(Tok::Text(n.len()));
    }
    if rules & CHECKABLE != 0 {
        out.push(Tok::Code(0, e.sem.check.code()));
    }
    if rules & EXPANDABLE != 0 {
        out.push(Tok::Code(1, e.sem.expand.code()));
    }
    out.push(Tok::Code(2, e.sem.avail.code()));
    if rules & LEVELED != 0 && e.sem.level != 0 {
        out.push(Tok::Num(0, u32::from(e.sem.level), 0));
    }
    if rules & POSITIONED != 0
        && let Some(p) = e.sem.pos
    {
        out.push(Tok::Num(1, p.pos, p.size.map_or(0, NonZeroU32::get)));
    }
    if !e.sem.flags.is_empty() {
        out.push(Tok::Num(2, e.sem.flags.bits(), 0));
    }
    if let Some(d) = e.sem.description() {
        out.push(Tok::Text(d.len()));
    }
    if e.sem.live.is_some() {
        out.push(Tok::Code(3, 1));
    }
    out.push(Tok::Code(4, e.sem.orientation.code()));
}

fn u8_of(v: Option<&PropValue>) -> u8 {
    match v {
        Some(PropValue::U8(c)) => *c,
        _ => 0,
    }
}

fn compose_c<B: Bag>(b: &B, out: &mut Vec<Tok>) {
    out.clear();
    let role = match b.get(P_ROLE) {
        Some(PropValue::U32(r)) => u16::try_from(*r).unwrap_or(0),
        _ => 0,
    };
    let rules = role_rules(role);
    out.push(Tok::Role(role));
    if let Some(PropValue::Str(n)) = b.get(P_NAME) {
        out.push(Tok::Text(n.len()));
    }
    if rules & CHECKABLE != 0 {
        out.push(Tok::Code(0, u8_of(b.get(P_CHECK))));
    }
    if rules & EXPANDABLE != 0 {
        out.push(Tok::Code(1, u8_of(b.get(P_EXPAND))));
    }
    out.push(Tok::Code(2, u8_of(b.get(P_AVAIL))));
    if rules & LEVELED != 0
        && let Some(PropValue::U8(l)) = b.get(P_LEVEL)
        && *l != 0
    {
        out.push(Tok::Num(0, u32::from(*l), 0));
    }
    if rules & POSITIONED != 0
        && let Some(PropValue::Pos(p, n, _)) = b.get(P_POS)
    {
        out.push(Tok::Num(1, *p, *n));
    }
    if let Some(PropValue::U32(f)) = b.get(P_FLAGS)
        && *f != 0
    {
        out.push(Tok::Num(2, *f, 0));
    }
    if let Some(PropValue::Str(d)) = b.get(P_DESC) {
        out.push(Tok::Text(d.len()));
    }
    if matches!(b.get(P_LIVE), Some(PropValue::Live(..))) {
        out.push(Tok::Code(3, 1));
    }
    out.push(Tok::Code(4, u8_of(b.get(P_ORIENT))));
}

fn fingerprint(t: &[Tok]) -> u64 {
    t.iter().fold(t.len() as u64, |h, x| {
        let v = match *x {
            Tok::Role(r) => u64::from(r),
            Tok::Text(n) => n as u64,
            Tok::Code(a, b) => u64::from(a) << 8 | u64::from(b),
            Tok::Num(a, b, c) => u64::from(a) ^ u64::from(b) << 8 ^ u64::from(c) << 32,
        };
        h.rotate_left(5) ^ v
    })
}

// ---- populations -----------------------------------------------------------------------------

struct Pop {
    a: Vec<Arc<SemEntry>>,
    cs: Vec<Arc<BagSorted>>,
    ch: Vec<Arc<BagHash>>,
}

/// `n` nodes of one kind, or a mix (`None`), allocated in order, then optionally shuffled so
/// that traversal order differs from allocation order (cache-cold behaviour on large `n`).
fn pop(kind: Option<Kind>, n: usize, shuffle: bool) -> Pop {
    let specs: Vec<Spec> = (0..n)
        .map(|i| spec(kind.unwrap_or(KINDS[i % KINDS.len()]), i))
        .collect();
    let mut p = Pop {
        a: specs.iter().map(build_a).collect(),
        cs: specs.iter().map(build_cs).collect(),
        ch: specs.iter().map(build_ch).collect(),
    };
    if shuffle {
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for i in (1..n).rev() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let j = usize::try_from(x % (i as u64 + 1)).unwrap();
            p.a.swap(i, j);
            p.cs.swap(i, j);
            p.ch.swap(i, j);
        }
    }
    p
}

// ---- percentile harness -------------------------------------------------------------------------

const BATCH: usize = 256;

struct Stat {
    p50: f64,
    p95: f64,
    p99: f64,
    mean: f64,
    rsd: f64,
}

/// Per-node ns over `samples` batches of `BATCH` nodes; `f(batch_start)` processes the batch.
fn percentiles(samples: usize, mut f: impl FnMut(usize)) -> Stat {
    for w in 0..64 {
        f(w * BATCH);
    }
    let mut v: Vec<f64> = (0..samples)
        .map(|s| {
            let t = Instant::now();
            f(s * BATCH);
            t.elapsed().as_nanos() as f64 / BATCH as f64
        })
        .collect();
    v.sort_by(f64::total_cmp);
    let mean = v.iter().sum::<f64>() / v.len() as f64;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / v.len() as f64;
    let q = |p: f64| v[((v.len() as f64 * p) as usize).min(v.len() - 1)];
    Stat {
        p50: q(0.50),
        p95: q(0.95),
        p99: q(0.99),
        mean,
        rsd: var.sqrt() / mean,
    }
}

fn heap_chunk(bytes: usize) -> usize {
    // glibc: n + 8 rounded up to 16, minimum 32 (math report 10.1, malloc_usable_size).
    (bytes + 8).div_ceil(16).saturating_mul(16).max(32)
}

fn read_first(path: &str) -> String {
    std::fs::read_to_string(path).map_or_else(|_| "?".into(), |s| s.trim().to_owned())
}

fn host_label() -> String {
    format!(
        "load={} governor={} cpus={}",
        read_first("/proc/loadavg"),
        read_first("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor"),
        std::thread::available_parallelism().map_or(0, usize::from)
    )
}

struct Rows {
    json: String,
    first: bool,
}

impl Rows {
    fn add(&mut self, scn: &str, repr: &str, s: &Stat) {
        eprintln!(
            "{scn:<26} {repr:<9} p50 {:7.1} p95 {:7.1} p99 {:7.1} ns/node  mean {:7.1}  rsd {:4.1}%",
            s.p50,
            s.p95,
            s.p99,
            s.mean,
            s.rsd * 100.0
        );
        let _ = write!(
            self.json,
            "{}\n    {{\"scenario\":\"{scn}\",\"repr\":\"{repr}\",\"p50_ns\":{:.2},\"p95_ns\":{:.2},\"p99_ns\":{:.2},\"mean_ns\":{:.2},\"rsd\":{:.4}}}",
            if self.first { "" } else { "," },
            s.p50,
            s.p95,
            s.p99,
            s.mean,
            s.rsd
        );
        self.first = false;
    }
}

fn run_custom() {
    let host = host_label();
    let stamp = std::env::var("M22A_STAMP").unwrap_or_else(|_| "adhoc".into());
    eprintln!("sem_repr host: {host}");
    let mut rows = Rows {
        json: format!("{{\n  \"host\": \"{host}\",\n  \"rows\": ["),
        first: true,
    };
    let mut out: Vec<Tok> = Vec::with_capacity(16);
    // (scenario, kind, node count, shuffled)
    let scns: Vec<(String, Option<Kind>, usize, bool)> = KINDS
        .iter()
        .map(|k| (format!("compose/{}/warm", k.name()), Some(*k), 4096, false))
        .chain([
            ("compose/mixed/warm".to_owned(), None, 4096, false),
            ("compose/mixed/100k-cold".to_owned(), None, 100_000, true),
        ])
        .collect();
    let mut sink = 0u64;
    for (name, kind, n, shuffle) in &scns {
        let n = *n;
        let p = pop(*kind, n, *shuffle);
        let samples = if n > 10_000 { 380 } else { 4000 };
        let sa = percentiles(samples, |s| {
            for i in 0..BATCH {
                compose_a(&p.a[(s * BATCH + i) % n], &mut out);
                sink ^= fingerprint(&out);
            }
        });
        rows.add(name, "A", &sa);
        let sc = percentiles(samples, |s| {
            for i in 0..BATCH {
                compose_c(&*p.cs[(s * BATCH + i) % n], &mut out);
                sink ^= fingerprint(&out);
            }
        });
        rows.add(name, "C-sorted", &sc);
        let sh = percentiles(samples, |s| {
            for i in 0..BATCH {
                compose_c(&*p.ch[(s * BATCH + i) % n], &mut out);
                sink ^= fingerprint(&out);
            }
        });
        rows.add(name, "C-hash", &sh);
    }
    // CoW re-fuse: clone the entry, change `check`, compare with the old one (early cutoff), drop.
    let p = pop(None, 4096, false);
    let sa = percentiles(4000, |s| {
        for i in 0..BATCH {
            let old = &p.a[(s * BATCH + i) % 4096];
            let mut e = SemEntry::clone(old);
            e.sem.check = Check::On;
            sink += u64::from(e == **old);
            black_box(&e);
        }
    });
    rows.add("cow+cutoff/mixed", "A", &sa);
    let sc = percentiles(4000, |s| {
        for i in 0..BATCH {
            let old = &p.cs[(s * BATCH + i) % 4096];
            let mut e = BagSorted::clone(old);
            match e.0.binary_search_by_key(&P_CHECK, |(k, _)| *k) {
                Ok(ix) => e.0[ix].1 = PropValue::U8(2),
                Err(ix) => e.0.insert(ix, (P_CHECK, PropValue::U8(2))),
            }
            sink += u64::from(e == **old);
            black_box(&e);
        }
    });
    rows.add("cow+cutoff/mixed", "C-sorted", &sc);
    let sh = percentiles(4000, |s| {
        for i in 0..BATCH {
            let old = &p.ch[(s * BATCH + i) % 4096];
            let mut e = BagHash::clone(old);
            e.0.insert(P_CHECK, PropValue::U8(2));
            sink += u64::from(e == **old);
            black_box(&e);
        }
    });
    rows.add("cow+cutoff/mixed", "C-hash", &sh);
    // Build from a neutral spec (write-path cost of the initial fuse result).
    let specs: Vec<Spec> = (0..4096).map(|i| spec(KINDS[i % 5], i)).collect();
    let sa = percentiles(4000, |s| {
        for i in 0..BATCH {
            black_box(build_a(&specs[(s * BATCH + i) % 4096]));
        }
    });
    rows.add("build/mixed", "A", &sa);
    let sc = percentiles(4000, |s| {
        for i in 0..BATCH {
            black_box(build_cs(&specs[(s * BATCH + i) % 4096]));
        }
    });
    rows.add("build/mixed", "C-sorted", &sc);
    let sh = percentiles(4000, |s| {
        for i in 0..BATCH {
            black_box(build_ch(&specs[(s * BATCH + i) % 4096]));
        }
    });
    rows.add("build/mixed", "C-hash", &sh);
    black_box(sink);

    // Footprint per node, mixed population (heap chunk sizes; strings shared and excluded).
    let n = 10_000usize;
    let specs: Vec<Spec> = (0..n).map(|i| spec(KINDS[i % 5], i)).collect();
    let a_bytes = n * heap_chunk(16 + std::mem::size_of::<SemEntry>());
    let desc_arcs = specs.iter().filter(|s| s.desc.is_some()).count();
    let a_text = desc_arcs * heap_chunk(16 + std::mem::size_of::<SemText>());
    let cs_bytes: usize = specs
        .iter()
        .map(|s| {
            let k = props(s).len();
            heap_chunk(16 + std::mem::size_of::<BagSorted>())
                + heap_chunk(k * std::mem::size_of::<(u16, PropValue)>())
        })
        .sum();
    let ch_bytes: usize = specs
        .iter()
        .map(|s| {
            let k = props(s).len();
            // hashbrown: buckets = next_pow2(k*8/7), (K+V) slots + ctrl bytes (buckets + 16).
            let buckets = (k * 8).div_ceil(7).next_power_of_two().max(4);
            heap_chunk(16 + std::mem::size_of::<BagHash>())
                + heap_chunk(buckets * std::mem::size_of::<(u16, PropValue)>() + buckets + 16)
        })
        .sum();
    let per = |b: usize| b as f64 / n as f64;
    eprintln!(
        "footprint per node (mixed): A {:.0} B (+{:.1} SemText), C-sorted {:.0} B, C-hash {:.0} B; sizes: SemEntry {} B, PropValue {} B",
        per(a_bytes),
        per(a_text),
        per(cs_bytes),
        per(ch_bytes),
        std::mem::size_of::<SemEntry>(),
        std::mem::size_of::<PropValue>()
    );
    let _ = write!(
        rows.json,
        "\n  ],\n  \"footprint_bytes_per_node\": {{\"A\": {:.1}, \"A_semtext\": {:.1}, \"C_sorted\": {:.1}, \"C_hash\": {:.1}, \"sem_entry\": {}, \"prop_value\": {}, \"x100k_MB\": {{\"A\": {:.1}, \"C_sorted\": {:.1}, \"C_hash\": {:.1}}}}}\n}}\n",
        per(a_bytes),
        per(a_text),
        per(cs_bytes),
        per(ch_bytes),
        std::mem::size_of::<SemEntry>(),
        std::mem::size_of::<PropValue>(),
        per(a_bytes + a_text) * 1e5 / 1e6,
        per(cs_bytes) * 1e5 / 1e6,
        per(ch_bytes) * 1e5 / 1e6
    );
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../bench/m22a/results");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("{stamp}-sem_repr.json"));
    if std::fs::write(&path, rows.json).is_ok() {
        eprintln!("wrote {}", path.display());
    }
}

fn main() {
    run_custom();
    let mut c = Criterion::default().configure_from_args();
    let p = pop(None, 4096, false);
    let mut out: Vec<Tok> = Vec::with_capacity(16);
    let mut g = c.benchmark_group("compose_mixed_warm");
    let mut i = 0usize;
    g.bench_function("A", |b| {
        b.iter(|| {
            i = (i + 1) % 4096;
            compose_a(&p.a[i], &mut out);
            black_box(fingerprint(&out))
        });
    });
    g.bench_function("C-sorted", |b| {
        b.iter(|| {
            i = (i + 1) % 4096;
            compose_c(&*p.cs[i], &mut out);
            black_box(fingerprint(&out))
        });
    });
    g.bench_function("C-hash", |b| {
        b.iter(|| {
            i = (i + 1) % 4096;
            compose_c(&*p.ch[i], &mut out);
            black_box(fingerprint(&out))
        });
    });
    g.finish();
    c.final_summary();
}
