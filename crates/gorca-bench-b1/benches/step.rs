// SPDX-License-Identifier: GPL-3.0-or-later
//! H-7 entry point: `cargo bench -p gorca-bench-b1 --bench step`.
//!
//! Writes `step.json` into `$B1_RUN_DIR` (or prints it). Subject selection:
//!
//! * `GORCA_STEP_SUBJECT=candidate` (default): `gorca_mirror::fill::Candidate` of the tree this
//!   bench is built in, through the adapter below. It needs nothing from the arm but the three
//!   facts documented on `crates/gorca-mirror/src/fill/candidate.rs` (`FillPolicy`, `Default`,
//!   `Send + 'static`), so an arm never edits this file;
//! * `GORCA_STEP_SUBJECT=noop`: the harness floor;
//! * `GORCA_STEP_SUBJECT=spin:<us>`: a deliberately slow policy (sensitivity check, must FAIL).
//!
//! What is timed per round is one whole adapter step: the shared invalidation the driver does
//! first, then up to six `policy.step` calls (resync, tick, events, a where-am-I demand, a
//! focus-settled and a window-activated input). That is a deliberately conservative reading of
//! MA-6 ("`step()` at most 250 us"): the sum bounds every single call.

use std::sync::Arc;
use std::time::Instant;

use gorca_bench_b1::step::{
    EventKind, NoopSubject, SpinSubject, StepInput, StepSubject, measure, scenarios,
};
use gorca_ingress::DirtySet;
use gorca_mirror::FillLimits;
use gorca_mirror::fill::{
    AppClass, Candidate, DemandOutcome, DemandWhy, FillCtx, FillEffects, FillInput, FillPolicy,
    HealthView, SlotView,
};
use gorca_mirror::{ApplyOrigin, MirrorHandle, MirrorWriter, mirror};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeEvent, NodeKey,
    NodeSnapshot, PropMask, Relations, Role, SnapshotBatch, StateSet, WindowKind,
};

/// Wiring of an arm's `Candidate` (SPEC 4.4): a mirror filled to the scenario's size, a long
/// ancestor chain at the top (where-am-I at the cap depth), and every step also runs the focus
/// invalidation path and a where-am-I demand over stale ancestors.
struct CandidateSubject {
    policy: Candidate,
    w: MirrorWriter,
    r: MirrorHandle,
    limits: FillLimits,
    app: AppId,
}

fn node_key(app: AppId, i: u32) -> NodeKey {
    NodeKey {
        app,
        local: LocalId(u64::from(i) + 1),
    }
}

impl CandidateSubject {
    fn new() -> Self {
        let (w, r) = mirror();
        Self {
            policy: Candidate::default(),
            w,
            r,
            limits: FillLimits::default(),
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
        }
    }
}

impl StepSubject for CandidateSubject {
    fn name(&self) -> String {
        self.policy.name().to_owned()
    }

    fn prepare(&mut self, nodes: u32) {
        let (w, r) = mirror();
        self.w = w;
        self.r = r;
        self.policy = Candidate::default();
        let app = self.app;
        let have = PropMask::PRESENT | PropMask::PARENT;
        let mut batch = Vec::with_capacity(1_024);
        for i in 0..nodes {
            // A chain of 64 at the top, everything else below its last link.
            let parent = match i {
                0 => None,
                1..=64 => Some(i - 1),
                _ => Some(64 + (i - 65) % 16),
            };
            batch.push(Arc::new(NodeSnapshot {
                key: node_key(app, i),
                role: Role::Button,
                states: StateSet::empty(),
                name: Some(Arc::from("n")),
                description: None,
                value: None,
                parent: parent.map(|p| node_key(app, p)),
                children: ChildList::Unknown,
                interfaces: Ifaces::empty(),
                attrs: AttrMap::default(),
                relations: Relations::default(),
                bounds: None,
                have,
                confirmed_seq: 0,
                fresh: Freshness::Fresh { at: Instant::now() },
                sem: gorca_model::NodeSem::NONE,
            }));
            if batch.len() == 1_024 || i + 1 == nodes {
                let issued_at = self.w.seq();
                self.w.apply(
                    SnapshotBatch::new(std::mem::take(&mut batch)),
                    ApplyOrigin::Reply { issued_at },
                );
            }
        }
    }

    fn step(&mut self, input: &StepInput) -> u64 {
        let app = self.app;
        let now = Instant::now();
        let deepest = node_key(app, 64);
        let focus = node_key(app, 3);
        let events: Vec<NodeEvent> = input
            .events
            .iter()
            .map(|e| {
                let node = node_key(app, e.node);
                match e.kind {
                    EventKind::Properties => NodeEvent::PropertiesChanged {
                        node,
                        mask: PropMask::NAME,
                    },
                    EventKind::Children => NodeEvent::ChildrenChanged { node },
                    EventKind::Removed => NodeEvent::NodeRemoved { node },
                    EventKind::Text => NodeEvent::SelectionChanged { node },
                    EventKind::Window => NodeEvent::WindowEvent {
                        node,
                        kind: WindowKind::Activate,
                    },
                }
            })
            .collect();
        // What the driver does before the policy: the shared invalidation.
        self.w.invalidate(focus, PropMask::NAME, now);
        if input.dirty_all {
            self.w.invalidate_all(now);
        }
        let dirty = DirtySet::default();
        let cx = FillCtx {
            now,
            app,
            app_class: AppClass::Unknown,
            health: HealthView::Healthy,
            dump_complete: false,
            mirror: &self.r,
            focus: Some(focus),
            slots: SlotView::default(),
            limits: &self.limits,
        };
        let mut out = FillEffects::new();
        if input.dirty_all {
            self.policy
                .step(&cx, FillInput::Resync { dirty: &dirty }, &mut out);
        }
        if input.tick {
            self.policy.step(&cx, FillInput::Tick, &mut out);
        }
        if !events.is_empty() {
            self.policy.step(&cx, FillInput::Events(&events), &mut out);
        }
        self.policy.step(
            &cx,
            FillInput::Demand {
                node: deepest,
                why: DemandWhy::WhereAmI,
                outcome: DemandOutcome::Hit,
            },
            &mut out,
        );
        self.policy
            .step(&cx, FillInput::FocusSettled { node: focus }, &mut out);
        self.policy.step(
            &cx,
            FillInput::WindowActivated {
                window: node_key(app, 0),
            },
            &mut out,
        );
        out.len() as u64
    }
}

fn main() {
    let which = std::env::var("GORCA_STEP_SUBJECT").unwrap_or_else(|_| "candidate".into());
    let mut subject: Box<dyn StepSubject> = match which.split_once(':') {
        Some(("spin", us)) => Box::new(SpinSubject {
            us: us.parse().unwrap_or(400),
        }),
        None if which == "noop" => Box::new(NoopSubject),
        _ => Box::new(CandidateSubject::new()),
    };
    let quick = std::env::args().any(|a| a == "--quick");
    let (rounds, iters) = if quick { (7, 20) } else { (7, 400) };
    let r = measure(subject.as_mut(), &scenarios(0xB1), rounds, iters, 20);
    let json = r.json();
    match std::env::var("B1_RUN_DIR") {
        Ok(d) => {
            if let Err(e) = std::fs::write(std::path::Path::new(&d).join("step.json"), &json) {
                eprintln!("step: cannot write step.json: {e}");
                std::process::exit(1);
            }
        }
        Err(_) => print!("{json}"),
    }
    eprintln!(
        "step[{}]: median over {rounds} rounds of the worst step = {:.1} us (gate 250 us)",
        r.subject,
        r.median_max_us()
    );
}
