//! `RealSut` against real `b1-fakeapp` processes, without a daemon: the Sut plumbing the
//! conformance suite and the walk run on in the real configuration.

#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::type_complexity,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::too_many_lines
)]

mod common;

use std::path::PathBuf;

use common::Bus;
use gorca_testkit::b1::proto::Cmd;
use gorca_testkit::b1::real::{RealConfig, RealSut, default_models_dir};
use gorca_testkit::b1::sut::{Sut, SutExt};

fn cfg(bus: &Bus) -> RealConfig {
    RealConfig {
        fakeapp_bin: PathBuf::from(env!("CARGO_BIN_EXE_b1-fakeapp")),
        models_dir: default_models_dir(),
        a11y_bus: bus.address.clone(),
        session_bus: None,
        work_dir: bus.dir.clone(),
        present_log: bus.dir.join("present.jsonl"),
        scale_nodes: None,
        allow_any_bus: true,
        register_listeners: false,
    }
}

#[test]
fn drives_apps_reads_truth_and_presentations() {
    let bus = Bus::start();
    let mut s = RealSut::new(cfg(&bus), "none");
    s.start_app("settings", "settings").unwrap();
    let c = s.path_of("settings", "controls[2]").unwrap();
    let t0 = s.now_ns();
    assert!(t0 > 1_000_000, "clock calibrated from the fake app");
    s.must(
        "settings",
        Cmd::Rename {
            node: c.clone(),
            name: "Hello".into(),
            evented: false,
        },
    )
    .unwrap();
    let truth = s.truth();
    assert_eq!(truth.len(), 1);
    assert_eq!(truth[0].app, "settings");
    assert!(truth[0].t_ns > 0 && truth[0].t_ns <= s.now_ns() + 1_000_000);
    s.focus("settings", &c).unwrap();
    let st = s.calls("settings").unwrap();
    assert!(
        st.total_calls() == 0,
        "no reader attached, no calls: {st:?}"
    );
    // A presentation written by a (pretend) daemon is read back with its stimulus.
    let bus_name = s.bus_name("settings");
    let line = format!(
        "{{\"t_ns\":{},\"utt_id\":1,\"epoch\":1,\"app_bus\":\"{bus_name}\",\"path\":\"{c}\",\"source\":\"miss_reply\",\"role\":\"button\",\"states\":[],\"name\":\"Hello\",\"value\":null,\"may_refine\":false}}\n",
        s.now_ns() + 1_000
    );
    std::fs::write(bus.dir.join("present.jsonl"), line).unwrap();
    let p = s.presentations();
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].app, "settings");
    assert!(p[0].stimulus_ns > 0 && p[0].stimulus_ns <= p[0].t_ns);
    // Restart: new bus name, same paths; kill leaves no live app.
    let old = s.bus_name("settings");
    s.restart_app("settings").unwrap();
    assert_ne!(s.bus_name("settings"), old);
    assert_eq!(s.path_of("settings", "controls[2]").unwrap(), c);
    s.cmd("settings", Cmd::Kill).unwrap();
    assert!(s.cmd("settings", Cmd::Landmarks).is_err());
}

#[test]
fn the_walk_runs_against_real_fake_apps_without_a_reader() {
    use gorca_testkit::b1::walk::{WalkSpec, execute};
    let bus = Bus::start();
    let mut s = RealSut::new(cfg(&bus), "none");
    let spec = WalkSpec {
        loops: 1,
        initial_tabs: 32,
        list_arrows: 20,
        panels: 2,
        storm_loop: 99,
        hung_loop: 99,
        step_ms_min: 0,
        step_ms_max: 1,
        pause_every: 0,
        burst_s: 0,
        ..WalkSpec::default()
    };
    let run = execute(&mut s, &spec, 3);
    assert!(
        run.steps_failed.is_empty() || run.steps_failed.iter().all(|f| f.starts_with("where-am-i")),
        "{:#?}",
        run.steps_failed
    );
    assert_eq!(run.actual.focus, run.scripted.focus);
}
