//! The policy audits of the B-1 kit (MA-1, MA-6, MA-9) against the adopted policy through the real driver, and
//! MA-6 at the SPEC 7 caps (65 536 live nodes, 4 096-event batch, dirty-all) on the policy alone.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Instant;

use gorca_ingress::DirtySet;
use gorca_mirror::driver::{DriverCx, FillDriver, GlobalRate};
use gorca_mirror::fill::{
    AppClass, FillCtx, FillEffects, FillInput, FillOp, FillPolicy, FillResult, FocusPrefetch,
    HealthView, SlotView,
};
use gorca_mirror::{ApplyOrigin, ApplyOutcome, FillLimits, MirrorHandle, MirrorWriter, mirror_for};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeEvent, NodeKey,
    NodeSnapshot, PropMask, Relations, Role, SnapshotBatch, StateSet,
};
use gorca_testkit::b1::audit::{self, Health, PInput, PRequest, PolicyAdapter};

fn app_id() -> AppId {
    AppId {
        connector: ConnectorId(0),
        serial: 1,
    }
}

fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app_id(),
        local: LocalId(n),
    }
}

struct DriverPolicy {
    driver: FillDriver,
    w: MirrorWriter,
    r: MirrorHandle,
}

impl DriverPolicy {
    fn new() -> Self {
        let (w, r) = mirror_for(app_id(), None);
        let driver = FillDriver::new(
            app_id(),
            AppClass::Unknown,
            Box::new(FocusPrefetch::default()),
            FillLimits::default(),
            Some(GlobalRate::new(5_000)),
        );
        Self { driver, w, r }
    }
}

fn health_view(h: Health) -> HealthView {
    match h {
        Health::Healthy => HealthView::Healthy,
        Health::Slow => HealthView::Slow,
        Health::Hung => HealthView::Hung,
        Health::Flooded => HealthView::Flooded,
        Health::Gone => HealthView::Gone,
    }
}

impl PolicyAdapter for DriverPolicy {
    fn name(&self) -> String {
        self.driver.policy_name().to_owned()
    }

    fn step(&mut self, health: Health, input: PInput) -> Vec<PRequest> {
        if matches!(input, PInput::Gone) {
            // The app actor owns the driver and the policy: they are dropped with it.
            *self = DriverPolicy::new();
            return Vec::new();
        }
        let cx = DriverCx {
            now: Instant::now(),
            health: health_view(health),
            mirror: &self.r,
            focus: Some(key(1)),
        };
        match input {
            PInput::Appeared => self.driver.on_input(&cx, &mut self.w, FillInput::Ready),
            PInput::Events(e) => {
                let evs: Vec<NodeEvent> = e
                    .iter()
                    .map(|(n, _)| NodeEvent::ChildrenChanged { node: key(*n) })
                    .collect();
                self.driver
                    .on_input(&cx, &mut self.w, FillInput::Events(&evs));
            }
            PInput::FocusSettled(n) => {
                self.driver
                    .on_input(&cx, &mut self.w, FillInput::FocusSettled { node: key(n) });
            }
            PInput::WindowActivated(n) => self.driver.on_input(
                &cx,
                &mut self.w,
                FillInput::WindowActivated { window: key(n) },
            ),
            PInput::Resync | PInput::Tick => {
                self.driver.on_input(&cx, &mut self.w, FillInput::Tick);
            }
            PInput::Gone => {}
        }
        self.driver
            .take_issued()
            .into_iter()
            .map(|f| PRequest {
                class: match f.class {
                    gorca_rt::BudgetClass::Interactive => audit::Class::Interactive,
                    gorca_rt::BudgetClass::Prefetch => audit::Class::Prefetch,
                    gorca_rt::BudgetClass::Background | gorca_rt::BudgetClass::Startup => {
                        audit::Class::Background
                    }
                },
                node: 0,
            })
            .collect()
    }

    fn memory_bytes(&self) -> usize {
        self.driver.policy_memory_bytes()
    }
}

#[test]
fn ma1_policy_memory_accounting_prefetch() {
    audit::run_policy_memory_accounting(DriverPolicy::new()).unwrap();
}

#[test]
fn ma6_policy_step_is_bounded_prefetch_unoptimised() {
    let worst = audit::run_step_bounded(DriverPolicy::new(), 250_000).unwrap();
    eprintln!("worst driver step {worst} us (debug)");
}

#[test]
fn ma9_prefetch_stops_prefetch_and_background_when_the_app_is_slow_or_hung() {
    audit::run_policy_respects_health(DriverPolicy::new()).unwrap();
}

// ---- MA-6 at the caps, on the policy alone -------------------------------------------------

const NODES: u64 = 65_536;
const FANOUT: u64 = 8;

fn parent_of(i: u64) -> Option<u64> {
    (i > 0).then(|| (i - 1) / FANOUT)
}

fn snap(i: u64) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key: key(i),
        role: match i {
            0 => Role::Application,
            1 => Role::Window,
            _ => Role::Button,
        },
        states: StateSet::empty(),
        name: Some(Arc::from("n")),
        description: None,
        value: None,
        parent: parent_of(i).map(key),
        children: ChildList::Count(8),
        interfaces: Ifaces::empty(),
        attrs: AttrMap::default(),
        relations: Relations::default(),
        bounds: None,
        have: PropMask::NAME
            | PropMask::ROLE
            | PropMask::STATES
            | PropMask::VALUE
            | PropMask::PARENT
            | PropMask::CHILDREN,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
    })
}

fn filled_mirror() -> (MirrorWriter, MirrorHandle) {
    let (mut w, r) = mirror_for(app_id(), None);
    for chunk in 0..(NODES / 1_024) {
        let nodes = (chunk * 1_024..(chunk + 1) * 1_024).map(snap).collect();
        let seq = w.next_seq();
        w.apply(
            SnapshotBatch::new(nodes),
            ApplyOrigin::Reply { issued_at: seq },
        );
    }
    (w, r)
}

/// Worst `step` over the SPEC 7 inputs, best of `rounds` rounds (the worst call of a round can
/// be a preemption of the test thread; a structural cost shows in every round).
fn worst_step_us(rounds: usize) -> u64 {
    let (_w, r) = filled_mirror();
    let limits = FillLimits::default();
    let leaf = NODES - 1;
    let mut p = FocusPrefetch::default();
    let mk = |now: Instant, health, focus| FillCtx {
        now,
        app: app_id(),
        app_class: AppClass::Unknown,
        health,
        dump_complete: true,
        mirror: &r,
        focus,
        slots: SlotView {
            interactive: 8,
            prefetch: 6,
            background: 2,
            tokens: 1_000,
        },
        limits: &limits,
    };
    // Give the policy realistic state: a focus deep in the tree, its chain and its pages.
    let now = Instant::now();
    let cx = mk(now, HealthView::Healthy, Some(key(leaf)));
    let mut out = FillEffects::new();
    p.step(&cx, FillInput::FocusSettled { node: key(leaf) }, &mut out);
    let mut guard = 0;
    while !out.is_empty() && guard < 64 {
        guard += 1;
        let reqs: Vec<FillOp> = out
            .drain(..)
            .filter_map(|e| match e {
                gorca_mirror::fill::FillEffect::Request { op, .. } => Some(op),
                _ => None,
            })
            .collect();
        for op in reqs {
            let keys: Vec<NodeKey> = match &op {
                FillOp::Ancestry { node, .. } => {
                    let mut v = vec![node.local.0];
                    while let Some(pp) = parent_of(*v.last().unwrap()) {
                        v.push(pp);
                    }
                    v.into_iter().map(key).collect()
                }
                FillOp::ChildSnapshots { node, range, .. } => (range.start..range.end)
                    .map(|i| node.local.0 * FANOUT + 1 + u64::from(i))
                    .filter(|c| *c < NODES && parent_of(*c) == Some(node.local.0))
                    .map(key)
                    .collect(),
                FillOp::Siblings {
                    parent,
                    node,
                    before,
                    after,
                    ..
                } => {
                    let at = node.local.0 - (parent.local.0 * FANOUT + 1);
                    let from = at.saturating_sub(u64::from(*before));
                    (from..=at + u64::from(*after))
                        .map(|i| parent.local.0 * FANOUT + 1 + i)
                        .filter(|c| *c < NODES && parent_of(*c) == Some(parent.local.0))
                        .map(key)
                        .collect()
                }
                _ => Vec::new(),
            };
            let res = FillResult::Applied {
                keys: &keys,
                nodes: keys.len(),
                outcome: ApplyOutcome::default(),
            };
            p.step(
                &mk(now, HealthView::Healthy, Some(key(leaf))),
                FillInput::Reply {
                    id: gorca_mirror::fill::FillReqId(guard),
                    op: &op,
                    result: res,
                },
                &mut out,
            );
        }
    }
    // Worst-case inputs: 4096 events on the watched parent chain and on random nodes, dirty-all,
    // a flood of window events, a focus step, a tick.
    let mut rng = 0x9e37_79b9_7f4a_7c15_u64;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        rng
    };
    let parent = parent_of(leaf).unwrap();
    let mut events_hot: Vec<NodeEvent> = (0..4_096)
        .map(|i| NodeEvent::ChildrenChanged {
            node: key(if i % 2 == 0 { parent } else { next() % NODES }),
        })
        .collect();
    events_hot.push(NodeEvent::PropertiesChanged {
        node: key(leaf),
        mask: PropMask::NAME,
    });
    let events_rand: Vec<NodeEvent> = (0..4_096)
        .map(|_| NodeEvent::ChildrenChanged {
            node: key(next() % NODES),
        })
        .collect();
    let mut dirty_all = DirtySet::default();
    dirty_all.mark_all();
    let mut best = u64::MAX;
    for _ in 0..rounds {
        let mut worst = 0u64;
        let mut time = |f: &mut dyn FnMut(&mut FillEffects)| {
            let mut out = FillEffects::new();
            let t = Instant::now();
            f(&mut out);
            worst = worst.max(u64::try_from(t.elapsed().as_micros()).unwrap_or(u64::MAX));
        };
        let cx = mk(Instant::now(), HealthView::Healthy, Some(key(leaf)));
        time(&mut |o| p.step(&cx, FillInput::Events(&events_hot), o));
        time(&mut |o| p.step(&cx, FillInput::Events(&events_rand), o));
        time(&mut |o| p.step(&cx, FillInput::Resync { dirty: &dirty_all }, o));
        time(&mut |o| p.step(&cx, FillInput::FocusSettled { node: key(leaf) }, o));
        time(&mut |o| {
            p.step(&cx, FillInput::WindowActivated { window: key(1) }, o);
        });
        time(&mut |o| p.step(&cx, FillInput::Tick, o));
        best = best.min(worst);
    }
    best
}

/// MA-6 at the caps. A release build on a quiet host is the authoritative number; a debug build
/// only has to be within an order of magnitude.
#[test]
fn ma6_the_policy_step_at_the_caps_is_bounded() {
    let best = worst_step_us(7);
    eprintln!("candidate: best-of-7 worst step at the caps = {best} us");
    let budget = if cfg!(debug_assertions) { 5_000 } else { 250 };
    assert!(best <= budget, "{best} us > {budget} us");
}

/// The policy never holds more than its state budget even when every input asks for state.
#[test]
fn policy_state_stays_far_below_the_cap() {
    let (_w, r) = filled_mirror();
    let limits = FillLimits::default();
    let mut p = FocusPrefetch::default();
    let mut out = FillEffects::new();
    let now = Instant::now();
    for leaf in (0..2_000u64).map(|i| NODES - 1 - i * 9) {
        let cx = FillCtx {
            now,
            app: app_id(),
            app_class: AppClass::Unknown,
            health: HealthView::Healthy,
            dump_complete: false,
            mirror: &r,
            focus: Some(key(leaf)),
            slots: SlotView::default(),
            limits: &limits,
        };
        p.step(&cx, FillInput::FocusSettled { node: key(leaf) }, &mut out);
        out.clear();
    }
    assert!(
        p.memory_bytes() < limits.state_max_bytes / 4,
        "{}",
        p.memory_bytes()
    );
}
