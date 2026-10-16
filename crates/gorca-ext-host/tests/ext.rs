//! Host behaviour on both backends (the same extension source, built both ways, ADR-0008 3.3):
//! hooks, verdict composition, fault containment, reset latency, three strikes, isolation between
//! applications, settings and the keymap merge.
#![allow(
    clippy::pedantic,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::disallowed_methods
)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{BACKENDS, Backend, app, host, info, settle, snap};
use gorca_core::{AppExt, ExtSource, ExtVerdict, PresentContext, PresenterRegistry, segments_text};
use gorca_model::{Epoch, NodeEvent, Politeness, Role, UttId};

fn announce(text: &str) -> NodeEvent {
    NodeEvent::Announcement {
        node: None,
        text: Arc::from(text),
        politeness: Politeness::Polite,
    }
}

fn attach(h: &gorca_ext_host::ExtHost, serial: u32, name: &str) -> Arc<dyn AppExt> {
    h.attach(app(serial), &info(name)).expect("matches")
}

#[test]
fn present_replaces_only_for_the_node_the_extension_wants() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "anything");
        let yes = snap(app(1), 1, Role::Button, Some("gorca-ext-present"));
        let no = snap(app(1), 2, Role::Button, Some("OK"));
        let got = ae.present(&yes, PresentContext::Focus, 1).expect("{b:?}");
        assert_eq!(segments_text(&got), "ext presented", "{b:?}");
        assert!(ae.present(&no, PresentContext::Focus, 1).is_none(), "{b:?}");
    }
}

#[test]
fn verdicts_pass_suppress_and_rewrite() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        assert_eq!(ae.on_event(&announce("hello")), ExtVerdict::Pass, "{b:?}");
        assert_eq!(
            ae.on_event(&announce("suppress")),
            ExtVerdict::Suppress,
            "{b:?}"
        );
        match ae.on_event(&announce("rewrite:spoken instead")) {
            ExtVerdict::Rewrite(s) => assert_eq!(segments_text(&s), "spoken instead", "{b:?}"),
            v => panic!("{b:?}: {v:?}"),
        }
        // Events outside the WIT subset are never offered.
        let doc = NodeEvent::DocumentEvent {
            node: snap(app(1), 1, Role::Button, None).key,
            kind: gorca_model::DocKind::LoadComplete,
        };
        assert_eq!(ae.on_event(&doc), ExtVerdict::Pass);
    }
}

#[test]
fn std_hashmap_and_clocks_work_inside_the_sandbox() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        match ae.on_event(&announce("std")) {
            ExtVerdict::Rewrite(s) => assert_eq!(segments_text(&s), "std ok", "{b:?}"),
            v => panic!("{b:?}: {v:?}"),
        }
        assert_eq!(h.stats().faults, 0, "{b:?}");
    }
}

#[test]
fn commands_run_on_the_worker_and_speak_through_the_sink() {
    for b in BACKENDS {
        let (h, sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        ae.command("ext-test", 1);
        let got = sink.wait(1, Duration::from_secs(2));
        assert_eq!(got, vec![("ext hello".to_owned(), true)], "{b:?}");
        // A command the extension does not know speaks nothing and breaks nothing.
        ae.command("ext-test", 999);
        ae.command("no-such-extension", 1);
        ae.command("ext-test", 5);
        let got = sink.wait(2, Duration::from_secs(2));
        assert_eq!(got[1], ("ext via host".to_owned(), false), "{b:?}");
        assert_eq!(h.stats().faults, 0, "{b:?}");
    }
}

#[test]
fn a_crash_is_contained_and_the_instance_is_reset_under_50_ms() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        // Warm: the first call of an instance is slower.
        assert_eq!(ae.on_event(&announce("hi")), ExtVerdict::Pass);
        let t = Instant::now();
        let v = ae.on_event(&announce("crash"));
        // The replacement is built by a worker; until it is ready the slot has no opinion.
        let during = ae.on_event(&announce("suppress"));
        assert!(
            matches!(during, ExtVerdict::Pass | ExtVerdict::Suppress),
            "{b:?}"
        );
        settle(&h);
        let healthy = ae.on_event(&announce("suppress"));
        let took = t.elapsed();
        assert_eq!(v, ExtVerdict::Pass, "a faulted hook has no opinion ({b:?})");
        assert_eq!(
            healthy,
            ExtVerdict::Suppress,
            "the fresh instance answers ({b:?})"
        );
        assert!(took < Duration::from_millis(50), "{b:?}: {took:?}");
        let s = h.stats();
        assert_eq!((s.faults, s.resets, s.disabled), (1, 1, 0), "{b:?}");
        println!(
            "{b:?} crash + reset + next call: {took:?}, reset {} us",
            s.max_reset_ns / 1000
        );
    }
}

#[test]
fn a_loop_is_interrupted_at_the_limit_and_reset_under_50_ms() {
    // Native cannot pre-empt a hang (ADR-0008 3.3): only the component build is tested.
    let (h, _sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    assert_eq!(ae.on_event(&announce("hi")), ExtVerdict::Pass);
    for _ in 0..2 {
        let t = Instant::now();
        let v = ae.on_event(&announce("spin"));
        let call = t.elapsed();
        assert_eq!(v, ExtVerdict::Pass);
        settle(&h);
        let took = t.elapsed();
        assert!(
            call < Duration::from_millis(50),
            "the caller waited {call:?}"
        );
        assert!(took < Duration::from_millis(50), "{took:?}");
        assert!(
            took >= Duration::from_micros(100),
            "interrupted too early: {took:?}"
        );
        println!("spin interrupted and reset in {took:?}");
    }
    assert_eq!(ae.on_event(&announce("suppress")), ExtVerdict::Suppress);
}

#[test]
fn a_command_that_loops_is_cut_at_20_ms_and_the_worker_survives() {
    let (h, sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    ae.command("ext-test", 3);
    settle(&h);
    ae.command("ext-test", 1);
    let got = sink.wait(1, Duration::from_secs(2));
    assert_eq!(got[0].0, "ext hello");
    assert_eq!(h.stats().faults, 1);
}

#[test]
fn memory_over_the_cap_is_a_fault_not_a_crash_of_the_host() {
    let (h, _sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    assert_eq!(ae.on_event(&announce("oom")), ExtVerdict::Pass);
    settle(&h);
    assert_eq!(h.stats().faults, 1);
    assert_eq!(ae.on_event(&announce("suppress")), ExtVerdict::Suppress);
}

#[test]
fn three_resets_in_a_minute_disable_with_one_announcement() {
    for b in BACKENDS {
        let (h, sink, _) = host(b);
        let ae = attach(&h, 1, "gnome-terminal");
        for _ in 0..3 {
            assert_eq!(ae.on_event(&announce("crash")), ExtVerdict::Pass);
            settle(&h);
        }
        let s = h.stats();
        assert_eq!((s.faults, s.disabled), (3, 1), "{b:?}");
        // Disabled: no opinion, no further faults, no second announcement.
        assert_eq!(
            ae.on_event(&announce("suppress")),
            ExtVerdict::Pass,
            "{b:?}"
        );
        let yes = snap(app(1), 1, Role::Button, Some("gorca-ext-present"));
        assert!(
            ae.present(&yes, PresentContext::Focus, 1).is_none(),
            "{b:?}"
        );
        assert_eq!(h.stats().faults, 3, "{b:?}");
        let said = sink.wait(1, Duration::from_millis(200));
        assert_eq!(said.len(), 1, "{b:?}: {said:?}");
        assert_eq!(said[0].0, "Extension ext test disabled for gnome-terminal");
        assert!(!said[0].1, "a notice does not interrupt");
    }
}

#[test]
fn a_faulty_app_does_not_affect_another_apps_instance() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let a = attach(&h, 1, "a");
        let c = attach(&h, 2, "c");
        for _ in 0..3 {
            a.on_event(&announce("crash"));
            settle(&h);
        }
        assert_eq!(a.on_event(&announce("suppress")), ExtVerdict::Pass, "{b:?}");
        assert_eq!(
            c.on_event(&announce("suppress")),
            ExtVerdict::Suppress,
            "{b:?}"
        );
    }
}

#[test]
fn the_core_still_presents_when_the_extension_faults() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let h = Arc::new(h);
        let _ae = attach(&h, 1, "x");
        let reg = PresenterRegistry::builtin().with_ext(h.clone());
        for name in ["gorca-ext-crash", "gorca-ext-spin"] {
            if b == Backend::Native && name == "gorca-ext-spin" {
                continue;
            }
            let n = snap(app(1), 1, Role::Button, Some(name));
            let t = Instant::now();
            let u = reg.present(PresentContext::Focus, &n, UttId(1), Epoch(1));
            let took = t.elapsed();
            settle(&h);
            let said = u
                .segments
                .iter()
                .map(|s| s.text.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            assert!(
                said.contains(name),
                "core presentation spoke the node: {said} ({b:?})"
            );
            assert!(took < Duration::from_millis(50), "{b:?} {name}: {took:?}");
        }
        let ok = snap(app(1), 2, Role::Button, Some("gorca-ext-present"));
        let u = reg.present(PresentContext::Focus, &ok, UttId(2), Epoch(1));
        assert_eq!(u.segments.len(), 1);
        assert_eq!(&*u.segments[0].text, "ext presented");
    }
}

#[test]
fn settings_are_per_extension_and_need_the_capability() {
    for b in BACKENDS {
        let (h, sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        ae.command("ext-test", 4);
        ae.command("ext-test", 4);
        let got = sink.wait(2, Duration::from_secs(2));
        assert_eq!(got[0].0, "call 1", "{b:?}");
        assert_eq!(got[1].0, "call 2", "{b:?}");
        assert_eq!(h.settings().get("ext-test", "calls").as_deref(), Some("2"));
        assert_eq!(h.settings().get("other", "calls"), None);
    }
}

#[test]
fn matching_attaches_nothing_when_no_manifest_matches() {
    let dir = Arc::new(gorca_core::MirrorDirectory::new());
    let mut b = gorca_ext_host::ExtHost::builder(dir);
    let mut m = common::manifest(Backend::Native);
    m.matcher.app_name = vec!["only-this".into()];
    b.add_native(m, common::native_factory()).unwrap();
    let h = b.build();
    assert!(h.attach(app(1), &info("something-else")).is_none());
    assert!(h.for_app(app(1)).is_none());
    assert!(h.attach(app(2), &info("only-this")).is_some());
    assert!(h.for_app(app(2)).is_some());
    h.detach(app(2));
    assert!(h.for_app(app(2)).is_none());
}

#[test]
fn the_keymap_merge_registers_commands_with_default_chords() {
    let (h, _sink, _) = host(Backend::Native);
    let (reg, problems) = h.commands();
    assert!(problems.is_empty(), "{problems:?}");
    let (id, spec) = reg.by_name("ext.ext_test.hello").expect("registered");
    assert_eq!(spec.ext, "ext-test");
    assert_eq!(spec.local, 1);
    assert!(id.0 >= gorca_core::EXT_CMD_BASE);
    assert_eq!(reg.len(), 4);
    let bound: Vec<_> = reg
        .default_bindings()
        .map(|(ctx, chord, ..)| (ctx, chord.to_string()))
        .collect();
    assert_eq!(
        bound,
        vec![(
            gorca_config::ContextName::Focus,
            "gorca+shift+f9".to_owned()
        )]
    );
}

#[test]
fn a_broken_extension_directory_is_reported_and_never_fatal() {
    let root = std::env::temp_dir().join(format!("gorca-ext-load-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("bad-toml")).unwrap();
    std::fs::write(root.join("bad-toml/ext.toml"), "name = [").unwrap();
    std::fs::create_dir_all(root.join("no-module")).unwrap();
    std::fs::write(
        root.join("no-module/ext.toml"),
        "name = \"x\"\nmodule = \"missing.wasm\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("garbage")).unwrap();
    std::fs::write(
        root.join("garbage/ext.toml"),
        "name = \"g\"\nmodule = \"g.wasm\"\n",
    )
    .unwrap();
    std::fs::write(root.join("garbage/g.wasm"), b"\0asm not a component").unwrap();
    std::fs::create_dir_all(root.join("good")).unwrap();
    std::fs::write(
        root.join("good/ext.toml"),
        include_str!("../../../extensions/ext-test/ext.toml"),
    )
    .unwrap();
    std::fs::write(root.join("good/ext_test.wasm"), common::wasm_bytes()).unwrap();

    let mut b = gorca_ext_host::ExtHost::builder(Arc::new(gorca_core::MirrorDirectory::new()));
    b.load_dir(&root);
    let ok: Vec<_> = b
        .reports()
        .iter()
        .filter(|r| r.result.is_ok())
        .filter_map(|r| r.name.clone())
        .collect();
    assert_eq!(ok, vec!["ext-test".to_owned()]);
    assert_eq!(b.reports().iter().filter(|r| r.result.is_err()).count(), 3);
    let _ = std::fs::remove_dir_all(root);
}

// ---- hostile extension fixtures (security review: caps cannot be bypassed) ----

#[test]
fn a_flood_of_host_calls_is_bounded_per_call_and_per_second() {
    for b in BACKENDS {
        let (h, sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        let t = Instant::now();
        assert_eq!(ae.on_event(&announce("spam")), ExtVerdict::Pass, "{b:?}");
        let took = t.elapsed();
        assert!(took < Duration::from_millis(50), "{b:?}: {took:?}");
        // 500 speak calls in one hook call: at most 8 get through.
        let said = sink.all();
        assert!(said.len() <= 8, "{b:?}: {} messages", said.len());
        // 500 set_setting calls: at most 16 stored.
        let stored = (0..500)
            .filter(|i| h.settings().get("ext-test", &format!("k{i}")).is_some())
            .count();
        assert!(stored <= 16, "{b:?}: {stored} keys");
        assert_eq!(
            h.stats().faults,
            0,
            "{b:?}: a flood within fuel is not a fault"
        );
        // Hammering the hook does not lift the per-second speech budget.
        for _ in 0..20 {
            ae.on_event(&announce("spam"));
        }
        let total = sink.all().len();
        assert!(total <= 12, "{b:?}: {total} messages in a burst");
    }
}

#[test]
fn a_rewrite_on_every_event_cannot_flood_speech() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        let mut spoken = 0;
        for _ in 0..200 {
            match ae.on_event(&announce("rewrite:chatter")) {
                ExtVerdict::Rewrite(_) => spoken += 1,
                ExtVerdict::Suppress => {}
                v => panic!("{b:?}: {v:?}"),
            }
        }
        assert!(
            (1..=12).contains(&spoken),
            "{b:?}: {spoken} rewrites spoken"
        );
    }
}

#[test]
fn re_announcing_an_application_does_not_reset_its_strikes() {
    for b in BACKENDS {
        let (h, _sink, _) = host(b);
        let ae = attach(&h, 1, "x");
        for _ in 0..3 {
            ae.on_event(&announce("crash"));
            settle(&h);
        }
        assert_eq!(h.stats().disabled, 1, "{b:?}");
        // The connector announces the app again (identity verified late): same slot, same state.
        let again = attach(&h, 1, "x");
        assert_eq!(
            again.on_event(&announce("suppress")),
            ExtVerdict::Pass,
            "{b:?}"
        );
        assert_eq!(h.stats().faults, 3, "{b:?}: no new instance, no new faults");
    }
}

#[test]
fn instances_that_cannot_be_built_count_and_end_in_a_global_disable() {
    let (h, sink, _) = host(Backend::Wasm);
    // The pool holds POOL_INSTANCES component instances; every attach past that fails to
    // instantiate. Three such failures disable the extension for every new application.
    let mut kept = Vec::new();
    for i in 0..(gorca_ext_host::POOL_INSTANCES + 20) {
        if let Some(a) = h.attach(app(10_000 + i), &info("x")) {
            kept.push(a);
        }
    }
    let s = h.stats();
    assert_eq!(kept.len(), gorca_ext_host::POOL_INSTANCES as usize);
    assert_eq!(
        s.instantiate_failures, 3,
        "the fourth attach is not even tried"
    );
    assert!(s.attach_skipped >= 16, "{s:?}");
    let said = sink.all();
    assert_eq!(said.len(), 1, "{said:?}");
    assert_eq!(said[0].0, "Extension ext test disabled");
    // Review M-2: an extension disabled as a whole stops in the applications it is already
    // attached to, at once, with no fresh allowance of faults per slot.
    assert_eq!(kept[0].on_event(&announce("suppress")), ExtVerdict::Pass);
    assert_eq!(h.stats().faults, 0);
}

#[test]
fn a_busy_slot_passes_discrete_events_and_counts_the_skip() {
    // A command holds the slot's lock while it runs (up to its CPU limit); events arriving
    // meanwhile must neither run nor leak the core's duplicate speech.
    let (h, _sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    // The extension suppressed announcements last; the high-rate repeat is unit-tested in host.rs.
    assert_eq!(ae.on_event(&announce("suppress")), ExtVerdict::Suppress);
    let before = h.stats();
    let slot = h.slots_of(app(1)).into_iter().next().expect("slot");
    let busy = slot.hold_for_test();
    // Review N-3: announcements are discrete, so a busy slot lets them through instead of
    // repeating the suppress (which would silence an unrelated announcement).
    assert_eq!(ae.on_event(&announce("hello")), ExtVerdict::Pass);
    drop(busy);
    let after = h.stats();
    assert_eq!(after.busy_skips, before.busy_skips + 1);
    assert_eq!(after.busy_suppressed, before.busy_suppressed);
    assert_eq!(ae.on_event(&announce("hello")), ExtVerdict::Pass);
}

#[test]
fn the_epoch_ticker_runs_only_while_a_call_is_in_flight() {
    // Review N-4: no 1 kHz wake-ups while idle, but the backstop still ends a hung hook.
    let (h, _sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    std::thread::sleep(std::time::Duration::from_millis(50));
    let idle0 = h.epoch_ticks();
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert_eq!(h.epoch_ticks(), idle0, "an idle host must not tick");
    // A hung hook is still cut (fuel first, the epoch backstop for the rest).
    assert_eq!(ae.on_event(&announce("spin")), ExtVerdict::Pass);
    assert_eq!(h.stats().faults, 1);
    // The faulted slot rebuilds its instance in the background (also a guarded call).
    for _ in 0..200 {
        if h.settled() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    std::thread::sleep(std::time::Duration::from_millis(20));
    let after = h.epoch_ticks();
    assert!(after > idle0, "the hung hook was cut by ticks: {after} vs {idle0}");
    std::thread::sleep(std::time::Duration::from_millis(150));
    assert_eq!(h.epoch_ticks(), after, "the ticker parks again after the call");
}

#[test]
fn more_than_the_extension_cap_is_refused() {
    let mut b = gorca_ext_host::ExtHost::builder(Arc::new(gorca_core::MirrorDirectory::new()));
    for i in 0..gorca_ext_host::MAX_EXTENSIONS {
        let mut m = common::manifest(Backend::Native);
        m.name = format!("ext-{i}");
        m.keymap.clear();
        b.add_native(m, common::native_factory()).unwrap();
    }
    let mut m = common::manifest(Backend::Native);
    m.name = "one-too-many".into();
    assert!(b.add_native(m, common::native_factory()).is_err());
}

#[test]
fn a_default_chord_without_the_gorca_modifier_is_dropped_with_a_problem() {
    let mut m = common::manifest(Backend::Native);
    m.keymap[0].chord = Some("a".to_owned());
    let mut b = gorca_ext_host::ExtHost::builder(Arc::new(gorca_core::MirrorDirectory::new()));
    b.add_native(m, common::native_factory()).unwrap();
    let h = b.build();
    let (reg, problems) = h.commands();
    assert!(
        reg.by_name("ext.ext_test.hello").is_some(),
        "the command stays"
    );
    assert_eq!(
        reg.default_bindings().count(),
        0,
        "a plain key is never grabbed"
    );
    assert!(
        problems.iter().any(|p| p.contains("gorca modifier")),
        "{problems:?}"
    );
}

#[test]
fn memory_loops_and_oom_are_faults_at_every_reset() {
    // The caps are per instance and rebuilt with it: a reset cannot be used to escape them.
    let (h, _sink, _) = host(Backend::Wasm);
    let ae = attach(&h, 1, "x");
    for trigger in ["oom", "spin"] {
        assert_eq!(ae.on_event(&announce(trigger)), ExtVerdict::Pass);
        settle(&h);
    }
    assert_eq!(h.stats().faults, 2);
    // The third strike disables; the instance is never rebuilt past that.
    ae.on_event(&announce("oom"));
    settle(&h);
    assert_eq!(h.stats().disabled, 1);
    assert_eq!(ae.on_event(&announce("suppress")), ExtVerdict::Pass);
}
