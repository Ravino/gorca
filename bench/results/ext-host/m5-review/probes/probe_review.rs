//! REVIEW PROBE (m5-review): host-side costs a hostile component can force that fuel cannot see.
//! Copy to crates/gorca-ext-host/tests/ and set HOSTILE_WASM to the ext-hostile component.
#![allow(clippy::all, clippy::pedantic, clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod common;

use std::sync::Arc;
use std::time::Instant;

use common::{app, info, snap, settle};
use gorca_core::{AppExt, ExtSource, MirrorDirectory, PresentContext};
use gorca_ext_host::{ExtHost, Manifest};
use gorca_model::{NodeEvent, Politeness, Role};

const MANIFEST: &str = r#"
name = "ext-hostile"
backend = "wasm"
module = "ext_hostile.wasm"
hooks = ["on-event", "present"]
capabilities = []
[matcher]
app_name = ["*"]
"#;

fn hostile() -> (ExtHost, Arc<dyn AppExt>) {
    let bytes = std::fs::read(std::env::var("HOSTILE_WASM").expect("HOSTILE_WASM")).unwrap();
    let mut b = ExtHost::builder(Arc::new(MirrorDirectory::new()));
    b.add_wasm(Manifest::parse(MANIFEST).unwrap(), &bytes).unwrap();
    let h = b.build();
    h.set_sink(Arc::new(common::TestSink::default()));
    let ae = h.attach(app(1), &info("victim")).unwrap();
    (h, ae)
}

fn ann(t: &str) -> NodeEvent {
    NodeEvent::Announcement { node: None, text: Arc::from(t), politeness: Politeness::Polite }
}

#[test]
fn review_probe_log_lift_cost() {
    let (h, ae) = hostile();
    ae.on_event(&ann("ok"));
    for t in ["log:1", "log:8", "log:64", "logforever"] {
        let t0 = Instant::now();
        ae.on_event(&ann(t));
        let dt = t0.elapsed();
        settle(&h);
        eprintln!("on_event({t}): {dt:?} stats={:?}", h.stats());
    }
}

#[test]
fn review_probe_present_logforever_on_reader() {
    let (h, ae) = hostile();
    for i in 0..3 {
        let n = snap(app(1), 1, Role::Button, Some("logforever"));
        let t0 = Instant::now();
        let r = ae.present(&n, PresentContext::Focus, i);
        eprintln!("present(logforever) #{i}: {:?} -> {:?}", t0.elapsed(), r.is_some());
        settle(&h);
    }
    eprintln!("stats={:?}", h.stats());
}

#[test]
fn review_probe_alias_small() {
    let (h, ae) = hostile();
    for n in [1usize, 4, 16] {
        let t0 = Instant::now();
        ae.on_event(&ann(&format!("alias:{n}")));
        eprintln!("on_event(alias:{n}) host lifts {} MiB: {:?} faults={}", n * 16, t0.elapsed(), h.stats().faults);
        settle(&h);
    }
}

/// Run alone under `ulimit -v`: 2000 x 16 MiB = 32 GB of host allocation from a 32 MiB guest.
#[test]
#[ignore = "review probe: kills the process"]
fn review_probe_alias_big() {
    let (_h, ae) = hostile();
    let n = snap(app(1), 1, Role::Button, Some("alias:2000"));
    let t0 = Instant::now();
    let r = ae.present(&n, PresentContext::Focus, 1);
    eprintln!("survived: {:?} {:?}", t0.elapsed(), r.is_some());
}
