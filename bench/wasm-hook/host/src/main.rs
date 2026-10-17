// SPDX-License-Identifier: GPL-3.0-or-later
//! ADR-0008 micro-benchmark: wasmtime component-model call overhead for per-engine web quirk
//! hooks, against the same logic called natively (direct and behind `catch_unwind`).
//!
//! Usage: quirk-host <guest.wasm> [iters]
//! Output: one TSV row per case on stdout (case, n, p50_ns, p99_ns, p999_ns, max_ns, mean_ns).

use std::hint::black_box;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use quirk_logic as q;
use wasmtime::component::{Component, HasSelf, InstancePre, Linker};
use wasmtime::error::Context;
use wasmtime::{Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store};
use wasmtime::{Result, bail, format_err};

wasmtime::component::bindgen!({ world: "quirk", path: "../wit" });

const SIZES: [usize; 6] = [1, 8, 32, 128, 512, 2048];
const WALKS: [u32; 4] = [1, 10, 100, 1000];

/// Stand-in for the lock-free mirror: answers from a computed node (name string included, so the
/// lift/lower of a realistic record is measured).
struct Mirror;

fn mirror_node(key: (u64, u64)) -> q_node::N {
    q_node::N {
        key,
        role: if key.1.is_multiple_of(7) { 0x2a } else { 0x15 },
        states: if key.1.is_multiple_of(3) { 0x10 } else { 0 },
        name: "Link: product details".to_string(),
    }
}

mod q_node {
    pub struct N {
        pub key: (u64, u64),
        pub role: u8,
        pub states: u64,
        pub name: String,
    }
}

impl QuirkImports for Mirror {
    fn get_node(&mut self, key: (u64, u64)) -> Option<Node> {
        let n = mirror_node(key);
        Some(Node {
            key: n.key,
            role: n.role,
            states: n.states,
            name: Some(n.name),
        })
    }
}

fn native_walk(start: (u64, u64), k: u32) -> u32 {
    let mut hits = 0;
    let mut key = start;
    for _ in 0..k {
        let n = mirror_node(black_box(key));
        if q::on_key(0x41, n.role, n.states).is_some() {
            hits += 1;
        }
        key = (n.key.0, n.key.1 + 1);
    }
    hits
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// Event mix modelled on a page-update burst: 25 % text-inserted (24 B payload, half UTF-16
/// flagged), 15 % caret, 20 % children-added (a third static text), 10 % reorder, rest attrs/state,
/// 2 % index -1.
fn make_mix(n: usize, seed: u64) -> Vec<q::Delta> {
    let mut r = Rng(seed | 1);
    (0..n)
        .map(|i| {
            let roll = r.next() % 100;
            let node = (7, i as u64 + 1000);
            let parent = (7, i as u64 / 4 + 10);
            let (kind, text, flags) = match roll {
                0..=24 => (
                    q::Kind::TextInserted,
                    Some("Добавлено: новый элемент 😀".to_string()),
                    if roll.is_multiple_of(2) {
                        q::F_UTF16
                    } else {
                        0
                    },
                ),
                25..=39 => (
                    q::Kind::CaretMoved,
                    None,
                    if roll.is_multiple_of(3) {
                        q::F_FOCUSED
                    } else {
                        0
                    },
                ),
                40..=59 => (
                    q::Kind::ChildrenAdded,
                    None,
                    if roll.is_multiple_of(3) {
                        q::F_STATIC_TEXT
                    } else {
                        0
                    },
                ),
                60..=69 => (q::Kind::Reorder, None, 0),
                70..=84 => (q::Kind::AttrsChanged, None, 0),
                _ => (q::Kind::StateChanged, None, 0),
            };
            let a = if roll == 99 {
                -1
            } else {
                (r.next() % 64) as i32
            };
            let b = a + (r.next() % 8) as i32;
            q::Delta {
                kind,
                node,
                parent,
                a,
                b,
                flags,
                text,
            }
        })
        .collect()
}

fn to_wit(d: &q::Delta) -> Delta {
    Delta {
        kind: match d.kind {
            q::Kind::ChildrenAdded => DeltaKind::ChildrenAdded,
            q::Kind::ChildrenRemoved => DeltaKind::ChildrenRemoved,
            q::Kind::TextInserted => DeltaKind::TextInserted,
            q::Kind::TextRemoved => DeltaKind::TextRemoved,
            q::Kind::CaretMoved => DeltaKind::CaretMoved,
            q::Kind::AttrsChanged => DeltaKind::AttrsChanged,
            q::Kind::StateChanged => DeltaKind::StateChanged,
            q::Kind::Reorder => DeltaKind::Reorder,
        },
        node: d.node,
        parent: d.parent,
        a: d.a,
        b: d.b,
        flags: d.flags,
        text: d.text.clone(),
    }
}

struct Stats {
    p50: u64,
    p99: u64,
    p999: u64,
    max: u64,
    mean: f64,
}

fn stats(mut v: Vec<u64>) -> Stats {
    v.sort_unstable();
    let pct = |p: f64| v[((v.len() as f64 - 1.0) * p) as usize];
    let mean = v.iter().sum::<u64>() as f64 / v.len() as f64;
    Stats {
        p50: pct(0.50),
        p99: pct(0.99),
        p999: pct(0.999),
        max: v[v.len() - 1],
        mean,
    }
}

fn row(case: &str, n: usize, s: &Stats) {
    println!(
        "{case}\t{n}\t{}\t{}\t{}\t{}\t{:.0}",
        s.p50, s.p99, s.p999, s.max, s.mean
    );
}

fn time<F: FnMut() -> Result<()>>(iters: usize, mut f: F) -> Result<Stats> {
    for _ in 0..iters / 10 + 10 {
        f()?;
    }
    let mut v = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        f()?;
        v.push(t.elapsed().as_nanos() as u64);
    }
    Ok(stats(v))
}

#[derive(Clone, Copy)]
enum Mode {
    Epoch,
    Fuel,
    PoolingEpoch,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Mode::Epoch => "epoch",
            Mode::Fuel => "fuel",
            Mode::PoolingEpoch => "pool+epoch",
        }
    }
}

fn engine(mode: Mode) -> Result<Engine> {
    let mut c = Config::new();
    c.wasm_component_model(true);
    match mode {
        Mode::Epoch => {
            c.epoch_interruption(true);
        }
        Mode::Fuel => {
            c.consume_fuel(true);
        }
        Mode::PoolingEpoch => {
            c.epoch_interruption(true);
            let mut p = PoolingAllocationConfig::default();
            p.total_component_instances(64);
            p.total_memories(64);
            p.total_tables(64);
            p.max_memory_size(32 << 20);
            c.allocation_strategy(InstanceAllocationStrategy::Pooling(p));
        }
    }
    Engine::new(&c)
}

fn new_store(e: &Engine, mode: Mode) -> Result<Store<Mirror>> {
    let mut s = Store::new(e, Mirror);
    match mode {
        Mode::Epoch | Mode::PoolingEpoch => s.set_epoch_deadline(u64::MAX / 2),
        Mode::Fuel => s.set_fuel(u64::MAX / 2)?,
    }
    Ok(s)
}

fn run_mode(wasm: &[u8], mode: Mode, iters: usize) -> Result<()> {
    let m = mode.name();
    let e = engine(mode)?;
    let t = Instant::now();
    let comp = Component::new(&e, wasm)?;
    eprintln!("[{m}] compile {:?}", t.elapsed());
    let mut linker: Linker<Mirror> = Linker::new(&e);
    Quirk::add_to_linker::<_, HasSelf<_>>(&mut linker, |s| s)?;
    linker.define_unknown_imports_as_traps(&comp)?;
    let pre: InstancePre<Mirror> = linker.instantiate_pre(&comp)?;
    let qpre = QuirkPre::new(pre.clone())?;

    let mut store = new_store(&e, mode)?;
    let inst = qpre.instantiate(&mut store)?;

    let s = time(iters, || {
        inst.call_noop(&mut store)?;
        Ok(())
    })?;
    row(&format!("wasm-{m}-noop"), 0, &s);

    let node = Node {
        key: (7, 1234),
        role: 0x2a,
        states: 0x10,
        name: Some("Search results for gorca".into()),
    };
    let s = time(iters, || {
        black_box(inst.call_on_key(&mut store, 0x48, &node)?);
        Ok(())
    })?;
    row(&format!("wasm-{m}-on_key"), 1, &s);
    let s = time(iters, || {
        black_box(inst.call_present(&mut store, &node)?);
        Ok(())
    })?;
    row(&format!("wasm-{m}-present"), 1, &s);
    for &k in &WALKS {
        let it = (iters * 10 / k.max(10) as usize).max(2000);
        let s = time(it, || {
            black_box(inst.call_walk(&mut store, (7, 1), k)?);
            Ok(())
        })?;
        row(&format!("wasm-{m}-walk_host_get_node"), k as usize, &s);
    }

    for &n in &SIZES {
        let it = (iters * 8 / n.max(8)).max(2000);
        for (eng, ename) in [(q::ENGINE_GECKO, "gecko"), (q::ENGINE_BLINK, "blink")] {
            let mix: Vec<Delta> = make_mix(n, 0x9e37 + n as u64).iter().map(to_wit).collect();
            let s = time(it, || {
                black_box(inst.call_normalize(&mut store, eng, &mix)?);
                Ok(())
            })?;
            row(&format!("wasm-{m}-normalize-{ename}"), n, &s);
            let s = time(it, || {
                black_box(inst.call_classify(&mut store, eng, &mix)?);
                Ok(())
            })?;
            row(&format!("wasm-{m}-classify-{ename}"), n, &s);
        }
    }

    // Trap recovery: trap -> fresh store + instance -> first successful call.
    let mut v = Vec::with_capacity(2000);
    for _ in 0..2000 {
        let mut s2 = new_store(&e, mode)?;
        let i2 = qpre.instantiate(&mut s2)?;
        let r = i2.call_trap(&mut s2);
        if r.is_ok() {
            bail!("trap export returned Ok");
        }
        let t = Instant::now();
        drop(s2);
        let mut s3 = new_store(&e, mode)?;
        let i3 = qpre.instantiate(&mut s3)?;
        i3.call_noop(&mut s3)?;
        v.push(t.elapsed().as_nanos() as u64);
    }
    row(&format!("wasm-{m}-trap_recovery"), 0, &stats(v));

    // Hang interruption latency: time from the deadline to the trap surfacing on the caller.
    match mode {
        Mode::Epoch | Mode::PoolingEpoch => {
            let stop = Arc::new(AtomicBool::new(false));
            let e2 = e.clone();
            let st = stop.clone();
            // A dedicated OS thread, not async code: sleeping here is the epoch tick.
            #[allow(clippy::disallowed_methods)]
            let ticker = std::thread::spawn(move || {
                while !st.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_micros(500));
                    e2.increment_epoch();
                }
            });
            let mut v = Vec::with_capacity(200);
            for _ in 0..200 {
                let mut s2 = new_store(&e, mode)?;
                // 4 ticks of 0.5 ms = 2 ms budget (ADR-0001 §10 on-event limit).
                s2.set_epoch_deadline(4);
                let i2 = qpre.instantiate(&mut s2)?;
                let t = Instant::now();
                let r = i2.call_spin(&mut s2);
                if r.is_ok() {
                    bail!("spin returned Ok");
                }
                v.push(t.elapsed().as_nanos() as u64);
            }
            stop.store(true, Ordering::Relaxed);
            ticker.join().map_err(|_| format_err!("ticker panicked"))?;
            row(&format!("wasm-{m}-spin_to_trap_2ms_budget"), 0, &stats(v));
        }
        Mode::Fuel => {}
    }
    Ok(())
}

fn run_native(iters: usize) -> Result<()> {
    let s = time(iters, || {
        black_box(q::on_key(black_box(0x48), black_box(0x2a), black_box(0x10)));
        Ok(())
    })?;
    row("native-on_key", 1, &s);
    for &k in &WALKS {
        let it = (iters * 10 / k.max(10) as usize).max(2000);
        let s = time(it, || {
            black_box(native_walk((7, 1), k));
            Ok(())
        })?;
        row("native-walk_get_node", k as usize, &s);
    }
    for &n in &SIZES {
        let it = (iters * 8 / n.max(8)).max(2000);
        for (eng, ename) in [(q::ENGINE_GECKO, "gecko"), (q::ENGINE_BLINK, "blink")] {
            let mix = make_mix(n, 0x9e37 + n as u64);
            let s = time(it, || {
                black_box(q::normalize(eng, black_box(mix.clone())));
                Ok(())
            })?;
            row(&format!("native-normalize-clone-{ename}"), n, &s);
            let s = time(it, || {
                let r = catch_unwind(AssertUnwindSafe(|| {
                    q::normalize(eng, black_box(mix.clone()))
                }));
                black_box(r.map_err(|_| format_err!("panic"))?);
                Ok(())
            })?;
            row(
                &format!("native-normalize-clone-catchunwind-{ename}"),
                n,
                &s,
            );
            // In place: what a native first-party hook over `&mut [Delta]` costs (no copy).
            let mut work = mix.clone();
            let mut v = Vec::with_capacity(it);
            for _ in 0..it {
                work.clone_from(&mix);
                let t = Instant::now();
                let mut acc = 0u32;
                for d in black_box(&mut work).iter_mut() {
                    acc += q::fix(eng, d) as u32;
                }
                black_box(acc);
                v.push(t.elapsed().as_nanos() as u64);
            }
            row(&format!("native-fix-inplace-{ename}"), n, &stats(v));
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .context("usage: quirk-host <guest.wasm> [iters]")?;
    let iters: usize = match args.next() {
        Some(s) => s.parse().map_err(|e| format_err!("bad iters: {e}"))?,
        None => 200_000,
    };
    let wasm = std::fs::read(&path).with_context(|| format!("read {path}"))?;
    println!("case\tn\tp50_ns\tp99_ns\tp999_ns\tmax_ns\tmean_ns");
    run_native(iters)?;
    for mode in [Mode::Epoch, Mode::Fuel, Mode::PoolingEpoch] {
        run_mode(&wasm, mode, iters)?;
    }
    Ok(())
}
