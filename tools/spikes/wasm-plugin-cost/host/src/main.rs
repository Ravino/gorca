//! Throwaway measurement spike for gorca's WASM plugin cost (M18a). See
//! docs/reports/wasm-plugin-cost-spike.md. Usage: `wasm-plugin-cost <guest.wasm> <section>`
//! with section in: load, calls, safety, async, hot, mem <ondemand|pooling> <min|1mb>.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use wasmtime::component::{Component, HasSelf, Linker};
use wasmtime::{
    Config, Engine, InstanceAllocationStrategy, PoolingAllocationConfig, Store, StoreLimits,
    StoreLimitsBuilder, Trap,
};

wasmtime::component::bindgen!({ path: "../wit", world: "plugin" });

struct St {
    limits: StoreLimits,
    pings: u64,
}
impl spike::plugin::types::Host for St {}
impl spike::plugin::hostapi::Host for St {
    fn ping(&mut self) -> u32 {
        self.pings += 1;
        1
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Opts {
    pooling: bool,
    fuel: bool,
    epoch: bool,
}

fn engine(o: Opts) -> Engine {
    let mut c = Config::new();
    c.consume_fuel(o.fuel);
    c.epoch_interruption(o.epoch);
    if o.pooling {
        let mut p = PoolingAllocationConfig::default();
        p.total_component_instances(512);
        p.total_core_instances(512);
        p.total_memories(512);
        p.total_tables(512);
        p.max_memory_size(32 << 20);
        c.allocation_strategy(InstanceAllocationStrategy::Pooling(p));
    }
    Engine::new(&c).expect("engine")
}

fn linker(e: &Engine, c: &Component) -> Linker<St> {
    let mut l = Linker::new(e);
    Plugin::add_to_linker::<St, HasSelf<St>>(&mut l, |s| s).expect("linker");
    l.define_unknown_imports_as_traps(c).expect("traps");
    l
}

fn store(e: &Engine, o: Opts) -> Store<St> {
    let mut s = Store::new(
        e,
        St { limits: StoreLimitsBuilder::new().memory_size(32 << 20).build(), pings: 0 },
    );
    s.limiter(|s| &mut s.limits);
    if o.fuel {
        s.set_fuel(u64::MAX / 2).expect("fuel");
    }
    if o.epoch {
        s.set_epoch_deadline(u64::MAX / 2);
    }
    s
}

fn pct(v: &mut [u64]) -> (u64, u64, u64) {
    v.sort_unstable();
    let n = v.len();
    (v[n / 2], v[(n * 99) / 100], v[n - 1])
}

fn bench<F: FnMut()>(label: &str, n: usize, mut f: F) {
    for _ in 0..n / 10 {
        f();
    }
    let mut v = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        f();
        v.push(t.elapsed().as_nanos() as u64);
    }
    let (p50, p99, max) = pct(&mut v);
    println!("{label:<34} p50={:>8} ns  p99={:>8} ns  max={:>9} ns", p50, p99, max);
}

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/statm").unwrap_or_default();
    let pages: u64 = s.split_whitespace().nth(1).and_then(|x| x.parse().ok()).unwrap_or(0);
    pages * 4
}

fn rec10() -> Rec10 {
    let s = || "x".repeat(20);
    Rec10 { a: s(), b: s(), c: s(), d: s(), e: s(), f: s(), g: s(), h: s(), i: s(), j: s() }
}

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let path = &a[1];
    let sec = a[2].as_str();
    let bytes = std::fs::read(path).expect("read wasm");
    println!("# section={sec} wasm_bytes={}", bytes.len());
    match sec {
        "load" => load(&bytes),
        "calls" => calls(&bytes),
        "safety" => safety(&bytes),
        "hot" => hot(&bytes),
        "async" => asyn(&bytes),
        "mem" => mem(&bytes, &a[3], &a[4]),
        _ => eprintln!("unknown section"),
    }
}

fn load(bytes: &[u8]) {
    let o = Opts { pooling: false, fuel: false, epoch: false };
    for _ in 0..3 {
        let e = engine(o);
        let t = Instant::now();
        let c = Component::new(&e, bytes).expect("compile");
        let compile = t.elapsed();
        let t = Instant::now();
        let ser = c.serialize().expect("ser");
        let ser_t = t.elapsed();
        let t = Instant::now();
        let c2 = unsafe { Component::deserialize(&e, &ser).expect("deser") };
        let deser = t.elapsed();
        let l = linker(&e, &c2);
        let t = Instant::now();
        let pre = PluginPre::new(l.instantiate_pre(&c2).expect("pre")).expect("pre2");
        let pre_t = t.elapsed();
        let mut v = Vec::new();
        for _ in 0..2000 {
            let mut s = store(&e, o);
            let t = Instant::now();
            let _i = pre.instantiate(&mut s).expect("inst");
            v.push(t.elapsed().as_nanos() as u64);
        }
        let (p50, p99, _) = pct(&mut v);
        // Instantiate + Store creation + drop, pooled.
        let op = Opts { pooling: true, ..o };
        let ep = engine(op);
        let cp = unsafe { Component::deserialize(&ep, &ser).expect("deser") };
        let prep = PluginPre::new(linker(&ep, &cp).instantiate_pre(&cp).expect("p")).expect("p2");
        let mut vp = Vec::new();
        for _ in 0..2000 {
            let mut s = store(&ep, op);
            let t = Instant::now();
            let _i = prep.instantiate(&mut s).expect("inst");
            vp.push(t.elapsed().as_nanos() as u64);
        }
        let (pp50, pp99, _) = pct(&mut vp);
        println!(
            "compile={:.1}ms cwasm_bytes={} serialize={:.1}ms deserialize={:.1}ms instantiate_pre={:.0}us | instantiate on-demand p50={}us p99={}us | pooled p50={}us p99={}us",
            compile.as_secs_f64() * 1e3,
            ser.len(),
            ser_t.as_secs_f64() * 1e3,
            deser.as_secs_f64() * 1e3,
            pre_t.as_secs_f64() * 1e6,
            p50 / 1000,
            p99 / 1000,
            pp50 / 1000,
            pp99 / 1000
        );
    }
}

fn calls(bytes: &[u8]) {
    let o = Opts { pooling: true, fuel: true, epoch: true };
    let e = engine(o);
    let c = Component::new(&e, bytes).expect("compile");
    let pre = PluginPre::new(linker(&e, &c).instantiate_pre(&c).expect("pre")).expect("pre2");
    let mut s = store(&e, o);
    let p = pre.instantiate(&mut s).expect("inst");
    let r = rec10();
    let total: usize = 200;
    assert_eq!(total, r.a.len() * 10);
    for _ in 0..3 {
        bench("noop()", 200_000, || {
            std::hint::black_box(p.call_noop(&mut s).expect("c"));
        });
        bench("ten(record 10 str, 200 B)", 100_000, || {
            std::hint::black_box(p.call_ten(&mut s, &r).expect("c"));
        });
        bench("list20() -> 20 records", 100_000, || {
            let v: Vec<Item> = p.call_list20(&mut s).expect("c");
            std::hint::black_box(v);
        });
        // guest->host: 1000 pings per outer call; per-ping = (call(1000) - call(0)) / 1000.
        let mut v0 = Vec::new();
        let mut v1 = Vec::new();
        for _ in 0..2000 {
            let t = Instant::now();
            p.call_call_host(&mut s, 0).expect("c");
            v0.push(t.elapsed().as_nanos() as u64);
            let t = Instant::now();
            p.call_call_host(&mut s, 1000).expect("c");
            v1.push(t.elapsed().as_nanos() as u64);
        }
        let (a50, _, _) = pct(&mut v0);
        let (b50, b99, _) = pct(&mut v1);
        println!(
            "guest->host ping: per-call ~{} ns (p50 of 1000-call batch {} ns, base {} ns, batch p99 {} ns)",
            (b50 - a50) / 1000, b50, a50, b99
        );
        println!("--");
    }
}

fn hot(bytes: &[u8]) {
    // Fuel and epoch cost on a hot loop.
    let n: u64 = 200_000_000;
    let cfgs = [
        ("none", Opts { pooling: false, fuel: false, epoch: false }),
        ("fuel", Opts { pooling: false, fuel: true, epoch: false }),
        ("epoch", Opts { pooling: false, fuel: false, epoch: true }),
        ("fuel+epoch", Opts { pooling: false, fuel: true, epoch: true }),
    ];
    for run in 0..3 {
        let mut base = 0.0;
        for (name, o) in cfgs {
            let e = engine(o);
            let c = Component::new(&e, bytes).expect("compile");
            let pre = PluginPre::new(linker(&e, &c).instantiate_pre(&c).expect("p")).expect("p2");
            let mut best = f64::MAX;
            let mut s = store(&e, o);
            let p = pre.instantiate(&mut s).expect("inst");
            for _ in 0..5 {
                let t = Instant::now();
                std::hint::black_box(p.call_spin(&mut s, n).expect("spin"));
                best = best.min(t.elapsed().as_secs_f64());
            }
            if name == "none" {
                base = best;
            }
            println!(
                "run{run} {name:<11} spin({n}) best-of-5 = {:.1} ms  ({:+.1}% vs none)",
                best * 1e3,
                (best / base - 1.0) * 100.0
            );
        }
    }
}

fn safety(bytes: &[u8]) {
    let o = Opts { pooling: true, fuel: true, epoch: true };
    let e = engine(o);
    let c = Component::new(&e, bytes).expect("compile");
    let pre = PluginPre::new(linker(&e, &c).instantiate_pre(&c).expect("pre")).expect("pre2");
    let stop = Arc::new(AtomicBool::new(false));
    let (e2, st2) = (e.clone(), stop.clone());
    let ticker = std::thread::spawn(move || {
        while !st2.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(1));
            e2.increment_epoch();
        }
    });
    for run in 0..3 {
        // Epoch interrupt latency on an infinite loop.
        for (label, ticks) in [("deadline=1 tick", 1u64), ("deadline=5 ticks", 5)] {
            let mut v = Vec::new();
            for _ in 0..200 {
                let mut s = store(&e, o);
                s.set_fuel(u64::MAX / 2).expect("f");
                s.set_epoch_deadline(ticks);
                let p = pre.instantiate(&mut s).expect("inst");
                let t = Instant::now();
                let r = p.call_forever(&mut s);
                v.push(t.elapsed().as_micros() as u64);
                let err = r.expect_err("must trap");
                assert_eq!(err.downcast_ref::<Trap>(), Some(&Trap::Interrupt));
            }
            let (p50, p99, max) = pct(&mut v);
            println!("run{run} epoch(1ms tick) {label}: time-to-trap p50={p50} us p99={p99} us max={max} us");
        }
        // Fuel interrupt (no epoch involvement).
        let mut v = Vec::new();
        for _ in 0..200 {
            let mut s = store(&e, o);
            s.set_fuel(3_000_000).expect("f");
            let p = pre.instantiate(&mut s).expect("inst");
            let t = Instant::now();
            let err = p.call_forever(&mut s).expect_err("trap");
            v.push(t.elapsed().as_micros() as u64);
            assert_eq!(err.downcast_ref::<Trap>(), Some(&Trap::OutOfFuel));
        }
        let (p50, p99, max) = pct(&mut v);
        println!("run{run} fuel 3M units: time-to-trap p50={p50} us p99={p99} us max={max} us");
        // Trap and panic: host stays usable, recreate instance.
        let mut s = store(&e, o);
        let p = pre.instantiate(&mut s).expect("inst");
        let t = Instant::now();
        let r = p.call_trap(&mut s);
        let trap_t = t.elapsed();
        let msg = format!("{:?}", r.expect_err("trap").downcast_ref::<Trap>());
        let mut s2 = store(&e, o);
        let p2 = pre.instantiate(&mut s2).expect("inst");
        let t = Instant::now();
        let r = p2.call_do_panic(&mut s2);
        let panic_t = t.elapsed();
        let pmsg = format!("{:?}", r.expect_err("panic").downcast_ref::<Trap>());
        // After the trap the old store is poisoned for reentry; check, then recreate.
        let again = p.call_noop(&mut s);
        let t = Instant::now();
        let mut s3 = store(&e, o);
        let p3 = pre.instantiate(&mut s3).expect("recreate");
        let ok = p3.call_noop(&mut s3).expect("noop");
        let recreate = t.elapsed();
        println!(
            "run{run} trap: {msg} in {} us; panic(abort): {pmsg} in {} us; reuse-after-trap: {}; recreate+noop={} us (ok={ok})",
            trap_t.as_micros(),
            panic_t.as_micros(),
            if again.is_err() { "refused (error)" } else { "still works" },
            recreate.as_micros()
        );
    }
    stop.store(true, Ordering::Relaxed);
    let _ = ticker.join();
}

fn mem(bytes: &[u8], alloc: &str, state: &str) {
    let o = Opts { pooling: alloc == "pooling", fuel: true, epoch: true };
    let e = engine(o);
    let c = Component::new(&e, bytes).expect("compile");
    let pre = PluginPre::new(linker(&e, &c).instantiate_pre(&c).expect("pre")).expect("pre2");
    {
        let mut s = store(&e, o);
        let p = pre.instantiate(&mut s).expect("i");
        p.call_noop(&mut s).expect("c");
    }
    let n = 100;
    let base = rss_kb();
    let mut keep = Vec::new();
    let t = Instant::now();
    for _ in 0..n {
        let mut s = store(&e, o);
        let p = pre.instantiate(&mut s).expect("i");
        p.call_noop(&mut s).expect("c");
        if state == "1mb" {
            p.call_grow(&mut s, 1 << 20).expect("g");
        }
        keep.push((s, p));
    }
    let el = t.elapsed();
    let after = rss_kb();
    println!(
        "{alloc} {state}: {n} live instances, RSS delta={} KiB total => {:.1} KiB/instance (create+first-call avg {:.0} us)",
        after - base,
        (after - base) as f64 / n as f64,
        el.as_micros() as f64 / n as f64
    );
    drop(keep);
    println!("after drop: RSS delta vs base={} KiB", rss_kb() as i64 - base as i64);
}

fn asyn(bytes: &[u8]) {
    let o = Opts { pooling: true, fuel: false, epoch: false };
    let e = engine(o);
    let c = Component::new(&e, bytes).expect("compile");
    let l = linker(&e, &c);
    let pre = l.instantiate_pre(&c).expect("pre");
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(8).enable_all().build().expect("rt");
    rt.block_on(async {
        for run in 0..3 {
            println!("-- run{run}");
            // Latency, one task, sequential.
            let mut s = store(&e, o);
            let inst = pre.instantiate_async(&mut s).await.expect("i");
            let f = inst.get_typed_func::<(), (u32,)>(&mut s, "noop").expect("f");
            let mut v = Vec::new();
            for i in 0..110_000 {
                let t = Instant::now();
                std::hint::black_box(f.call_async(&mut s, ()).await.expect("c"));
                if i >= 10_000 {
                    v.push(t.elapsed().as_nanos() as u64);
                }
            }
            let (p50, p99, max) = pct(&mut v);
            println!("call_async noop (in task)          p50={p50} ns p99={p99} ns max={max} ns");
            let mut v = Vec::new();
            for _ in 0..100_000 {
                let t = Instant::now();
                std::hint::black_box(f.call(&mut s, ()).expect("c"));
                v.push(t.elapsed().as_nanos() as u64);
            }
            let (p50, p99, max) = pct(&mut v);
            println!("sync call noop inline (same store) p50={p50} ns p99={p99} ns max={max} ns");
            drop(s);
            // spawn_blocking round trip.
            let mut s = store(&e, o);
            let inst = pre.instantiate(&mut s).expect("i");
            let f = inst.get_typed_func::<(), (u32,)>(&mut s, "noop").expect("f");
            let mut slot = Some((s, f));
            let mut v = Vec::new();
            for i in 0..60_000 {
                let (mut s, f) = slot.take().expect("slot");
                let t = Instant::now();
                let (s, f) = tokio::task::spawn_blocking(move || {
                    std::hint::black_box(f.call(&mut s, ()).expect("c"));
                    (s, f)
                })
                .await
                .expect("join");
                if i >= 10_000 {
                    v.push(t.elapsed().as_nanos() as u64);
                }
                slot = Some((s, f));
            }
            let (p50, p99, max) = pct(&mut v);
            println!("sync via spawn_blocking round trip p50={p50} ns p99={p99} ns max={max} ns");
            drop(slot);

            // Throughput: 64 independent stores, 20k calls each, on 8 workers.
            let tasks = 64usize;
            let per = 20_000usize;
            let t = Instant::now();
            let mut hs = Vec::new();
            for _ in 0..tasks {
                let mut s = store(&e, o);
                let inst = pre.instantiate_async(&mut s).await.expect("i");
                let f = inst.get_typed_func::<(), (u32,)>(&mut s, "noop").expect("f");
                hs.push(tokio::spawn(async move {
                    for _ in 0..per {
                        f.call_async(&mut s, ()).await.expect("c");
                    }
                }));
            }
            for h in hs {
                h.await.expect("j");
            }
            let el = t.elapsed().as_secs_f64();
            println!("throughput call_async  {tasks} tasks: {:.2} M calls/s", (tasks * per) as f64 / el / 1e6);
            let t = Instant::now();
            let mut hs = Vec::new();
            for _ in 0..tasks {
                let mut s = store(&e, o);
                let inst = pre.instantiate(&mut s).expect("i");
                let f = inst.get_typed_func::<(), (u32,)>(&mut s, "noop").expect("f");
                hs.push(tokio::task::spawn_blocking(move || {
                    for _ in 0..per {
                        f.call(&mut s, ()).expect("c");
                    }
                }));
            }
            for h in hs {
                h.await.expect("j");
            }
            let el = t.elapsed().as_secs_f64();
            println!("throughput spawn_blocking (inner loop, no per-call hop) {tasks} tasks: {:.2} M calls/s", (tasks * per) as f64 / el / 1e6);
        }
    });
}
