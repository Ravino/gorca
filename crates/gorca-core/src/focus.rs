// SPDX-License-Identifier: GPL-3.0-or-later
//! Focus state machine (ADR-0001 section 7.1).

use std::time::{Duration, Instant};

use gorca_model::{AppId, Epoch, NodeKey, Role};

/// δ for roles known to flap (menu, menu item, popover). Extensions can tune it (M-6).
pub const FOCUS_FLAP_DELAY: Duration = Duration::from_millis(30);

/// δ is 0 ms by default and 30 ms for roles known to flap.
pub fn settle_delay(role: Option<Role>) -> Duration {
    match role {
        Some(r) if r.flaps_on_focus() => FOCUS_FLAP_DELAY,
        Some(_) | None => Duration::ZERO,
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FocusState {
    NoFocus,
    /// Debounces flapping focus.
    Settling {
        candidate: NodeKey,
        epoch: Epoch,
        until: Instant,
    },
    Focused {
        node: NodeKey,
        epoch: Epoch,
        presented: bool,
    },
    /// The focused node was removed or its app is gone.
    Orphaned {
        last: NodeKey,
        epoch: Epoch,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FocusMsg {
    /// The Reader has already advanced the `EpochCell`: `epoch` is the new value, so stale
    /// speech cancels immediately through E-2, not after δ.
    FocusChanged {
        node: NodeKey,
        epoch: Epoch,
        until: Instant,
    },
    /// The settle timer fired.
    TimerElapsed {
        now: Instant,
    },
    /// A cache hit or a reply made the node presentable.
    Presented {
        node: NodeKey,
    },
    NodeRemoved(NodeKey),
    AppGone(AppId),
    /// The focus app went `Hung`.
    AppHung(AppId),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FocusEffect {
    /// Mirror read (lock-free); on a miss `Fetch(node, PRESENT_MASK)` plus ancestry
    /// (Interactive).
    BeginPresent { node: NodeKey, epoch: Epoch },
    /// `SpeechIntent(Interrupt, epoch)`.
    Speak { node: NodeKey, epoch: Epoch },
    /// Query the active window's focus (Interactive).
    QueryActiveWindowFocus,
    /// Speak "not responding" once per Hung episode.
    AnnounceNotResponding,
}

impl FocusState {
    /// Pure transition function. Every (state, message) pair is spelled out.
    pub fn step(self, msg: FocusMsg) -> (FocusState, Option<FocusEffect>) {
        match self {
            FocusState::NoFocus => no_focus(msg),
            FocusState::Settling {
                candidate,
                epoch,
                until,
            } => settling(candidate, epoch, until, msg),
            FocusState::Focused {
                node,
                epoch,
                presented,
            } => focused(node, epoch, presented, msg),
            FocusState::Orphaned { last, epoch } => orphaned(last, epoch, msg),
        }
    }
}

fn settle(node: NodeKey, epoch: Epoch, until: Instant) -> (FocusState, Option<FocusEffect>) {
    (
        FocusState::Settling {
            candidate: node,
            epoch,
            until,
        },
        None,
    )
}

fn no_focus(msg: FocusMsg) -> (FocusState, Option<FocusEffect>) {
    match msg {
        FocusMsg::FocusChanged { node, epoch, until } => settle(node, epoch, until),
        FocusMsg::TimerElapsed { .. }
        | FocusMsg::Presented { .. }
        | FocusMsg::NodeRemoved(_)
        | FocusMsg::AppGone(_)
        | FocusMsg::AppHung(_) => (FocusState::NoFocus, None),
    }
}

fn settling(
    candidate: NodeKey,
    epoch: Epoch,
    until: Instant,
    msg: FocusMsg,
) -> (FocusState, Option<FocusEffect>) {
    let same = FocusState::Settling {
        candidate,
        epoch,
        until,
    };
    match msg {
        FocusMsg::FocusChanged { node, epoch, until } => settle(node, epoch, until),
        FocusMsg::TimerElapsed { now } => {
            if now >= until {
                (
                    FocusState::Focused {
                        node: candidate,
                        epoch,
                        presented: false,
                    },
                    Some(FocusEffect::BeginPresent {
                        node: candidate,
                        epoch,
                    }),
                )
            } else {
                (same, None)
            }
        }
        FocusMsg::Presented { .. } | FocusMsg::AppHung(_) => (same, None),
        FocusMsg::NodeRemoved(n) => {
            if n == candidate {
                (
                    FocusState::Orphaned {
                        last: candidate,
                        epoch,
                    },
                    Some(FocusEffect::QueryActiveWindowFocus),
                )
            } else {
                (same, None)
            }
        }
        FocusMsg::AppGone(app) => {
            if candidate.app == app {
                (
                    FocusState::Orphaned {
                        last: candidate,
                        epoch,
                    },
                    Some(FocusEffect::QueryActiveWindowFocus),
                )
            } else {
                (same, None)
            }
        }
    }
}

fn focused(
    node: NodeKey,
    epoch: Epoch,
    presented: bool,
    msg: FocusMsg,
) -> (FocusState, Option<FocusEffect>) {
    let same = FocusState::Focused {
        node,
        epoch,
        presented,
    };
    match msg {
        FocusMsg::FocusChanged { node, epoch, until } => settle(node, epoch, until),
        FocusMsg::TimerElapsed { .. } => (same, None),
        FocusMsg::Presented { node: n } => {
            if n == node && !presented {
                (
                    FocusState::Focused {
                        node,
                        epoch,
                        presented: true,
                    },
                    Some(FocusEffect::Speak { node, epoch }),
                )
            } else {
                (same, None)
            }
        }
        FocusMsg::NodeRemoved(n) => {
            if n == node {
                (
                    FocusState::Orphaned { last: node, epoch },
                    Some(FocusEffect::QueryActiveWindowFocus),
                )
            } else {
                (same, None)
            }
        }
        FocusMsg::AppGone(app) => {
            if node.app == app {
                (
                    FocusState::Orphaned { last: node, epoch },
                    Some(FocusEffect::QueryActiveWindowFocus),
                )
            } else {
                (same, None)
            }
        }
        FocusMsg::AppHung(app) => {
            if node.app == app {
                (same, Some(FocusEffect::AnnounceNotResponding))
            } else {
                (same, None)
            }
        }
    }
}

fn orphaned(last: NodeKey, epoch: Epoch, msg: FocusMsg) -> (FocusState, Option<FocusEffect>) {
    let same = FocusState::Orphaned { last, epoch };
    match msg {
        FocusMsg::FocusChanged { node, epoch, until } => settle(node, epoch, until),
        FocusMsg::TimerElapsed { .. }
        | FocusMsg::Presented { .. }
        | FocusMsg::NodeRemoved(_)
        | FocusMsg::AppGone(_)
        | FocusMsg::AppHung(_) => (same, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{ConnectorId, LocalId};

    fn app(n: u32) -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: n,
        }
    }

    fn key(a: u32, n: u64) -> NodeKey {
        NodeKey {
            app: app(a),
            local: LocalId(n),
        }
    }

    fn changed(node: NodeKey, e: u64, until: Instant) -> FocusMsg {
        FocusMsg::FocusChanged {
            node,
            epoch: Epoch(e),
            until,
        }
    }

    #[test]
    fn focus_settles_then_presents_then_speaks() {
        let t = Instant::now();
        let (s, e) = FocusState::NoFocus.step(changed(key(1, 1), 1, t));
        assert_eq!(
            s,
            FocusState::Settling {
                candidate: key(1, 1),
                epoch: Epoch(1),
                until: t
            }
        );
        assert_eq!(e, None);

        let (s, e) = s.step(FocusMsg::TimerElapsed { now: t });
        assert_eq!(
            s,
            FocusState::Focused {
                node: key(1, 1),
                epoch: Epoch(1),
                presented: false
            }
        );
        assert_eq!(
            e,
            Some(FocusEffect::BeginPresent {
                node: key(1, 1),
                epoch: Epoch(1)
            })
        );

        let (s, e) = s.step(FocusMsg::Presented { node: key(1, 1) });
        assert_eq!(
            s,
            FocusState::Focused {
                node: key(1, 1),
                epoch: Epoch(1),
                presented: true
            }
        );
        assert_eq!(
            e,
            Some(FocusEffect::Speak {
                node: key(1, 1),
                epoch: Epoch(1)
            })
        );

        // Speaking happens once.
        let (s2, e) = s.step(FocusMsg::Presented { node: key(1, 1) });
        assert_eq!(s2, s);
        assert_eq!(e, None);
    }

    #[test]
    fn flapping_focus_restarts_the_settle_timer() {
        let t = Instant::now();
        let later = t + FOCUS_FLAP_DELAY;
        let (s, _) = FocusState::NoFocus.step(changed(key(1, 1), 1, later));
        let (s, _) = s.step(changed(key(1, 2), 2, later));
        // Too early: stays settling on the newest candidate.
        let (s, e) = s.step(FocusMsg::TimerElapsed { now: t });
        assert_eq!(
            s,
            FocusState::Settling {
                candidate: key(1, 2),
                epoch: Epoch(2),
                until: later
            }
        );
        assert_eq!(e, None);
        let (s, _) = s.step(FocusMsg::TimerElapsed { now: later });
        assert!(matches!(s, FocusState::Focused { node, .. } if node == key(1, 2)));
    }

    #[test]
    fn presented_for_a_different_node_is_ignored() {
        let s = FocusState::Focused {
            node: key(1, 1),
            epoch: Epoch(1),
            presented: false,
        };
        let (s2, e) = s.step(FocusMsg::Presented { node: key(1, 9) });
        assert_eq!(s2, s);
        assert_eq!(e, None);
    }

    #[test]
    fn removal_orphans_and_queries_the_active_window() {
        let s = FocusState::Focused {
            node: key(1, 1),
            epoch: Epoch(3),
            presented: true,
        };
        let (s, e) = s.step(FocusMsg::NodeRemoved(key(1, 1)));
        assert_eq!(
            s,
            FocusState::Orphaned {
                last: key(1, 1),
                epoch: Epoch(3)
            }
        );
        assert_eq!(e, Some(FocusEffect::QueryActiveWindowFocus));
        // Unrelated removals do not.
        let f = FocusState::Focused {
            node: key(1, 1),
            epoch: Epoch(3),
            presented: true,
        };
        assert_eq!(f.step(FocusMsg::NodeRemoved(key(1, 2))).0, f);
    }

    #[test]
    fn app_gone_orphans_only_its_own_focus() {
        let f = FocusState::Focused {
            node: key(1, 1),
            epoch: Epoch(3),
            presented: true,
        };
        assert_eq!(f.step(FocusMsg::AppGone(app(2))).0, f);
        assert!(matches!(
            f.step(FocusMsg::AppGone(app(1))).0,
            FocusState::Orphaned { .. }
        ));
        let s = FocusState::Settling {
            candidate: key(1, 1),
            epoch: Epoch(3),
            until: Instant::now(),
        };
        assert!(matches!(
            s.step(FocusMsg::AppGone(app(1))).0,
            FocusState::Orphaned { .. }
        ));
    }

    #[test]
    fn orphaned_recovers_on_the_next_focus() {
        let t = Instant::now();
        let o = FocusState::Orphaned {
            last: key(1, 1),
            epoch: Epoch(3),
        };
        assert!(matches!(
            o.step(changed(key(2, 5), 4, t)).0,
            FocusState::Settling { .. }
        ));
    }

    #[test]
    fn hung_focus_app_announces_without_changing_state() {
        let f = FocusState::Focused {
            node: key(1, 1),
            epoch: Epoch(3),
            presented: true,
        };
        let (s, e) = f.step(FocusMsg::AppHung(app(1)));
        assert_eq!(s, f);
        assert_eq!(e, Some(FocusEffect::AnnounceNotResponding));
        assert_eq!(f.step(FocusMsg::AppHung(app(2))).1, None);
    }

    #[test]
    fn settle_delay_is_zero_except_for_flapping_roles() {
        assert_eq!(settle_delay(None), Duration::ZERO);
        assert_eq!(settle_delay(Some(Role::Button)), Duration::ZERO);
        assert_eq!(settle_delay(Some(Role::MenuItem)), FOCUS_FLAP_DELAY);
    }
}
