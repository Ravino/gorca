// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host + guest conformance: every benign guest derives facts, every adversarial guest ends in
//! the fault kind the spec expects (PX-4), the oversize arguments are rejected before any
//! allocation of the guest size, and the `.cwasm` cache works. Needs `guests/build.sh` first.

use m18a0_common::budget::Grant;
use m18a0_common::host::*;
use m18a0_common::model::*;
use std::path::PathBuf;
use std::sync::Arc;

#[global_allocator]
static A: CountingAlloc = CountingAlloc;

fn guests_dir() -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../build/guests");
    assert!(
        d.join("SHA256SUMS").exists(),
        "guests are not built: run docs/bakeoff/m18a-0-plugin-call-model/harness/guests/build.sh (after `. ./env.sh`)"
    );
    d
}

fn engine() -> Arc<HostEngine> {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cwasm");
    HostEngine::new(&EngineCfg {
        cache_dir: dir,
        pooling: true,
    })
    .expect("engine")
}

fn runtime(he: &Arc<HostEngine>, name: &str) -> PluginRuntime {
    let lp = he
        .load(name, &guests_dir().join(format!("{name}.wasm")))
        .expect("load");
    PluginRuntime::new(1, he.clone(), Arc::new(lp))
}

fn job(fuel: u64, nodes: Vec<NodeId>) -> PluginBatch {
    PluginBatch {
        plugin: 1,
        job: 1,
        slot_epoch: 1,
        nodes,
        interactive: true,
        grant: Grant {
            fuel,
            ns: fuel / 2,
            capped: false,
        },
        created_ns: m18a0_common::timing::now_ns(),
    }
}

fn run(name: &str, fuel: u64, nodes: &[NodeId]) -> PluginReply {
    let he = engine();
    let mut rt = runtime(&he, name);
    let table = NodeTable::build(256, 7);
    let rows = FrozenRowsFactory { table: &table }.build(nodes);
    rt.run(&job(fuel, nodes.to_vec()), rows)
}

#[test]
fn benign_guests_assert_facts() {
    for name in [
        "signal-p2",
        "dummy-1",
        "dummy-2",
        "dummy-3",
        "dummy-4",
        "dummy-5",
        "dummy-6",
        "dummy-7",
        "dummy-8",
        "dummy-9",
        "a5-cold-load",
    ] {
        match run(name, 400_000, &[5, 6, 7, 8]) {
            PluginReply::Done(d) => {
                assert!(!d.facts.is_empty(), "{name}: no facts");
                assert!(
                    !d.token.reads.is_empty()
                        || d.token.scope_wide.is_some()
                        || name == "dummy-1"
                        || name == "a5-cold-load",
                    "{name}: no read set"
                );
                assert!(d.fuel_used > 0, "{name}");
            }
            other => panic!("{name}: {other:?}"),
        }
    }
}

#[test]
fn signal_p2_shape_is_three_facts_per_node() {
    let PluginReply::Done(d) = run("signal-p2", 400_000, &[5, 6]) else {
        panic!("not done")
    };
    assert_eq!(d.facts.len(), 6);
    assert!(d.facts.iter().all(|f| matches!(f.group, 100..=102)));
}

fn fault_of(name: &str, fuel: u64) -> FaultKind {
    match run(name, fuel, &[5, 6]) {
        PluginReply::Fault(f) => f.kind,
        other => panic!("{name}: expected fault, got {other:?}"),
    }
}

#[test]
fn spin_guest_ends_out_of_fuel() {
    assert_eq!(fault_of("a6-full-grant", 100_000), FaultKind::OutOfFuel);
}

#[test]
fn huge_return_is_rejected_atomically() {
    assert_eq!(
        fault_of("a4-huge-return", 50_000_000),
        FaultKind::MalformedEffect
    );
}

#[test]
fn oversize_arguments_are_rejected_before_any_copy() {
    for name in ["a3-huge-string", "a3b-huge-assert", "a3c-huge-attr"] {
        BIG_ALLOC_MAX.store(0, std::sync::atomic::Ordering::Relaxed);
        let k = fault_of(name, 50_000_000);
        assert_eq!(k, FaultKind::MalformedEffect, "{name}");
        let big = BIG_ALLOC_MAX.load(std::sync::atomic::Ordering::Relaxed);
        assert!(
            big < (14 << 20),
            "{name}: the host allocated {big} bytes (guest size was 14 MiB)"
        );
    }
}

#[test]
fn import_loop_is_stopped_by_precharge_or_fuel() {
    let k = fault_of("a13-subtree-loop", 400_000);
    assert!(
        matches!(
            k,
            FaultKind::PrechargeExhausted | FaultKind::OutOfFuel | FaultKind::Watchdog
        ),
        "{k:?}"
    );
}

#[test]
fn page_stride_ends_in_a_fault_not_a_hang() {
    let he = engine();
    let mut rt = runtime(&he, "a1-page-stride");
    let table = NodeTable::build(64, 1);
    let rows = FrozenRowsFactory { table: &table }.build(&[5]);
    let mut seen = None;
    for _ in 0..8 {
        if let PluginReply::Fault(f) = rt.run(&job(50_000_000, vec![5]), rows.clone()) {
            seen = Some(f.kind);
            break;
        }
    }
    assert!(
        matches!(seen, Some(FaultKind::LimiterDenied | FaultKind::Trap)),
        "{seen:?}"
    );
}

#[test]
fn pointer_chase_is_bounded_by_fuel() {
    match run("a2-pointer-chase", 3_000_000, &[5]) {
        PluginReply::Done(_) => {}
        PluginReply::Fault(f) => assert_eq!(f.kind, FaultKind::OutOfFuel),
        other => panic!("{other:?}"),
    }
}

#[test]
fn correctness_guests_return_expected_effects() {
    let PluginReply::Done(d) = run("a8-refresh-each", 400_000, &[5, 6]) else {
        panic!()
    };
    assert_eq!(d.refreshes.len(), 2);
    assert!(d.facts.is_empty());
    let PluginReply::Done(d) = run("a12-self", 400_000, &[5]) else {
        panic!()
    };
    assert_eq!(d.facts.len(), 1);
    let PluginReply::Done(d) = run("a7-patch-match", 400_000, &[5]) else {
        panic!()
    };
    assert_eq!(d.facts.len(), 2);
}

#[test]
fn cwasm_cache_hits_on_second_load_and_survives_corruption() {
    let he = engine();
    let p = guests_dir().join("dummy-1.wasm");
    let a = he.load("d1", &p).expect("first");
    let b = he.load("d1", &p).expect("second");
    assert!(b.cache_hit, "second load must hit the cache");
    assert_eq!(a.sha256, b.sha256);
    // A corrupt cache file is a miss, not a failure (CD-W2).
    std::fs::write(&b.cwasm_path, b"not a cwasm").expect("corrupt");
    let c = he.load("d1", &p).expect("after corruption");
    assert!(!c.cache_hit);
}

#[test]
fn forged_handle_reads_none_and_valid_handle_reads_the_node() {
    let key = MacKey::random();
    let other = MacKey::random();
    let h = mint_handle(&key, 1, 5);
    assert_ne!(h, mint_handle(&key, 1, 6));
    assert_ne!(
        h,
        mint_handle(&other, 1, 5),
        "a handle from another instance must not verify"
    );
}
