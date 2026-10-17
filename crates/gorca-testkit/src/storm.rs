// SPDX-License-Identifier: GPL-3.0-or-later
//! Deterministic event storm generator (M-3, M-11 harness input). xorshift64*, no `rand`
//! dependency, so a seed reproduces a run exactly.

use gorca_model::{AppId, CaretOrigin, LocalId, NodeEvent, NodeKey, PropMask, TextDelta};

#[derive(Clone, Debug)]
pub struct StormGen {
    state: u64,
    app: AppId,
    nodes: u64,
}

impl StormGen {
    /// `nodes` distinct nodes (at least 1) in `app`.
    pub fn new(seed: u64, app: AppId, nodes: u64) -> Self {
        Self {
            state: seed.max(1),
            app,
            nodes: nodes.max(1),
        }
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn node(&mut self) -> NodeKey {
        let n = self.next_u64() % self.nodes;
        NodeKey {
            app: self.app,
            local: LocalId(n),
        }
    }

    /// One event. The mix mimics a busy terminal plus UI noise: mostly text and property
    /// churn, occasional caret, children, focus and removal.
    pub fn next_event(&mut self) -> NodeEvent {
        let r = self.next_u64() % 100;
        let node = self.node();
        match r {
            0..=39 => {
                let offset = u32::try_from(self.next_u64() % 4096).unwrap_or(0);
                if r.is_multiple_of(2) {
                    NodeEvent::TextChanged {
                        node,
                        delta: TextDelta::Insert {
                            offset,
                            len: 1,
                            text: None,
                        },
                    }
                } else {
                    NodeEvent::TextChanged {
                        node,
                        delta: TextDelta::Delete {
                            offset,
                            len: 1,
                            text: None,
                        },
                    }
                }
            }
            40..=64 => NodeEvent::PropertiesChanged {
                node,
                mask: PropMask::NAME,
            },
            65..=79 => NodeEvent::CaretMoved {
                node,
                offset: 0,
                origin: CaretOrigin::Unknown,
            },
            80..=89 => NodeEvent::ChildrenChanged { node },
            90..=95 => NodeEvent::SelectionChanged { node },
            96..=98 => NodeEvent::FocusChanged { node },
            _ => NodeEvent::NodeRemoved { node },
        }
    }
}

impl Iterator for StormGen {
    type Item = NodeEvent;

    fn next(&mut self) -> Option<NodeEvent> {
        Some(self.next_event())
    }
}

/// `count` events for `app` over `nodes` distinct nodes.
pub fn storm(seed: u64, app: AppId, nodes: u64, count: usize) -> Vec<NodeEvent> {
    StormGen::new(seed, app, nodes).take(count).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::ConnectorId;

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: 1,
        }
    }

    #[test]
    fn same_seed_same_storm() {
        let a = storm(7, app(), 100, 500);
        let b = storm(7, app(), 100, 500);
        let c = storm(8, app(), 100, 500);
        let fmt = |v: &[NodeEvent]| format!("{v:?}");
        assert_eq!(fmt(&a), fmt(&b));
        assert_ne!(fmt(&a), fmt(&c));
    }

    #[test]
    fn events_stay_within_the_node_range_and_cover_the_mix() {
        let evs = storm(1, app(), 10, 5000);
        assert!(evs.iter().all(|e| e.node().is_some_and(|n| n.local.0 < 10)));
        let kinds: std::collections::HashSet<_> = evs.iter().map(std::mem::discriminant).collect();
        assert!(kinds.len() >= 6, "{} kinds", kinds.len());
    }
}
