//! m5-rereview probe (not for merge).
#![allow(clippy::pedantic, clippy::expect_used, clippy::unwrap_used, clippy::panic, clippy::disallowed_methods)]
mod common;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use common::{TestSink, app, info, settle, snap};
use gorca_core::{ExtSource, MirrorDirectory, PresentContext};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::Role;

const MANIFEST: &str = r#"
name = "ext-hostile2"
backend = "wasm"
module = "ext_hostile2.wasm"
hooks = ["on-event", "present"]
capabilities = ["settings", "speak"]
[matcher]
app_name = ["*"]
"#;

fn host() -> ExtHost {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/ext-wasm/wasm32-wasip2/release/ext_hostile2.wasm");
    let bytes = std::fs::read(p).unwrap();
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    b.add_wasm(Manifest::parse(MANIFEST).unwrap(), &bytes).unwrap();
    let h = b.build();
    h.set_sink(Arc::new(TestSink::default()));
    h
}

fn stats(mut t: Vec<Duration>) -> String {
    t.sort();
    let n = t.len();
    format!("n {n} p50 {:?} p99 {:?} max {:?}", t[n / 2], t[(n * 99 / 100).min(n - 1)], t[n - 1])
}

/// One app, `n` presents in a row: does a self-timed hook ever fault / get disabled?
#[test]
fn probe_sustained_one_app() {
    for (mode, us) in [("selftime", 1000u64), ("randtime", 2000), ("randtime", 4000), ("randtime", 5000), ("randtime", 5500)] {
        let h = host();
        let ae = h.attach(app(1), &info("victim")).unwrap();
        let attack = format!("{mode}:{us}");
        let node = snap(app(1), 1, Role::Button, Some(&attack));
        let mut t = Vec::new();
        for _ in 0..300 {
            let t0 = Instant::now();
            let _ = ae.present(&node, PresentContext::Focus, 1);
            t.push(t0.elapsed());
            settle(&h);
        }
        let st = h.stats();
        eprintln!("PROBE sustained {attack:<15} {} faults {} disabled {}", stats(t), st.faults, st.disabled);
    }
}

/// Fresh apps per attack (fault paths).
#[test]
fn probe_fault_paths() {
    for attack in ["random", "clock", "settings", "bigresult:10", "bigresult:100", "bigresult:2000"] {
        let h = host();
        let mut t = Vec::new();
        let mut some = 0;
        for i in 0..20u32 {
            let ae = h.attach(app(i + 1), &info("victim")).unwrap();
            let node = snap(app(i + 1), 1, Role::Button, Some(attack));
            let t0 = Instant::now();
            if ae.present(&node, PresentContext::Focus, 1).is_some() { some += 1; }
            t.push(t0.elapsed());
            settle(&h);
        }
        let st = h.stats();
        eprintln!("PROBE fresh {attack:<15} {} faults {} answered {some}", stats(t), st.faults);
    }
}
