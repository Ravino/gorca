//! The binary: control channel, truth log, isolation refusal, every shipped model loads.

#![allow(clippy::disallowed_methods)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::Bus;
use gorca_testkit::b1::appctl::{AppArgs, B1App};
use gorca_testkit::b1::proto::{Cmd, ModelSpec};

fn bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_b1-fakeapp"))
}

fn models() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../models")
}

fn args(bus: &Bus, model: &str, label: &str) -> AppArgs {
    let mut a = AppArgs::new(model, label, &bus.address);
    a.register = false;
    a.allow_any_bus = true;
    a.dir = Some(bus.dir.clone());
    a
}

#[test]
fn every_shipped_model_parses_and_builds_a_sane_tree() {
    use gorca_testkit::b1::world::World;
    let mut seen = 0;
    for e in std::fs::read_dir(models()).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        let spec: ModelSpec = serde_json::from_str(&std::fs::read_to_string(&p).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", p.display()));
        let w = World::new(spec.clone(), &spec.name);
        let live = w.live_count();
        let want = spec.nodes_idle;
        assert!(
            live >= want.saturating_sub(want / 10) && live <= want + want / 5 + 80,
            "{}: {live} nodes for nodes_idle {want}",
            p.display()
        );
        seen += 1;
    }
    assert!(seen >= 7, "{seen} models");
}

#[test]
fn refuses_a_bus_that_is_not_a_harness_session() {
    let out = Command::new(bin())
        .args(["--model", "settings", "--bus", "unix:path=/run/user/1000/at-spi/bus_0"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not a harness session"));
}

#[test]
fn control_channel_truth_log_and_restart_with_the_same_paths() {
    let bus = Bus::start();
    let mut a = B1App::spawn(&bin(), &args(&bus, "settings", "s1"), &bus.dir).unwrap();
    assert!(a.bus_name().starts_with(':'));
    let lm = a.cmd(&Cmd::Landmarks).unwrap();
    assert!(lm.ok);
    let first_path = lm.data["landmarks"]["controls"][0].as_str().unwrap().to_owned();
    let r = a
        .cmd(&Cmd::Rename { node: "controls[0]".into(), name: "Hello".into(), evented: false })
        .unwrap();
    assert!(r.ok);
    let t = a.truth();
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].path.as_str(), t[0].new.as_str()), (first_path.as_str(), "Hello"));
    assert!(!a.cmd(&Cmd::Rename { node: "nope".into(), name: "x".into(), evented: true }).unwrap().ok);
    let stats = a.stats().unwrap();
    assert!(stats.nodes_live > 700);
    // Crash and restart: same seed gives the same paths under a new bus name (IC-6).
    let old_name = a.bus_name().to_owned();
    a.kill();
    let mut b = B1App::spawn(&bin(), &args(&bus, "settings", "s1b"), &bus.dir).unwrap();
    assert_ne!(b.bus_name(), old_name);
    let lm2 = b.cmd(&Cmd::Landmarks).unwrap();
    assert_eq!(lm2.data["landmarks"]["controls"][0].as_str().unwrap(), first_path);
}
