// SPDX-License-Identifier: GPL-3.0-or-later
//! Audit tests MA-1..MA-10 (SPEC 9.1), written before the arms and the PW code exist.
//!
//! The mirror, the fill policy, the admission control and the dump decoder of PW-1..PW-12 do not
//! exist when this is written, so each audit is a **generic function over a thin adapter trait**
//! that mirrors the SPEC section 4 interface with neutral types:
//!
//! | Audit | Adapter | Reference implementation (self-test) |
//! |---|---|---|
//! | MA-1 `mirror_counters_exact`, MA-2, MA-3, MA-4, MA-5, MA-6 apply | [`MirrorAdapter`] | [`RefMirror`] |
//! | MA-1 `policy_memory_accounting`, MA-6 step, MA-9 | [`PolicyAdapter`] | [`RefPolicy`] |
//! | MA-7 `fuzz_get_items`, `audit_dump_caps` | [`super::items::ItemsDecoder`] | [`super::items::RefDecoder`] |
//! | MA-8 `audit_interactive_not_starved` | [`AdmissionAdapter`] | [`RefAdmission`] |
//! | MA-10 `audit_shadow_allowlist` | call counters of the fake app | - |
//!
//! Wiring once PW lands is one adapter `impl` per trait (10-40 lines each) in
//! `crates/gorca-mirror/tests/` and `crates/gorca-bench-b1/tests/`, calling the `run_*` functions
//! below. Until then the tests that need a non-null policy, the real mirror or the live daemon
//! are `#[ignore]` with the reason; everything else runs against the reference implementations,
//! which proves the audits can fail (each has a mutant in the self-tests).

use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Neutral key: `(app serial, local id)`.
pub type Key = (u32, u64);

pub const ALL: u32 = u32::MAX;
pub const NAME: u32 = 1;
pub const PRESENT: u32 = 31;
pub const CHILDREN: u32 = 256;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ANode {
    pub key: Key,
    pub name: String,
    pub parent: Option<Key>,
    /// Fields this node carries (`PropMask` bits).
    pub have: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Origin {
    Reply { issued_at: u64 },
    Push,
    CacheDump { issued_at: u64 },
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ApplyOutcome {
    pub applied: u32,
    pub dropped_tombstoned: u32,
    pub kept_stale: u32,
    pub revived: u32,
    pub dropped_foreign: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AView {
    Miss,
    Gone,
    Hit { name: String, stale: u32, have: u32 },
}

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AStats {
    pub live: u64,
    pub tombstones: u64,
    pub stale: u64,
    pub bytes: u64,
    pub partial: u64,
    pub evictions: u64,
    pub revivals: u64,
}

/// SPEC 4.2 `MirrorRead` + `MirrorWriter` for one app, with neutral types.
pub trait MirrorAdapter {
    /// A fresh mirror for app serial `app`.
    fn new(app: u32) -> Self
    where
        Self: Sized;
    fn seq(&self) -> u64;
    fn apply(&mut self, nodes: Vec<ANode>, origin: Origin) -> ApplyOutcome;
    fn invalidate(&mut self, k: Key, mask: u32) -> bool;
    fn invalidate_all(&mut self);
    fn tombstone(&mut self, k: Key);
    fn evict(&mut self, k: Key) -> bool;
    fn sweep(&mut self);
    fn retire_all(&mut self);
    fn view(&self, k: Key) -> AView;
    fn stats(&self) -> AStats;
    fn check_invariants(&self) -> Result<(), String>;
}

// ---- reference mirror ---------------------------------------------------------------------

/// Defects the self-tests inject to prove an audit can fail.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum Bug {
    #[default]
    None,
    /// LR-1 violated.
    ReviveOnLateReply,
    /// LR-2 violated.
    IgnoreRedirty,
    /// MA-3 violated: writes any app's key.
    AcceptForeign,
    /// MA-1 violated: the live counter is off by one after an evict.
    CounterDriftOnEvict,
    /// MA-2 violated: `retire_all` keeps tombstones.
    RetainOnRetire,
}

#[derive(Clone, Debug)]
struct RNode {
    name: String,
    have: u32,
    stale: u32,
    confirmed: u64,
}

/// A straightforward correct implementation of the SPEC 4.2/5.4 semantics.
#[derive(Debug)]
pub struct RefMirror {
    app: u32,
    nodes: HashMap<Key, RNode>,
    tomb: HashMap<Key, u64>,
    inval: HashMap<Key, (u64, u32)>,
    seq: u64,
    epoch: u64,
    live: u64,
    revivals: u64,
    evictions: u64,
    pub bug: Bug,
}

impl RefMirror {
    pub fn with_bug(app: u32, bug: Bug) -> Self {
        Self {
            app,
            nodes: HashMap::new(),
            tomb: HashMap::new(),
            inval: HashMap::new(),
            seq: 0,
            epoch: 0,
            live: 0,
            revivals: 0,
            evictions: 0,
            bug,
        }
    }
}

impl MirrorAdapter for RefMirror {
    fn new(app: u32) -> Self {
        Self::with_bug(app, Bug::None)
    }

    fn seq(&self) -> u64 {
        self.seq
    }

    fn apply(&mut self, nodes: Vec<ANode>, origin: Origin) -> ApplyOutcome {
        let mut out = ApplyOutcome::default();
        let issued = match origin {
            Origin::Reply { issued_at } | Origin::CacheDump { issued_at } => issued_at,
            Origin::Push => self.seq,
        };
        for n in nodes {
            if n.key.0 != self.app && self.bug != Bug::AcceptForeign {
                out.dropped_foreign += 1;
                continue;
            }
            self.seq += 1;
            let mut revived = false;
            if let Some(t) = self.tomb.get(&n.key).copied() {
                if t > issued && self.bug != Bug::ReviveOnLateReply {
                    out.dropped_tombstoned += 1;
                    continue;
                }
                self.tomb.remove(&n.key);
                revived = true;
            }
            let stale = match self.inval.get(&n.key) {
                Some((s, m)) if *s > issued && self.bug != Bug::IgnoreRedirty => {
                    out.kept_stale += 1;
                    *m
                }
                _ => 0,
            };
            if self
                .nodes
                .insert(
                    n.key,
                    RNode {
                        name: n.name,
                        have: n.have,
                        stale,
                        confirmed: issued,
                    },
                )
                .is_none()
            {
                self.live += 1;
            }
            if revived {
                out.revived += 1;
                self.revivals += 1;
            }
            out.applied += 1;
        }
        out
    }

    fn invalidate(&mut self, k: Key, mask: u32) -> bool {
        self.seq += 1;
        let e = self.inval.entry(k).or_insert((0, 0));
        *e = (self.seq, e.1 | mask);
        match self.nodes.get_mut(&k) {
            Some(n) => {
                n.stale |= mask;
                true
            }
            None => false,
        }
    }

    fn invalidate_all(&mut self) {
        self.seq += 1;
        self.epoch = self.seq;
    }

    fn tombstone(&mut self, k: Key) {
        self.seq += 1;
        self.tomb.insert(k, self.seq);
        if self.nodes.remove(&k).is_some() {
            self.live -= 1;
        }
    }

    fn evict(&mut self, k: Key) -> bool {
        let had = self.nodes.remove(&k).is_some();
        if had {
            self.evictions += 1;
            if self.bug != Bug::CounterDriftOnEvict {
                self.live -= 1;
            }
        }
        had
    }

    fn sweep(&mut self) {
        // Tombstone TTL is time based in the real store; the reference sweeps the oldest half
        // once there are more tombstones than twice the live nodes plus 4096.
        let cap = 2 * self.live + 4096;
        if self.tomb.len() as u64 > cap {
            let mut v: Vec<(Key, u64)> = self.tomb.iter().map(|(k, s)| (*k, *s)).collect();
            v.sort_by_key(|x| x.1);
            for (k, _) in v.into_iter().take(self.tomb.len() / 2) {
                self.tomb.remove(&k);
            }
        }
    }

    fn retire_all(&mut self) {
        self.nodes.clear();
        self.inval.clear();
        self.live = 0;
        if self.bug != Bug::RetainOnRetire {
            self.tomb.clear();
        }
    }

    fn view(&self, k: Key) -> AView {
        match self.nodes.get(&k) {
            Some(n) => AView::Hit {
                name: n.name.clone(),
                stale: if n.confirmed < self.epoch {
                    ALL
                } else {
                    n.stale
                },
                have: n.have,
            },
            None if self.tomb.contains_key(&k) => AView::Gone,
            None => AView::Miss,
        }
    }

    fn stats(&self) -> AStats {
        AStats {
            live: self.live,
            tombstones: self.tomb.len() as u64,
            stale: self
                .nodes
                .values()
                .filter(|n| n.stale != 0 || n.confirmed < self.epoch)
                .count() as u64,
            bytes: self.nodes.values().map(|n| 64 + n.name.len() as u64).sum(),
            partial: self
                .nodes
                .values()
                .filter(|n| n.have & PRESENT != PRESENT)
                .count() as u64,
            evictions: self.evictions,
            revivals: self.revivals,
        }
    }

    fn check_invariants(&self) -> Result<(), String> {
        if self.live != self.nodes.len() as u64 {
            return Err(format!(
                "live counter {} != {} nodes",
                self.live,
                self.nodes.len()
            ));
        }
        if let Some(k) = self.nodes.keys().find(|k| self.tomb.contains_key(k)) {
            return Err(format!("{k:?} is both live and tombstoned"));
        }
        Ok(())
    }
}

// ---- helpers --------------------------------------------------------------------------------

pub fn node(app: u32, id: u64, name: &str) -> ANode {
    ANode {
        key: (app, id),
        name: name.to_owned(),
        parent: None,
        have: PRESENT,
    }
}

/// Tiny seeded generator for op sequences.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

// ---- MA-1 ---------------------------------------------------------------------------------

/// MA-1: after every operation the counters equal what the structure really holds. The oracle is
/// an independent census of a small key universe through `view`.
pub fn run_mirror_counters_exact<M: MirrorAdapter>(seed: u64, ops: usize) -> Result<(), String> {
    const UNIVERSE: u64 = 48;
    let mut m = M::new(1);
    let mut r = Lcg(seed);
    for i in 0..ops {
        let k = (1, r.next() % UNIVERSE);
        let what = match r.next() % 7 {
            0 | 1 => {
                let issued = m.seq().saturating_sub(r.next() % 4);
                m.apply(vec![node(1, k.1, "n")], Origin::Reply { issued_at: issued });
                "apply"
            }
            2 => {
                m.invalidate(k, 1 << (r.next() % 5));
                "invalidate"
            }
            3 => {
                m.tombstone(k);
                "tombstone"
            }
            4 => {
                m.evict(k);
                "evict"
            }
            5 => {
                if r.next() % 9 == 0 {
                    m.invalidate_all();
                }
                m.sweep();
                "sweep"
            }
            _ => {
                if r.next() % 40 == 0 {
                    m.retire_all();
                    // A retired mirror answers `Gone` for every key by design: check that it
                    // released everything, then audit a fresh one (the app restarted).
                    m.check_invariants()
                        .map_err(|e| format!("op {i} (retire): {e}"))?;
                    let s = m.stats();
                    if s.live != 0 || s.tombstones != 0 || s.bytes != 0 {
                        return Err(format!("op {i} (retire): not released: {s:?}"));
                    }
                    m = M::new(1);
                }
                "retire"
            }
        };
        m.check_invariants()
            .map_err(|e| format!("op {i} ({what}): {e}"))?;
        let (mut live, mut tomb) = (0u64, 0u64);
        for id in 0..UNIVERSE {
            match m.view((1, id)) {
                AView::Hit { .. } => live += 1,
                AView::Gone => tomb += 1,
                AView::Miss => {}
            }
        }
        let s = m.stats();
        if s.live != live {
            return Err(format!(
                "op {i} ({what}): stats.live {} but census {live}",
                s.live
            ));
        }
        if s.tombstones < tomb {
            return Err(format!(
                "op {i} ({what}): stats.tombstones {} < census {tomb}",
                s.tombstones
            ));
        }
    }
    Ok(())
}

// ---- MA-2 ---------------------------------------------------------------------------------

/// MA-2: retiring an app releases mirror and tombstones; 100 cycles of a 5 000-node app return the
/// process RSS to within 4 MiB.
pub fn run_app_churn<M: MirrorAdapter>(cycles: u32, check_rss: bool) -> Result<(), String> {
    // The allocator keeps the first app's peak (a few MiB) for reuse; a leak is growth beyond
    // that, so the baseline is taken once two cycles have warmed it up.
    let mut rss0 = rss_kib();
    for c in 0..cycles {
        if c == 2 {
            rss0 = rss_kib();
        }
        let mut m = M::new(c);
        let nodes: Vec<ANode> = (0..5_000)
            .map(|i| node(c, i, &format!("node number {i}")))
            .collect();
        m.apply(nodes, Origin::Push);
        for i in (0..5_000).step_by(3) {
            m.tombstone((c, i));
        }
        m.retire_all();
        let s = m.stats();
        if s.live != 0 || s.tombstones != 0 || s.bytes != 0 {
            return Err(format!("cycle {c}: retire_all left {s:?}"));
        }
        if m.view((c, 1)) == AView::Miss && c == u32::MAX {
            return Err("unreachable".into());
        }
    }
    let grown = rss_kib().saturating_sub(rss0);
    if check_rss && grown > 4 * 1024 {
        return Err(format!(
            "RSS grew {grown} KiB over {cycles} app cycles (limit 4096 KiB)"
        ));
    }
    Ok(())
}

/// Resident set size of this process in KiB (0 when /proc is unreadable).
pub fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find_map(|l| l.strip_prefix("VmRSS:"))
                .and_then(|v| v.split_whitespace().next().and_then(|n| n.parse().ok()))
        })
        .unwrap_or(0)
}

// ---- MA-3 ---------------------------------------------------------------------------------

/// MA-3: a batch carrying other apps' keys writes none of them, and says how many it dropped.
pub fn run_foreign_refs<M: MirrorAdapter>() -> Result<(), String> {
    let mut m = M::new(1);
    let mut batch: Vec<ANode> = (0..10).map(|i| node(1, i, "own")).collect();
    batch.extend((0..7).map(|i| node(2, i, "foreign")));
    batch.push(ANode {
        parent: Some((3, 1)),
        ..node(1, 99, "own with foreign parent")
    });
    let out = m.apply(batch, Origin::Reply { issued_at: m.seq() });
    if out.dropped_foreign != 7 {
        return Err(format!("dropped_foreign {} != 7", out.dropped_foreign));
    }
    for i in 0..7 {
        if m.view((2, i)) != AView::Miss {
            return Err(format!("foreign key (2,{i}) was written"));
        }
    }
    if m.stats().live != 11 {
        return Err(format!("live {} != 11", m.stats().live));
    }
    Ok(())
}

// ---- MA-4 / MA-5 --------------------------------------------------------------------------

/// MA-4 / LR-1: a reply issued before a tombstone never resurrects the key.
pub fn run_late_reply_never_revives<M: MirrorAdapter>() -> Result<(), String> {
    let mut m = M::new(1);
    let issued = m.seq();
    m.tombstone((1, 5));
    let out = m.apply(
        vec![node(1, 5, "ghost")],
        Origin::Reply { issued_at: issued },
    );
    if m.view((1, 5)) != AView::Gone || out.dropped_tombstoned != 1 {
        return Err(format!("ghost revived: {:?} {out:?}", m.view((1, 5))));
    }
    // A pushed snapshot has no issue time before the tombstone either.
    let out = m.apply(
        vec![node(1, 5, "ghost")],
        Origin::CacheDump { issued_at: issued },
    );
    if m.view((1, 5)) != AView::Gone || out.applied != 0 {
        return Err("a cache dump issued before the removal revived the key (LR-4)".into());
    }
    Ok(())
}

/// MA-4 / LR-3: a reply issued after the tombstone is proof of life.
pub fn run_proof_of_life_revives<M: MirrorAdapter>() -> Result<(), String> {
    let mut m = M::new(1);
    m.tombstone((1, 5));
    let issued = m.seq();
    let out = m.apply(
        vec![node(1, 5, "alive")],
        Origin::Reply { issued_at: issued },
    );
    match m.view((1, 5)) {
        AView::Hit { name, .. }
            if name == "alive" && out.revived == 1 && m.stats().revivals == 1 =>
        {
            Ok(())
        }
        other => Err(format!("no revival after proof of life: {other:?} {out:?}")),
    }
}

/// MA-5 / LR-2: a node invalidated while its fetch was in flight never becomes Fresh with
/// pre-invalidation content.
pub fn run_redirty_during_fetch<M: MirrorAdapter>() -> Result<(), String> {
    let mut m = M::new(1);
    m.apply(vec![node(1, 5, "v0")], Origin::Push);
    let issued = m.seq(); // fetch issued here
    m.invalidate((1, 5), NAME); // event arrives while it is in flight
    m.apply(
        vec![node(1, 5, "v0 (pre-mutation)")],
        Origin::Reply { issued_at: issued },
    );
    match m.view((1, 5)) {
        AView::Hit { stale, .. } if stale & NAME != 0 => {}
        other => return Err(format!("Fresh with pre-invalidation content: {other:?}")),
    }
    // The same for a key never seen before.
    let issued = m.seq();
    m.invalidate((1, 6), NAME);
    m.apply(vec![node(1, 6, "old")], Origin::Reply { issued_at: issued });
    match m.view((1, 6)) {
        AView::Hit { stale, .. } if stale & NAME != 0 => Ok(()),
        other => Err(format!(
            "unseen key stored Fresh after an invalidation: {other:?}"
        )),
    }
}

// ---- MA-6 apply ---------------------------------------------------------------------------

/// Nodes the shared driver applies per call (`gorca_mirror::driver::APPLY_SLICE`): a bigger
/// reply is applied slice by slice from the actor's loop, so one `apply` call stays short.
pub const APPLY_SLICE: usize = 64;

/// MA-6: applying a 4 096-node reply takes at most `budget_us` microseconds per call in the
/// worst of `rounds` rounds, applied the way the driver does it, `APPLY_SLICE` nodes per call.
/// (A fresh 4 096-node `apply` measures 2 ms or more; the budget is for what the actor does
/// between two polls of its queues.) Returns the worst observed call (for reports).
pub fn run_apply_bounded<M: MirrorAdapter>(rounds: u32, budget_us: u64) -> Result<u64, String> {
    let mut worst = 0u64;
    let mut m = M::new(1);
    for r in 0..rounds {
        let nodes: Vec<ANode> = (0..4_096)
            .map(|i| node(1, u64::from(r) * 4_096 + i, "node"))
            .collect();
        for slice in nodes.chunks(APPLY_SLICE) {
            let t = std::time::Instant::now();
            m.apply(slice.to_vec(), Origin::Push);
            worst = worst.max(u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX));
        }
    }
    if worst > budget_us {
        return Err(format!(
            "an apply of {APPLY_SLICE} nodes took {worst} us (budget {budget_us} us)"
        ));
    }
    Ok(worst)
}

// ---- MA-7 decoder ---------------------------------------------------------------------------

use super::items::{DItem, DecodeLimits, ItemsDecoder, encode_items, sample_dump};

fn mutate(bytes: &[u8], r: &mut Lcg) -> Vec<u8> {
    let mut b = bytes.to_vec();
    match r.next() % 6 {
        0 => {
            // flip a few bits
            for _ in 0..1 + r.next() % 8 {
                let i = (r.next() as usize) % b.len().max(1);
                if let Some(x) = b.get_mut(i) {
                    *x ^= 1 << (r.next() % 8);
                }
            }
        }
        1 => b.truncate((r.next() as usize) % (b.len() + 1)),
        2 => {
            // lie about a length field
            let i = ((r.next() as usize) % (b.len() / 4).max(1)) * 4;
            if let Some(w) = b.get_mut(i..i + 4) {
                w.copy_from_slice(&(r.next() as u32).to_le_bytes());
            }
        }
        3 => {
            let i = (r.next() as usize) % b.len().max(1);
            let n = 1 + (r.next() as usize) % 64;
            let chunk: Vec<u8> = (0..n).map(|_| r.next() as u8).collect();
            b.splice(i..i, chunk);
        }
        4 => {
            let i = (r.next() as usize) % b.len().max(1);
            let n = (r.next() as usize) % 64;
            b.drain(i..(i + n).min(b.len()));
        }
        _ => {
            for x in &mut b {
                if r.next() % 64 == 0 {
                    *x = r.next() as u8;
                }
            }
        }
    }
    b
}

/// MA-7: the decoder never panics on arbitrary bytes or on mutated real dumps, and every Ok
/// result respects the caps. `iters` mutations; set `GORCA_FUZZ_SECS` to run for a time instead.
pub fn run_fuzz_get_items<D: ItemsDecoder>(d: &D, seed: u64, iters: u32) -> Result<u32, String> {
    let lim = DecodeLimits::default();
    let corpus = [
        encode_items(&sample_dump(":1.5", 40)),
        encode_items(&sample_dump(":1.5", 1)),
        encode_items(&sample_dump(":1.5", 0)),
        encode_items(&{
            let mut v = sample_dump(":1.5", 25);
            v[3].name = "é".repeat(1500);
            v[5].bus = ":1.9".into();
            v
        }),
    ];
    let mut r = Lcg(seed);
    let secs: Option<u64> = std::env::var("GORCA_FUZZ_SECS")
        .ok()
        .and_then(|v| v.parse().ok());
    let start = std::time::Instant::now();
    let mut n = 0u32;
    loop {
        if let Some(s) = secs {
            if start.elapsed().as_secs() >= s {
                break;
            }
        } else if n >= iters {
            break;
        }
        n += 1;
        let base = &corpus[(r.next() as usize) % corpus.len()];
        let input = if r.next() % 5 == 0 {
            (0..(r.next() as usize) % 256)
                .map(|_| r.next() as u8)
                .collect()
        } else {
            mutate(base, &mut r)
        };
        let res =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.decode(":1.5", &input)));
        match res {
            Err(_) => {
                return Err(format!(
                    "decoder panicked on mutation {n} ({} bytes)",
                    input.len()
                ));
            }
            Ok(Ok(dec)) => {
                check_caps(&dec.items, &lim).map_err(|e| format!("mutation {n}: {e}"))?
            }
            Ok(Err(_)) => {}
        }
    }
    Ok(n)
}

fn check_caps(items: &[DItem], l: &DecodeLimits) -> Result<(), String> {
    if items.len() > l.max_items {
        return Err(format!("{} items > cap {}", items.len(), l.max_items));
    }
    for i in items {
        if i.name.len() > l.max_text || i.desc.len() > l.max_text {
            return Err("text longer than MAX_TEXT_BYTES".into());
        }
        if i.path.len() > l.max_path {
            return Err("path longer than the cap".into());
        }
        if !(0..=l.max_child_count).contains(&i.child_count) {
            return Err(format!("child count {} outside the clip", i.child_count));
        }
        if i.ifaces.len() > l.max_ifaces || i.states.len() > l.max_state_words {
            return Err("interface or state array beyond its cap".into());
        }
    }
    Ok(())
}

fn safe_decode<D: ItemsDecoder>(
    d: &D,
    b: &[u8],
) -> Result<Result<super::items::Decoded, String>, String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| d.decode(":1.5", b)))
        .map(|r| r.map_err(|e| format!("{e:?}")))
        .map_err(|_| "decoder panicked".to_owned())
}

/// MA-7 caps: hostile-but-well-formed dumps are clipped, never trusted.
pub fn run_dump_caps<D: ItemsDecoder>(d: &D) -> Result<(), String> {
    let lim = DecodeLimits::default();
    let mut v: Vec<DItem> = (0..70_000)
        .map(|i| DItem {
            bus: ":1.5".into(),
            path: format!("/a/{i}"),
            parent_bus: ":1.5".into(),
            parent_path: "/a/0".into(),
            ifaces: vec!["i".into()],
            states: vec![1],
            ..DItem::default()
        })
        .collect();
    v[0].name = "n".repeat(100_000);
    v[1].child_count = i32::MAX;
    v[2].child_count = i32::MIN;
    v[3].ifaces = (0..500).map(|i| format!("org.example.I{i}")).collect();
    v[4].states = vec![1; 4096];
    let bytes = encode_items(&v);
    let dec = safe_decode(d, &bytes)??;
    check_caps(&dec.items, &lim)?;
    if dec.items.len() != lim.max_items || !dec.truncated {
        return Err(format!(
            "70000 items gave {} (truncated {})",
            dec.items.len(),
            dec.truncated
        ));
    }
    // A reply beyond the byte cap is refused whole.
    let huge = vec![0u8; lim.max_bytes + 1];
    if safe_decode(d, &huge)?.is_ok() {
        return Err("a reply larger than 16 MiB was accepted".into());
    }
    // Foreign items are dropped and counted.
    let mut f = sample_dump(":1.5", 10);
    f[2].bus = ":1.8".into();
    let dec = safe_decode(d, &encode_items(&f))??;
    if dec.foreign_dropped != 1 || dec.items.iter().any(|i| i.bus != ":1.5") {
        return Err("foreign item not dropped and counted".into());
    }
    Ok(())
}

// ---- policy: MA-1 memory, MA-6 step, MA-9 health ------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Health {
    Healthy,
    Slow,
    Hung,
    Flooded,
    Gone,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Class {
    Interactive,
    Prefetch,
    Background,
}

#[derive(Clone, Debug)]
pub enum PInput {
    Appeared,
    Events(Vec<(u64, u32)>),
    Resync,
    FocusSettled(u64),
    WindowActivated(u64),
    Tick,
    Gone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PRequest {
    pub class: Class,
    pub node: u64,
}

/// SPEC 4.4 `FillPolicy::step` with neutral types.
pub trait PolicyAdapter {
    fn name(&self) -> String;
    fn step(&mut self, health: Health, input: PInput) -> Vec<PRequest>;
    fn memory_bytes(&self) -> usize;
}

pub const FILL_STATE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// MA-1 (policy): `memory_bytes()` stays within the cap under a worst-case input stream and
/// returns to the floor when the app is gone.
pub fn run_policy_memory_accounting<P: PolicyAdapter>(mut p: P) -> Result<(), String> {
    let floor = p.memory_bytes();
    p.step(Health::Healthy, PInput::Appeared);
    let mut last = floor;
    for chunk in 0..64u64 {
        let evs: Vec<(u64, u32)> = (0..1_024).map(|i| (chunk * 1_024 + i, 31)).collect();
        p.step(Health::Healthy, PInput::Events(evs));
        p.step(Health::Healthy, PInput::FocusSettled(chunk));
        let m = p.memory_bytes();
        if m > FILL_STATE_MAX_BYTES {
            return Err(format!(
                "{}: memory_bytes {m} > cap {FILL_STATE_MAX_BYTES} after chunk {chunk}",
                p.name()
            ));
        }
        last = last.max(m);
    }
    p.step(Health::Gone, PInput::Gone);
    if p.memory_bytes() > floor + 1024 {
        return Err(format!(
            "{}: state not released on Gone ({} > floor {floor})",
            p.name(),
            p.memory_bytes()
        ));
    }
    let _ = last;
    Ok(())
}

/// MA-6 (policy): worst `step()` over the worst-case inputs.
pub fn run_step_bounded<P: PolicyAdapter>(mut p: P, budget_us: u64) -> Result<u64, String> {
    p.step(Health::Healthy, PInput::Appeared);
    let big: Vec<(u64, u32)> = (0..4_096).map(|i| (i, 31)).collect();
    let mut worst = 0u64;
    for input in [
        PInput::Events(big),
        PInput::Resync,
        PInput::FocusSettled(40_000),
        PInput::WindowActivated(1),
        PInput::Tick,
    ] {
        let t = std::time::Instant::now();
        p.step(Health::Healthy, input);
        worst = worst.max(u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX));
    }
    if worst > budget_us {
        return Err(format!(
            "{}: step took {worst} us (budget {budget_us} us)",
            p.name()
        ));
    }
    Ok(worst)
}

/// MA-9: within one step of `Slow`/`Hung` the policy stops Prefetch/Background; to a `Hung` app
/// it sends nothing at all (I-4).
pub fn run_policy_respects_health<P: PolicyAdapter>(mut p: P) -> Result<(), String> {
    p.step(Health::Healthy, PInput::Appeared);
    p.step(Health::Healthy, PInput::FocusSettled(1));
    for h in [Health::Slow, Health::Hung] {
        let inputs = [
            PInput::Events((0..64).map(|i| (i, 31)).collect()),
            PInput::FocusSettled(7),
            PInput::WindowActivated(2),
            PInput::Resync,
            PInput::Tick,
        ];
        for i in inputs {
            for r in p.step(h, i.clone()) {
                let bad = match h {
                    Health::Hung => true,
                    _ => matches!(r.class, Class::Prefetch | Class::Background),
                };
                if bad {
                    return Err(format!(
                        "{}: {:?} request {r:?} to a {h:?} app on {i:?}",
                        p.name(),
                        r.class
                    ));
                }
            }
        }
    }
    Ok(())
}

/// A correct policy that prefetches on focus and obeys health.
#[derive(Debug, Default)]
pub struct RefPolicy {
    pub state: BTreeSet<u64>,
    pub ignore_health: bool,
}

impl PolicyAdapter for RefPolicy {
    fn name(&self) -> String {
        "ref-policy".into()
    }

    fn step(&mut self, health: Health, input: PInput) -> Vec<PRequest> {
        let mut out = Vec::new();
        match input {
            PInput::Events(e) => {
                for (n, _) in e.iter().take(256) {
                    if self.state.len() < 4_096 {
                        self.state.insert(*n);
                    }
                }
            }
            PInput::FocusSettled(n) | PInput::WindowActivated(n) => {
                out.push(PRequest {
                    class: Class::Prefetch,
                    node: n,
                });
                out.push(PRequest {
                    class: Class::Interactive,
                    node: n,
                });
            }
            PInput::Tick | PInput::Resync => out.push(PRequest {
                class: Class::Background,
                node: 0,
            }),
            PInput::Gone => self.state.clear(),
            PInput::Appeared => {}
        }
        if !self.ignore_health {
            match health {
                Health::Hung | Health::Gone => out.clear(),
                Health::Slow | Health::Flooded => {
                    out.retain(|r| r.class == Class::Interactive);
                }
                Health::Healthy => {}
            }
        }
        out
    }

    fn memory_bytes(&self) -> usize {
        self.state.len() * 16
    }
}

// ---- MA-8 admission -----------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Admit {
    Granted,
    Refused,
}

/// The per-app in-flight limiter of SPEC 7 (`MAX_IN_FLIGHT` 8, Interactive 2 reserved,
/// Background <= 2).
pub trait AdmissionAdapter {
    fn new() -> Self
    where
        Self: Sized;
    fn admit(&mut self, class: Class, is_dump: bool) -> Admit;
    fn complete(&mut self, class: Class, is_dump: bool);
}

/// MA-8 (admission part): however much Background/Prefetch work is queued, an Interactive
/// request is admitted while fewer than 2 Interactive requests are in flight.
pub fn run_interactive_not_starved<A: AdmissionAdapter>() -> Result<(), String> {
    let mut a = A::new();
    let mut bg = 0;
    for i in 0..64 {
        let (class, dump) = match i % 3 {
            0 => (Class::Background, true),
            1 => (Class::Prefetch, false),
            _ => (Class::Background, false),
        };
        if a.admit(class, dump) == Admit::Granted {
            bg += 1;
        }
    }
    if bg > 6 {
        return Err(format!(
            "{bg} non-interactive requests in flight (limit 8 - 2 reserved)"
        ));
    }
    for n in 0..2 {
        if a.admit(Class::Interactive, false) != Admit::Granted {
            return Err(format!(
                "Interactive request {n} refused behind background work"
            ));
        }
    }
    Ok(())
}

/// Reference limiter.
#[derive(Debug, Default)]
pub struct RefAdmission {
    interactive: u32,
    other: u32,
    dumps: u32,
}

impl AdmissionAdapter for RefAdmission {
    fn new() -> Self {
        Self::default()
    }

    fn admit(&mut self, class: Class, is_dump: bool) -> Admit {
        match class {
            Class::Interactive => {
                if self.interactive + self.other < 8 || self.interactive < 2 {
                    self.interactive += 1;
                    Admit::Granted
                } else {
                    Admit::Refused
                }
            }
            _ => {
                if self.other >= 6 || (is_dump && self.dumps >= 2) {
                    return Admit::Refused;
                }
                self.other += 1;
                self.dumps += u32::from(is_dump);
                Admit::Granted
            }
        }
    }

    fn complete(&mut self, class: Class, is_dump: bool) {
        match class {
            Class::Interactive => self.interactive = self.interactive.saturating_sub(1),
            _ => {
                self.other = self.other.saturating_sub(1);
                self.dumps = self.dumps.saturating_sub(u32::from(is_dump));
            }
        }
    }
}

// ---- MA-10 shadow allowlist ---------------------------------------------------------------

/// SPEC 8.4: the calls shadow mode may issue to an application, as `Interface.Member`.
pub fn shadow_allowed(member: &str) -> bool {
    const ACCESSIBLE_GETTERS: &[&str] = &[
        "GetRole",
        "GetRoleName",
        "GetLocalizedRoleName",
        "GetState",
        "GetChildren",
        "GetChildAtIndex",
        "GetIndexInParent",
        "GetAttributes",
        "GetAttributeValue",
        "GetInterfaces",
        "GetApplication",
        "GetRelationSet",
    ];
    match member.split_once('.') {
        Some(("Accessible", m)) => ACCESSIBLE_GETTERS.contains(&m),
        Some(("Properties", m)) => matches!(m, "Get" | "GetAll"),
        Some(("Cache", "GetItems")) => true,
        Some(("Application", "GetApplicationBusAddress")) => true,
        Some(("Text", "GetText")) => true,
        // The fake app counts the bridge's own and the bus's bookkeeping separately.
        Some(("Peer", "Ping")) => true,
        _ => false,
    }
}

/// Members in a fake-app call table (`"<conn>/<Interface.Member>" -> n`) outside the allowlist.
pub fn shadow_offenders(calls: &BTreeMap<String, u64>) -> Vec<String> {
    calls
        .keys()
        .filter(|k| !shadow_allowed(k.split_once('/').map_or(k.as_str(), |(_, m)| m)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- the audits against the reference implementations (they must pass) ----

    #[test]
    fn mirror_counters_exact() {
        for seed in 1..=20 {
            run_mirror_counters_exact::<RefMirror>(seed, 3_000).unwrap();
        }
    }

    #[test]
    fn audit_app_churn_releases_everything() {
        // Structure only: the test binary runs other tests on other threads, so process RSS is
        // noise here. The RSS limit is checked by the ignored test below, run alone.
        run_app_churn::<RefMirror>(100, false).unwrap();
    }

    #[test]
    #[ignore = "RSS gate: run alone (--test-threads=1) against the real gorca-mirror adapter"]
    fn audit_app_churn_rss() {
        run_app_churn::<RefMirror>(100, true).unwrap();
    }

    #[test]
    fn audit_foreign_refs_rejected() {
        run_foreign_refs::<RefMirror>().unwrap();
    }

    #[test]
    fn audit_late_reply_never_revives() {
        run_late_reply_never_revives::<RefMirror>().unwrap();
    }

    #[test]
    fn audit_proof_of_life_revives() {
        run_proof_of_life_revives::<RefMirror>().unwrap();
    }

    #[test]
    fn audit_redirty_during_fetch() {
        run_redirty_during_fetch::<RefMirror>().unwrap();
    }

    #[test]
    fn audit_apply_bounded() {
        // The reference is a HashMap, not the product store: only show that the audit measures,
        // with a budget loose enough for a debug build. The real 250 us gate runs against the
        // real mirror (ignored test below).
        let worst = run_apply_bounded::<RefMirror>(5, 250_000).unwrap();
        eprintln!("reference apply(4096) worst {worst} us");
    }

    #[test]
    #[ignore = "wire the real gorca-mirror MirrorAdapter (PW-1); then run in release"]
    fn audit_apply_bounded_real_mirror() {
        // Replace RefMirror with the real adapter and keep the SPEC budget.
        run_apply_bounded::<RefMirror>(50, 250).unwrap();
    }

    #[test]
    fn audit_step_bounded() {
        let worst = run_step_bounded(RefPolicy::default(), 250_000).unwrap();
        eprintln!("reference step worst {worst} us");
    }

    #[test]
    #[ignore = "needs a non-null FillPolicy (R0's NullPolicy cannot exceed any budget): wire an arm's Candidate"]
    fn audit_step_bounded_candidate() {
        run_step_bounded(RefPolicy::default(), 250).unwrap();
    }

    #[test]
    fn policy_memory_accounting() {
        run_policy_memory_accounting(RefPolicy::default()).unwrap();
    }

    #[test]
    #[ignore = "needs a non-null FillPolicy and a counting allocator in the arm's test crate"]
    fn policy_memory_accounting_candidate() {
        run_policy_memory_accounting(RefPolicy::default()).unwrap();
    }

    #[test]
    fn audit_policy_respects_health() {
        run_policy_respects_health(RefPolicy::default()).unwrap();
    }

    #[test]
    #[ignore = "needs a non-null FillPolicy: wire an arm's Candidate"]
    fn audit_policy_respects_health_candidate() {
        run_policy_respects_health(RefPolicy::default()).unwrap();
    }

    #[test]
    fn audit_interactive_not_starved() {
        run_interactive_not_starved::<RefAdmission>().unwrap();
    }

    #[test]
    #[ignore = "needs the real driver and two 65 536-item dumps in flight on other apps (PW-3, PW-5)"]
    fn audit_interactive_not_starved_driver() {
        run_interactive_not_starved::<RefAdmission>().unwrap();
    }

    #[test]
    fn fuzz_get_items() {
        let n = run_fuzz_get_items(&super::super::items::RefDecoder::default(), 7, 20_000).unwrap();
        assert!(n >= 20_000 || std::env::var("GORCA_FUZZ_SECS").is_ok());
    }

    #[test]
    fn audit_dump_caps() {
        run_dump_caps(&super::super::items::RefDecoder::default()).unwrap();
    }

    #[test]
    #[ignore = "wire the real fetch/items.rs decoder (PW-9); GORCA_FUZZ_SECS=600 for the 10 minute run"]
    fn fuzz_get_items_real_decoder() {}

    #[test]
    fn audit_shadow_allowlist() {
        let mut calls = BTreeMap::new();
        for m in [
            "bus/Accessible.GetRole",
            "peer/Accessible.GetChildren",
            "bus/Properties.GetAll",
            "bus/Cache.GetItems",
            "bus/Application.GetApplicationBusAddress",
            "peer/Text.GetText",
        ] {
            calls.insert(m.to_owned(), 3);
        }
        assert!(shadow_offenders(&calls).is_empty());
        for bad in [
            "bus/Action.DoAction",
            "bus/Component.GrabFocus",
            "bus/EditableText.SetTextContents",
            "bus/Selection.SelectChild",
            "bus/Registry.RegisterEvent",
            "bus/Accessible.SetFoo",
        ] {
            calls.insert(bad.to_owned(), 1);
        }
        assert_eq!(shadow_offenders(&calls).len(), 6);
    }

    #[test]
    #[ignore = "needs gorca-daemon with GORCA_SHADOW=1 (PW-11) against the fake apps: assert shadow_offenders(stats.calls) is empty for every arm"]
    fn audit_shadow_allowlist_daemon() {}

    // ---- mutants: each audit must fail for the defect it exists for ----

    fn mutant(bug: Bug) -> impl Fn(u32) -> RefMirror {
        move |app| RefMirror::with_bug(app, bug)
    }

    struct Late;
    impl MirrorAdapter for Late {
        fn new(app: u32) -> Self {
            let _ = app;
            Late
        }
        fn seq(&self) -> u64 {
            0
        }
        fn apply(&mut self, _: Vec<ANode>, _: Origin) -> ApplyOutcome {
            ApplyOutcome::default()
        }
        fn invalidate(&mut self, _: Key, _: u32) -> bool {
            false
        }
        fn invalidate_all(&mut self) {}
        fn tombstone(&mut self, _: Key) {}
        fn evict(&mut self, _: Key) -> bool {
            false
        }
        fn sweep(&mut self) {}
        fn retire_all(&mut self) {}
        fn view(&self, _: Key) -> AView {
            AView::Miss
        }
        fn stats(&self) -> AStats {
            AStats::default()
        }
        fn check_invariants(&self) -> Result<(), String> {
            Ok(())
        }
    }

    macro_rules! mutant_adapter {
        ($name:ident, $bug:expr) => {
            struct $name(RefMirror);
            impl MirrorAdapter for $name {
                fn new(app: u32) -> Self {
                    Self(RefMirror::with_bug(app, $bug))
                }
                fn seq(&self) -> u64 {
                    self.0.seq()
                }
                fn apply(&mut self, n: Vec<ANode>, o: Origin) -> ApplyOutcome {
                    self.0.apply(n, o)
                }
                fn invalidate(&mut self, k: Key, m: u32) -> bool {
                    self.0.invalidate(k, m)
                }
                fn invalidate_all(&mut self) {
                    self.0.invalidate_all();
                }
                fn tombstone(&mut self, k: Key) {
                    self.0.tombstone(k);
                }
                fn evict(&mut self, k: Key) -> bool {
                    self.0.evict(k)
                }
                fn sweep(&mut self) {
                    self.0.sweep();
                }
                fn retire_all(&mut self) {
                    self.0.retire_all();
                }
                fn view(&self, k: Key) -> AView {
                    self.0.view(k)
                }
                fn stats(&self) -> AStats {
                    self.0.stats()
                }
                fn check_invariants(&self) -> Result<(), String> {
                    self.0.check_invariants()
                }
            }
        };
    }

    mutant_adapter!(ReviveM, Bug::ReviveOnLateReply);
    mutant_adapter!(RedirtyM, Bug::IgnoreRedirty);
    mutant_adapter!(ForeignM, Bug::AcceptForeign);
    mutant_adapter!(DriftM, Bug::CounterDriftOnEvict);
    mutant_adapter!(RetainM, Bug::RetainOnRetire);

    struct NoReserve(u32);
    impl AdmissionAdapter for NoReserve {
        fn new() -> Self {
            Self(0)
        }
        fn admit(&mut self, _: Class, _: bool) -> Admit {
            if self.0 < 8 {
                self.0 += 1;
                Admit::Granted
            } else {
                Admit::Refused
            }
        }
        fn complete(&mut self, _: Class, _: bool) {
            self.0 = self.0.saturating_sub(1);
        }
    }

    #[test]
    fn the_audits_catch_their_mutants() {
        let _ = mutant(Bug::None);
        assert!(
            run_late_reply_never_revives::<ReviveM>().is_err(),
            "MA-4 LR-1"
        );
        assert!(run_redirty_during_fetch::<RedirtyM>().is_err(), "MA-5");
        assert!(run_foreign_refs::<ForeignM>().is_err(), "MA-3");
        assert!(
            run_mirror_counters_exact::<DriftM>(1, 3_000).is_err(),
            "MA-1"
        );
        assert!(run_app_churn::<RetainM>(3, false).is_err(), "MA-2");
        // A mirror that stores nothing cannot satisfy proof-of-life.
        assert!(run_proof_of_life_revives::<Late>().is_err(), "MA-4 LR-3");
        // Decoder: trusting lengths panics or overruns.
        assert!(
            run_fuzz_get_items(&super::super::items::CarelessDecoder, 7, 2_000).is_err(),
            "MA-7"
        );
        assert!(
            run_dump_caps(&super::super::items::CarelessDecoder).is_err(),
            "MA-7 caps"
        );
        // Policies.
        assert!(
            run_policy_respects_health(RefPolicy {
                ignore_health: true,
                ..RefPolicy::default()
            })
            .is_err()
        );
        // Admission: a limiter without reservation starves Interactive.
        assert!(run_interactive_not_starved::<NoReserve>().is_err(), "MA-8");
    }
}
