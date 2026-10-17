// SPDX-License-Identifier: GPL-3.0-or-later
//! A policy for tests of the **shared** fill code (driver, store, connector): it asks for
//! everything, as often as it may, and leaves refusing, rate limiting and eligibility to the
//! driver.
//!
//! The null policy never requests anything, so the conformance cases that exercise shared code
//! reached by requests (hidden ghosts, dump eligibility) cannot run against it. This one is wired
//! only by tests (`gorca-daemon/tests/b1_ic.rs`); no product binary names it.

use std::time::Duration;

use gorca_model::{NodeKey, PropMask};
use gorca_rt::BudgetClass;

use super::{FillCtx, FillEffect, FillEffects, FillInput, FillOp, FillPolicy};

/// How often an idle app is asked again.
const AGAIN: Duration = Duration::from_millis(500);
/// Nodes asked for per dump request.
const DUMP: u32 = 5_000;
/// Focused nodes remembered and refetched.
const WATCH: usize = 32;

/// `CacheDump` on readiness and on every tick, and a refetch of every node the user has focused
/// on every tick (what a policy that keeps its prefetched nodes fresh would do).
#[derive(Clone, Debug, Default)]
pub struct PrefetchAllPolicy {
    watched: Vec<NodeKey>,
}

impl FillPolicy for PrefetchAllPolicy {
    fn name(&self) -> &'static str {
        "test-prefetch-all"
    }

    fn step(&mut self, cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects) {
        match input {
            FillInput::Ready => {
                out.push(FillEffect::Request {
                    op: FillOp::CacheDump { max_nodes: DUMP },
                    class: BudgetClass::Background,
                });
                out.push(FillEffect::WakeAt(cx.now + AGAIN));
            }
            FillInput::FocusSettled { node } => {
                if !self.watched.contains(&node) && self.watched.len() < WATCH {
                    self.watched.push(node);
                }
            }
            FillInput::Tick => {
                out.push(FillEffect::Request {
                    op: FillOp::CacheDump { max_nodes: DUMP },
                    class: BudgetClass::Background,
                });
                for node in &self.watched {
                    out.push(FillEffect::Request {
                        op: FillOp::Fetch {
                            node: *node,
                            want: PropMask::PRESENT,
                        },
                        class: BudgetClass::Background,
                    });
                }
                out.push(FillEffect::WakeAt(cx.now + AGAIN));
            }
            FillInput::Appeared { .. }
            | FillInput::Events(_)
            | FillInput::Resync { .. }
            | FillInput::Demand { .. }
            | FillInput::WindowActivated { .. }
            | FillInput::Reply { .. } => {}
        }
    }

    fn memory_bytes(&self) -> usize {
        self.watched.capacity() * std::mem::size_of::<NodeKey>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fill::{AppClass, HealthView, SlotView};
    use crate::limits::FillLimits;
    use crate::store::mirror;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};
    use std::time::Instant;

    #[test]
    fn it_dumps_on_readiness_and_refetches_what_was_focused_on_every_tick() {
        let (_w, r) = mirror();
        let limits = FillLimits::default();
        let app = AppId {
            connector: ConnectorId(0),
            serial: 1,
        };
        let cx = FillCtx {
            now: Instant::now(),
            app,
            app_class: AppClass::Unknown,
            health: HealthView::Healthy,
            dump_complete: false,
            mirror: &r,
            focus: None,
            slots: SlotView::default(),
            limits: &limits,
        };
        let mut p = PrefetchAllPolicy::default();
        let mut out = FillEffects::new();
        p.step(&cx, FillInput::Ready, &mut out);
        assert!(matches!(
            &out[0],
            FillEffect::Request {
                op: FillOp::CacheDump { .. },
                class: BudgetClass::Background
            }
        ));
        out.clear();
        let node = NodeKey {
            app,
            local: LocalId(7),
        };
        p.step(&cx, FillInput::FocusSettled { node }, &mut out);
        p.step(&cx, FillInput::FocusSettled { node }, &mut out);
        assert!(out.is_empty());
        p.step(&cx, FillInput::Tick, &mut out);
        let fetches = out
            .iter()
            .filter(|e| {
                matches!(e, FillEffect::Request { op: FillOp::Fetch { node: n, .. }, .. } if *n == node)
            })
            .count();
        assert_eq!(fetches, 1, "a node is watched once");
        assert!(matches!(out.last(), Some(FillEffect::WakeAt(_))));
        assert!(p.memory_bytes() >= std::mem::size_of::<NodeKey>());
    }
}
