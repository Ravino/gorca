#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later

use std::sync::{Arc, Mutex};

use gorca_model::{ConnectorId, Freshness, LocalId, NodeSnapshot, PropMask, Role, StateSet};

use super::*;
use crate::fill::NullPolicy;
use crate::store::{MirrorHandle, mirror_for};

fn app() -> AppId {
    AppId {
        connector: ConnectorId(0),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn node(n: u64) -> Arc<NodeSnapshot> {
    Arc::new(
        NodeSnapshot::new(key(n), Freshness::Fresh { at: Instant::now() })
            .with_name(Some(Arc::from("n")))
            .with_have(PropMask::PRESENT)
            .with_source(Role::Button, StateSet::empty()),
    )
}

/// Emits the queued effects on any input except `Reply`, and records every reply result.
#[derive(Default)]
struct Script {
    next: Vec<FillEffect>,
    log: Arc<Mutex<Vec<String>>>,
}

use crate::fill::FillEffect;

impl FillPolicy for Script {
    fn name(&self) -> &'static str {
        "script"
    }
    fn step(&mut self, _cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects) {
        match input {
            FillInput::Reply { result, .. } => {
                let s = match result {
                    FillResult::Applied { nodes, .. } => format!("applied:{nodes}"),
                    FillResult::Gone => "gone".to_owned(),
                    FillResult::Unresolved => "unresolved".to_owned(),
                    FillResult::Timeout => "timeout".to_owned(),
                    FillResult::NotSupported => "notsupported".to_owned(),
                    FillResult::Failed => "failed".to_owned(),
                    FillResult::Refused(r) => format!("refused:{}", r.name()),
                    FillResult::Cancelled => "cancelled".to_owned(),
                };
                if let Ok(mut l) = self.log.lock() {
                    l.push(s);
                }
            }
            FillInput::Appeared { .. }
            | FillInput::Ready
            | FillInput::Events(_)
            | FillInput::Resync { .. }
            | FillInput::Demand { .. }
            | FillInput::FocusSettled { .. }
            | FillInput::WindowActivated { .. }
            | FillInput::Tick => out.extend(self.next.drain(..)),
        }
    }
    fn memory_bytes(&self) -> usize {
        0
    }
}

struct Rig {
    driver: FillDriver,
    writer: MirrorWriter,
    reader: MirrorHandle,
    log: Arc<Mutex<Vec<String>>>,
    now: Instant,
}

fn rig_with(effects: Vec<FillEffect>, limits: FillLimits) -> Rig {
    let (writer, reader) = mirror_for(app(), None);
    let policy = Script {
        next: effects,
        log: Arc::default(),
    };
    let log = Arc::clone(&policy.log);
    Rig {
        driver: FillDriver::new(app(), AppClass::Unknown, Box::new(policy), limits, None),
        writer,
        reader,
        log,
        now: Instant::now(),
    }
}

fn rig(effects: Vec<FillEffect>) -> Rig {
    rig_with(effects, FillLimits::default())
}

impl Rig {
    fn tick(&mut self, health: HealthView) -> Vec<IssuedFill> {
        let cx = DriverCx {
            now: self.now,
            health,
            mirror: &self.reader,
            focus: None,
        };
        self.driver.on_input(&cx, &mut self.writer, FillInput::Tick);
        self.driver.take_issued()
    }

    fn reply(&mut self, f: &IssuedFill, r: FillReply) -> ReplyReport {
        let cx = DriverCx {
            now: self.now,
            health: HealthView::Healthy,
            mirror: &self.reader,
            focus: None,
        };
        self.driver
            .on_reply(&cx, &mut self.writer, f.id, f.issued_at, r)
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

fn fetch(n: u64, class: BudgetClass) -> FillEffect {
    FillEffect::Request {
        op: FillOp::Fetch {
            node: key(n),
            want: PropMask::PRESENT,
        },
        class,
    }
}

#[test]
fn a_request_gets_an_id_a_sequence_and_a_deadline() {
    let mut r = rig(vec![fetch(1, BudgetClass::Prefetch)]);
    let issued = r.tick(HealthView::Healthy);
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].issued_at, r.writer.seq());
    assert_eq!(
        issued[0].deadline,
        r.now + BudgetClass::Prefetch.default_timeout()
    );
}

#[test]
fn identical_requests_in_flight_are_deduplicated() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Interactive),
    ]);
    // The Interactive one is more urgent than the Prefetch flight: it is issued itself.
    assert_eq!(r.tick(HealthView::Healthy).len(), 2);
    assert_eq!(r.driver.stats().deduped, 1);
}

#[test]
fn a_reply_is_applied_with_its_sequence_and_reported_to_the_policy() {
    let mut r = rig(vec![fetch(1, BudgetClass::Prefetch)]);
    let f = r.tick(HealthView::Healthy).remove(0);
    let report = r.reply(&f, FillReply::Ok(SnapshotBatch::new(vec![node(1)])));
    assert_eq!(report.health, HealthSignal::Success);
    assert!(r.reader.get(key(1)).hit().is_some());
    assert_eq!(r.log(), ["applied:1"]);
    assert_eq!(r.driver.in_flight(), 0);
}

#[test]
fn a_reply_after_a_removal_never_revives() {
    let mut r = rig(vec![fetch(1, BudgetClass::Prefetch)]);
    let f = r.tick(HealthView::Healthy).remove(0);
    r.writer
        .tombstone(key(1), r.now, crate::store::RemovalCause::Event);
    r.reply(&f, FillReply::Ok(SnapshotBatch::new(vec![node(1)])));
    assert_eq!(r.reader.get(key(1)), gorca_model::Lookup::Gone);
    assert_eq!(r.driver.stats().dropped_tombstoned, 1);
}

#[test]
fn gone_tombstones_the_target_and_timeout_is_a_health_penalty() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(2, BudgetClass::Prefetch),
    ]);
    let fs = r.tick(HealthView::Healthy);
    let rep = r.reply(&fs[0], FillReply::Err(FillError::Gone));
    assert_eq!(rep.health, HealthSignal::Success);
    assert_eq!(r.reader.get(key(1)), gorca_model::Lookup::Gone);
    let rep = r.reply(&fs[1], FillReply::Err(FillError::Timeout));
    assert_eq!(rep.health, HealthSignal::Timeout(BudgetClass::Prefetch));
    assert_eq!(r.reader.get(key(2)), gorca_model::Lookup::Miss);
    assert_eq!(r.log(), ["gone", "timeout"]);
}

#[test]
fn unresolved_is_a_miss_never_gone() {
    let mut r = rig(vec![fetch(1, BudgetClass::Interactive)]);
    let f = r.tick(HealthView::Healthy).remove(0);
    r.reply(&f, FillReply::Err(FillError::Unresolved));
    assert_eq!(r.reader.get(key(1)), gorca_model::Lookup::Miss);
    assert_eq!(r.log(), ["unresolved"]);
}

#[test]
fn hung_apps_get_nothing_and_slow_apps_only_interactive() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(2, BudgetClass::Interactive),
    ]);
    assert!(r.tick(HealthView::Hung).is_empty());
    assert_eq!(r.driver.stats().refused[RefuseReason::Health.index()], 2);
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(2, BudgetClass::Background),
        fetch(3, BudgetClass::Interactive),
    ]);
    let issued = r.tick(HealthView::Slow);
    assert_eq!(issued.len(), 1);
    assert_eq!(issued[0].class, BudgetClass::Interactive);
}

#[test]
fn refusals_are_reported_to_the_policy() {
    let mut r = rig(vec![fetch(1, BudgetClass::Prefetch)]);
    r.tick(HealthView::Hung);
    assert_eq!(r.log(), ["refused:health"]);
}

#[test]
fn interactive_keeps_reserved_slots_against_prefetch() {
    let limits = FillLimits::default();
    let shared = limits.max_in_flight - limits.interactive_reserved;
    let mut effects: Vec<_> = (0..(shared as u64 + 4))
        .map(|n| fetch(n, BudgetClass::Prefetch))
        .collect();
    effects.push(fetch(100, BudgetClass::Interactive));
    effects.push(fetch(101, BudgetClass::Interactive));
    let mut r = rig_with(effects, limits.clone());
    let issued = r.tick(HealthView::Healthy);
    assert_eq!(
        issued
            .iter()
            .filter(|f| f.class == BudgetClass::Prefetch)
            .count(),
        shared
    );
    assert_eq!(
        issued
            .iter()
            .filter(|f| f.class == BudgetClass::Interactive)
            .count(),
        2,
        "both reserved slots are still free"
    );
    assert_eq!(r.driver.in_flight(), limits.max_in_flight);
    assert_eq!(r.driver.queued(), 4, "the rest waits");
}

#[test]
fn a_freed_slot_starts_the_queue() {
    let limits = FillLimits::default();
    let shared = limits.max_in_flight - limits.interactive_reserved;
    let effects: Vec<_> = (0..(shared as u64 + 2))
        .map(|n| fetch(n, BudgetClass::Prefetch))
        .collect();
    let mut r = rig_with(effects, limits);
    let first = r.tick(HealthView::Healthy);
    assert_eq!(r.driver.queued(), 2);
    r.reply(&first[0], FillReply::Err(FillError::Protocol));
    let more = r.driver.take_issued();
    assert_eq!(more.len(), 1);
    assert_eq!(r.driver.queued(), 1);
}

#[test]
fn background_is_limited_and_a_full_queue_refuses_the_newest() {
    let limits = FillLimits {
        max_pending: 3,
        ..FillLimits::default()
    };
    let effects: Vec<_> = (0..8u64)
        .map(|n| fetch(n, BudgetClass::Background))
        .collect();
    let mut r = rig_with(effects, limits);
    let issued = r.tick(HealthView::Healthy);
    assert_eq!(issued.len(), 2, "MAX_BACKGROUND_IN_FLIGHT");
    assert_eq!(r.driver.queued(), 3);
    assert_eq!(r.driver.stats().refused[RefuseReason::QueueFull.index()], 3);
}

#[test]
fn the_fill_rate_is_bounded_and_interactive_is_exempt() {
    let limits = FillLimits {
        rate_non_focused: 50,
        max_in_flight: 1_000,
        max_background: 1_000,
        ..FillLimits::default()
    };
    let mut effects: Vec<_> = (0..200u64)
        .map(|n| fetch(n, BudgetClass::Prefetch))
        .collect();
    effects.extend((1000..1010u64).map(|n| fetch(n, BudgetClass::Interactive)));
    let mut r = rig_with(effects, limits);
    let issued = r.tick(HealthView::Healthy);
    let prefetch = issued
        .iter()
        .filter(|f| f.class == BudgetClass::Prefetch)
        .count();
    assert_eq!(prefetch, 5, "burst is rate/10");
    assert_eq!(
        issued.len() - prefetch,
        10,
        "Interactive is never rate limited"
    );
    assert!(r.driver.stats().refused[RefuseReason::RateLimit.index()] >= 190);
}

#[test]
fn the_global_rate_is_shared() {
    let g = GlobalRate::new(100);
    let t = Instant::now();
    let ok = (0..50).filter(|_| g.try_take(t)).count();
    assert_eq!(ok, 10, "10 per 100 ms window");
    assert!(g.try_take(t + Duration::from_millis(150)));
}

#[test]
fn cache_dump_needs_verified_eligibility_and_is_rate_limited() {
    let dump = FillEffect::Request {
        op: FillOp::CacheDump {
            max_nodes: u32::MAX,
        },
        class: BudgetClass::Interactive,
    };
    let mut r = rig(vec![dump.clone()]);
    assert!(r.tick(HealthView::Healthy).is_empty());
    assert_eq!(
        r.driver.stats().refused[RefuseReason::DumpIneligible.index()],
        1
    );
    let mut r = rig(vec![dump.clone()]);
    r.driver.set_dump_complete(true);
    let f = r.tick(HealthView::Healthy);
    assert_eq!(f.len(), 1);
    assert_eq!(
        f[0].class,
        BudgetClass::Background,
        "a dump is always Background"
    );
    assert!(matches!(
        f[0].op,
        FillOp::CacheDump { max_nodes } if max_nodes == FillLimits::default().max_dump_nodes
    ));
    // Not again within the interval, even after the first finished.
    r.reply(&f[0], FillReply::Err(FillError::Timeout));
    r.driver_push(dump);
    r.now += Duration::from_secs(5);
    assert!(r.tick(HealthView::Healthy).is_empty());
    assert_eq!(
        r.driver.stats().refused[RefuseReason::DumpTooSoon.index()],
        1
    );
}

impl Rig {
    fn driver_push(&mut self, effect: FillEffect) {
        // Re-arm the scripted policy through a fresh driver would lose state; instead the policy
        // is rebuilt with the effect (the driver keeps its own state).
        let policy = Script {
            next: vec![effect],
            log: Arc::clone(&self.log),
        };
        self.driver.replace_policy_for_test(Box::new(policy));
    }
}

#[test]
fn a_repeated_announcement_does_not_revive_a_refuted_dump_claim() {
    let dump = FillEffect::Request {
        op: FillOp::CacheDump { max_nodes: 100 },
        class: BudgetClass::Background,
    };
    let mut r = rig(vec![dump]);
    r.driver.set_dump_complete(true);
    let f = r.tick(HealthView::Healthy).remove(0);
    r.reply(
        &f,
        FillReply::Ok(SnapshotBatch::new(vec![node(1), node(2)])),
    );
    assert!(!r.driver.dump_complete());
    // The connector announces the app again (its identity was verified late) with the same claim.
    r.driver.set_dump_complete(true);
    assert!(!r.driver.dump_complete(), "the refuted claim stays refuted");
}

#[test]
fn an_implausible_dump_clears_the_eligibility_for_good() {
    let dump = FillEffect::Request {
        op: FillOp::CacheDump { max_nodes: 100 },
        class: BudgetClass::Background,
    };
    let mut r = rig(vec![dump]);
    r.driver.set_dump_complete(true);
    let f = r.tick(HealthView::Healthy).remove(0);
    r.reply(
        &f,
        FillReply::Ok(SnapshotBatch::new(vec![node(1), node(2)])),
    );
    assert!(!r.driver.dump_complete());
    assert_eq!(r.driver.stats().dump_flag_cleared, 1);
}

#[test]
fn ops_and_replies_are_clamped_to_the_hard_bounds() {
    let effects = vec![
        FillEffect::Request {
            op: FillOp::Ancestry {
                node: key(1),
                max_depth: 250,
                want: PropMask::PRESENT,
            },
            class: BudgetClass::Prefetch,
        },
        FillEffect::Request {
            op: FillOp::ChildSnapshots {
                node: key(2),
                range: 0..1_000_000,
                want: PropMask::PRESENT,
            },
            class: BudgetClass::Prefetch,
        },
    ];
    let mut r = rig(effects);
    let f = r.tick(HealthView::Healthy);
    assert!(matches!(f[0].op, FillOp::Ancestry { max_depth: 64, .. }));
    assert!(matches!(&f[1].op, FillOp::ChildSnapshots { range, .. } if range.end == 1024));
    // A connector that answers with more than asked is cut (the reply is untrusted).
    let nodes: Vec<_> = (0..70u64).map(node).collect();
    r.reply(&f[0], FillReply::Ok(SnapshotBatch::new(nodes)));
    // The cut batch is applied in slices: the rest follows from the timers.
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    r.driver.poll_timers(&cx, &mut r.writer);
    assert_eq!(r.reader.stats().live, 65);
}

#[test]
fn a_siblings_request_is_clamped_and_its_reply_is_cut_to_the_window() {
    let effects = vec![FillEffect::Request {
        op: FillOp::Siblings {
            parent: key(1),
            node: key(2),
            before: 5_000,
            after: 5_000,
            indexed: false,
            want: PropMask::PRESENT,
        },
        class: BudgetClass::Prefetch,
    }];
    let mut r = rig(effects);
    let f = r.tick(HealthView::Healthy);
    // At most `max_child_range` entries in all; the span before the node is taken first.
    assert!(
        matches!(
            &f[0].op,
            FillOp::Siblings {
                before: 1024,
                after: 0,
                ..
            }
        ),
        "{:?}",
        f[0].op
    );
    // A reply longer than the (clamped) window is cut.
    let nodes: Vec<_> = (0..1_100u64).map(node).collect();
    r.reply(&f[0], FillReply::Ok(SnapshotBatch::new(nodes)));
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    for _ in 0..40 {
        r.driver.poll_timers(&cx, &mut r.writer);
    }
    assert_eq!(r.reader.stats().live, 1_025);
    // A `Gone` reply tombstones the parent whose child list was asked, not the node.
    assert_eq!(
        FillOp::Siblings {
            parent: key(1),
            node: key(2),
            before: 1,
            after: 1,
            indexed: false,
            want: PropMask::PRESENT,
        }
        .target(),
        Some(key(1))
    );
}

#[test]
fn cancel_removes_a_queued_request() {
    let limits = FillLimits {
        max_background: 0,
        ..FillLimits::default()
    };
    let mut r = rig_with(vec![fetch(1, BudgetClass::Background)], limits);
    assert!(r.tick(HealthView::Healthy).is_empty());
    assert_eq!(r.driver.queued(), 1);
    r.driver.replace_policy_for_test(Box::new(Script {
        next: vec![FillEffect::Cancel(FillReqId(1))],
        log: Arc::default(),
    }));
    r.tick(HealthView::Healthy);
    assert_eq!(r.driver.queued(), 0);
    assert_eq!(r.driver.stats().cancelled, 1);
}

#[test]
fn queued_prefetch_expires() {
    let limits = FillLimits {
        max_background: 0,
        ..FillLimits::default()
    };
    let mut r = rig_with(vec![fetch(1, BudgetClass::Background)], limits);
    r.tick(HealthView::Healthy);
    r.now += Duration::from_secs(2);
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    r.driver.poll_timers(&cx, &mut r.writer);
    assert_eq!(r.driver.queued(), 0);
    assert_eq!(r.driver.stats().refused[RefuseReason::Expired.index()], 1);
}

#[test]
fn a_policy_that_answers_every_refusal_with_the_same_request_cannot_loop() {
    struct Stubborn;
    impl FillPolicy for Stubborn {
        fn name(&self) -> &'static str {
            "stubborn"
        }
        fn step(&mut self, _cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects) {
            match input {
                FillInput::Tick | FillInput::Reply { .. } => {
                    out.push(fetch(7, BudgetClass::Prefetch));
                }
                FillInput::Appeared { .. }
                | FillInput::Ready
                | FillInput::Events(_)
                | FillInput::Resync { .. }
                | FillInput::Demand { .. }
                | FillInput::FocusSettled { .. }
                | FillInput::WindowActivated { .. } => {}
            }
        }
        fn memory_bytes(&self) -> usize {
            0
        }
    }
    let (mut w, rd) = mirror_for(app(), None);
    let mut d = FillDriver::new(
        app(),
        AppClass::Unknown,
        Box::new(Stubborn),
        FillLimits::default(),
        None,
    );
    let cx = DriverCx {
        now: Instant::now(),
        health: HealthView::Hung,
        mirror: &rd,
        focus: None,
    };
    d.on_input(&cx, &mut w, FillInput::Tick);
    assert!(d.stats().refused_total() <= 70);
    assert!(d.stats().notices_dropped > 0);
}

#[test]
fn external_replies_are_applied_without_telling_the_policy() {
    let mut r = rig(vec![]);
    let issued_at = r.writer.seq();
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    r.driver.on_external_reply(
        &cx,
        &mut r.writer,
        key(1),
        issued_at,
        FillReply::Ok(SnapshotBatch::new(vec![node(1)])),
    );
    assert!(r.reader.get(key(1)).hit().is_some());
    let seq = r.writer.seq();
    r.driver.on_external_reply(
        &cx,
        &mut r.writer,
        key(1),
        seq,
        FillReply::Err(FillError::Gone),
    );
    assert_eq!(r.reader.get(key(1)), gorca_model::Lookup::Gone);
    assert!(r.log().is_empty());
}

#[test]
fn the_null_policy_issues_nothing() {
    let (mut w, rd) = mirror_for(app(), None);
    let mut d = FillDriver::new(
        app(),
        AppClass::Unknown,
        Box::new(NullPolicy),
        FillLimits::default(),
        None,
    );
    let cx = DriverCx {
        now: Instant::now(),
        health: HealthView::Healthy,
        mirror: &rd,
        focus: None,
    };
    d.on_input(&cx, &mut w, FillInput::Ready);
    d.on_input(&cx, &mut w, FillInput::Tick);
    assert!(d.take_issued().is_empty());
    assert_eq!(d.policy_name(), "r0");
    assert!(d.next_wake().is_none());
}

#[test]
fn a_big_reply_is_applied_slice_by_slice_and_the_policy_hears_at_the_end() {
    let dump = FillEffect::Request {
        op: FillOp::CacheDump { max_nodes: 5_000 },
        class: BudgetClass::Background,
    };
    let mut r = rig(vec![dump]);
    r.driver.set_dump_complete(true);
    let f = r.tick(HealthView::Healthy).remove(0);
    let total = APPLY_SLICE * 3 + 10;
    let nodes: Vec<_> = (0..total as u64).map(node).collect();
    r.reply(&f, FillReply::Ok(SnapshotBatch::new(nodes)));
    assert_eq!(
        r.reader.stats().live,
        APPLY_SLICE as u64,
        "only the first slice is in"
    );
    assert!(r.log().is_empty(), "the policy is told when it is complete");
    assert!(r.driver.has_work());
    assert!(r.driver.next_wake().is_some());
    let mut calls = 0;
    while r.driver.has_work() {
        let cx = DriverCx {
            now: r.now,
            health: HealthView::Healthy,
            mirror: &r.reader,
            focus: None,
        };
        r.driver.poll_timers(&cx, &mut r.writer);
        calls += 1;
        assert!(calls < 20);
    }
    assert_eq!(r.reader.stats().live, total as u64);
    assert_eq!(r.log(), [format!("applied:{total}")]);
    assert_eq!(r.driver.stats().nodes_applied, total as u64);
}

#[test]
fn a_node_removed_while_its_slice_waits_is_not_revived() {
    let dump = FillEffect::Request {
        op: FillOp::CacheDump { max_nodes: 5_000 },
        class: BudgetClass::Background,
    };
    let mut r = rig(vec![dump]);
    r.driver.set_dump_complete(true);
    let f = r.tick(HealthView::Healthy).remove(0);
    let total = APPLY_SLICE * 2;
    let nodes: Vec<_> = (0..total as u64).map(node).collect();
    r.reply(&f, FillReply::Ok(SnapshotBatch::new(nodes)));
    // The victim sits in the second slice; it is removed before that slice is applied.
    let victim = APPLY_SLICE as u64 + 8;
    r.writer
        .tombstone(key(victim), r.now, crate::store::RemovalCause::Event);
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    r.driver.poll_timers(&cx, &mut r.writer);
    assert_eq!(r.reader.get(key(victim)), gorca_model::Lookup::Gone);
    assert_eq!(r.reader.stats().live, total as u64 - 1);
}

#[test]
fn a_policy_over_its_state_budget_is_counted() {
    struct Hog;
    impl FillPolicy for Hog {
        fn name(&self) -> &'static str {
            "hog"
        }
        fn step(&mut self, _: &FillCtx<'_>, _: FillInput<'_>, _: &mut FillEffects) {}
        fn memory_bytes(&self) -> usize {
            crate::limits::FILL_STATE_MAX_BYTES + 1
        }
    }
    let (mut w, rd) = mirror_for(app(), None);
    let mut d = FillDriver::new(
        app(),
        AppClass::Unknown,
        Box::new(Hog),
        FillLimits::default(),
        None,
    );
    let cx = DriverCx {
        now: Instant::now(),
        health: HealthView::Healthy,
        mirror: &rd,
        focus: None,
    };
    d.on_input(&cx, &mut w, FillInput::Ready);
    assert_eq!(d.stats().policy_over_budget, 1);
}

#[test]
fn gone_forgets_everything_in_flight() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(2, BudgetClass::Prefetch),
    ]);
    let f = r.tick(HealthView::Healthy);
    assert_eq!(f.len(), 2);
    r.driver.on_gone();
    assert_eq!(r.driver.in_flight(), 0);
    assert!(!r.driver.has_work());
    // A late reply of a forgotten request is ignored.
    let rep = r.reply(&f[0], FillReply::Ok(SnapshotBatch::new(vec![node(1)])));
    assert_eq!(rep.outcome, None);
    assert!(r.reader.get(key(1)).hit().is_none());
}

/// Reviewer probe F7: every accepted request is answered, a deduplicated one included.
#[test]
fn a_deduplicated_request_gets_the_carriers_reply() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Background),
    ]);
    let issued = r.tick(HealthView::Healthy);
    assert_eq!(issued.len(), 1, "one request on the wire");
    assert_eq!(r.driver.stats().deduped, 2);
    let f = issued[0].clone();
    r.reply(&f, FillReply::Ok(SnapshotBatch::new(vec![node(1)])));
    assert_eq!(r.log(), ["applied:1", "applied:1", "applied:1"]);
}

#[test]
fn deduplicated_requests_share_failures_too() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Prefetch),
    ]);
    let f = r.tick(HealthView::Healthy).remove(0);
    r.reply(&f, FillReply::Err(FillError::Timeout));
    assert_eq!(r.log(), ["timeout", "timeout"]);
}

/// An Interactive request never rides on a less urgent flight.
#[test]
fn an_interactive_request_is_not_deduplicated_onto_a_prefetch_flight() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(1, BudgetClass::Interactive),
    ]);
    let issued = r.tick(HealthView::Healthy);
    assert_eq!(issued.len(), 2);
    assert_eq!(issued[1].class, BudgetClass::Interactive);
    assert_eq!(
        issued[1].deadline,
        r.now + BudgetClass::Interactive.default_timeout()
    );
}

/// Every request gets its own sequence point (F1): a request issued after another is issued
/// strictly later, even when nothing else wrote.
#[test]
fn issue_points_are_strictly_increasing() {
    let mut r = rig(vec![
        fetch(1, BudgetClass::Prefetch),
        fetch(2, BudgetClass::Prefetch),
        fetch(3, BudgetClass::Prefetch),
    ]);
    let issued = r.tick(HealthView::Healthy);
    assert!(issued[0].issued_at < issued[1].issued_at);
    assert!(issued[1].issued_at < issued[2].issued_at);
}

#[test]
fn a_queued_request_that_expires_is_answered_with_its_aliases() {
    let limits = FillLimits {
        max_background: 0,
        ..FillLimits::default()
    };
    let mut r = rig_with(
        vec![
            fetch(1, BudgetClass::Background),
            fetch(1, BudgetClass::Background),
        ],
        limits,
    );
    r.tick(HealthView::Healthy);
    r.now += Duration::from_secs(2);
    let cx = DriverCx {
        now: r.now,
        health: HealthView::Healthy,
        mirror: &r.reader,
        focus: None,
    };
    r.driver.poll_timers(&cx, &mut r.writer);
    assert_eq!(r.log(), ["refused:expired", "refused:expired"]);
}
