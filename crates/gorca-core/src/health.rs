//! Per-app health state machine (ADR-0001 section 6.4).

use std::time::{Duration, Instant};

use gorca_ingress::DirtySet;
use gorca_rt::BudgetClass;
use smallvec::{SmallVec, smallvec};

/// ">= 3 timeouts / 2 s" window.
pub const SLOW_WINDOW: Duration = Duration::from_secs(2);
/// "Slow: 10 s without timeout -> Healthy".
pub const SLOW_RECOVERY: Duration = Duration::from_secs(10);
/// Probe backoff 250 ms to 4 s.
pub const HUNG_PROBE_MIN: Duration = Duration::from_millis(250);
pub const HUNG_PROBE_MAX: Duration = Duration::from_secs(4);
/// Consecutive timeouts that make an app `Hung`.
const HUNG_AFTER: u8 = 3;

/// ADR-0001 section 6.4. Deviations, all needed by the transition table:
/// `Slow` also tracks `consecutive` and `last_timeout` ("3 consecutive timeouts", "10 s without
/// timeout"); `Hung` tracks the probe `backoff` and `probe_in_flight` (I-4: at most one probe).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AppHealth {
    Healthy,
    Slow {
        timeouts: u8,
        window_start: Instant,
        consecutive: u8,
        last_timeout: Instant,
    },
    Hung {
        since: Instant,
        probe_at: Instant,
        backoff: Duration,
        probe_in_flight: bool,
    },
    Flooded {
        since: Instant,
        dirty: DirtySet,
    },
    Gone,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HealthMsg {
    /// A request to the app timed out.
    Timeout {
        class: BudgetClass,
    },
    /// A request succeeded.
    Success,
    /// Periodic timer.
    Tick,
    ProbeOk,
    ProbeTimeout,
    /// The app's ingress went over its watermark (R-3); carries the dirty set built so far.
    IngressOverWatermark {
        dirty: DirtySet,
    },
    /// Global `Shed` (D-2). The app is a candidate if not focused and in the top three by ingress
    /// rate.
    GlobalShed {
        focused: bool,
        top3_by_rate: bool,
    },
    BacklogDrained,
    /// Owner vanished or the connector reported `Gone`.
    OwnerVanished,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HealthEffect {
    /// Halve Prefetch concurrency.
    HalveConcurrency,
    RestoreConcurrency,
    /// Reject Prefetch and Background requests; Interactive is answered from cache only.
    RestrictToCache,
    /// Announce "application not responding" if the app is focused (the Reader decides).
    AnnounceNotResponding,
    /// One cheap probe (`GetRole` on the app root), Interactive deadline.
    SendProbe,
    /// D-5 per-app event deregistration, if supported (P-11).
    DeregisterEvents,
    ReRegisterEvents,
    /// Lazy resync of the dirty set: nodes the Reader asks for, plus focus ancestry.
    Resync(DirtySet),
    TombstoneAll,
    /// The Reader gets `AppLifecycle::Gone`.
    NotifyGone,
}

pub type HealthEffects = SmallVec<[HealthEffect; 3]>;

#[derive(Copy, Clone, Debug)]
pub struct HealthStepCtx {
    pub now: Instant,
}

impl AppHealth {
    /// Pure transition function. Every (state, message) pair is spelled out.
    pub fn step(self, msg: HealthMsg, ctx: &HealthStepCtx) -> (AppHealth, HealthEffects) {
        match self {
            AppHealth::Healthy => healthy(msg, ctx),
            AppHealth::Slow {
                timeouts,
                window_start,
                consecutive,
                last_timeout,
            } => slow(timeouts, window_start, consecutive, last_timeout, msg, ctx),
            AppHealth::Hung {
                since,
                probe_at,
                backoff,
                probe_in_flight,
            } => hung(since, probe_at, backoff, probe_in_flight, &msg, ctx),
            AppHealth::Flooded { since, dirty } => flooded(since, dirty, msg),
            AppHealth::Gone => (AppHealth::Gone, smallvec![]),
        }
    }

    /// May a request (other than a probe) be sent right now? I-4: a `Hung` app receives no
    /// request except a probe; Interactive needs are answered from the mirror.
    pub fn admits_requests(&self) -> bool {
        match self {
            AppHealth::Healthy | AppHealth::Slow { .. } | AppHealth::Flooded { .. } => true,
            AppHealth::Hung { .. } | AppHealth::Gone => false,
        }
    }
}

fn owner_vanished() -> (AppHealth, HealthEffects) {
    (
        AppHealth::Gone,
        smallvec![HealthEffect::TombstoneAll, HealthEffect::NotifyGone],
    )
}

fn enter_hung(ctx: &HealthStepCtx) -> (AppHealth, HealthEffects) {
    (
        AppHealth::Hung {
            since: ctx.now,
            probe_at: ctx.now + HUNG_PROBE_MIN,
            backoff: HUNG_PROBE_MIN,
            probe_in_flight: false,
        },
        smallvec![
            HealthEffect::RestrictToCache,
            HealthEffect::AnnounceNotResponding
        ],
    )
}

fn healthy(msg: HealthMsg, ctx: &HealthStepCtx) -> (AppHealth, HealthEffects) {
    match msg {
        HealthMsg::Timeout { .. } => (
            AppHealth::Slow {
                timeouts: 1,
                window_start: ctx.now,
                consecutive: 1,
                last_timeout: ctx.now,
            },
            smallvec![HealthEffect::HalveConcurrency],
        ),
        HealthMsg::Success
        | HealthMsg::Tick
        | HealthMsg::ProbeOk
        | HealthMsg::ProbeTimeout
        | HealthMsg::BacklogDrained => (AppHealth::Healthy, smallvec![]),
        HealthMsg::IngressOverWatermark { dirty } => (
            AppHealth::Flooded {
                since: ctx.now,
                dirty,
            },
            smallvec![],
        ),
        HealthMsg::GlobalShed {
            focused,
            top3_by_rate,
        } => shed(AppHealth::Healthy, focused, top3_by_rate, ctx),
        HealthMsg::OwnerVanished => owner_vanished(),
    }
}

fn shed(
    same: AppHealth,
    focused: bool,
    top3_by_rate: bool,
    ctx: &HealthStepCtx,
) -> (AppHealth, HealthEffects) {
    if !focused && top3_by_rate {
        (
            AppHealth::Flooded {
                since: ctx.now,
                dirty: DirtySet::default(),
            },
            smallvec![HealthEffect::DeregisterEvents],
        )
    } else {
        (same, smallvec![])
    }
}

fn slow(
    timeouts: u8,
    window_start: Instant,
    consecutive: u8,
    last_timeout: Instant,
    msg: HealthMsg,
    ctx: &HealthStepCtx,
) -> (AppHealth, HealthEffects) {
    let same = AppHealth::Slow {
        timeouts,
        window_start,
        consecutive,
        last_timeout,
    };
    match msg {
        HealthMsg::Timeout { .. } => {
            let consecutive = consecutive.saturating_add(1);
            if consecutive >= HUNG_AFTER {
                return enter_hung(ctx);
            }
            let (timeouts, window_start) =
                if ctx.now.saturating_duration_since(window_start) > SLOW_WINDOW {
                    (1, ctx.now)
                } else {
                    (timeouts.saturating_add(1), window_start)
                };
            (
                AppHealth::Slow {
                    timeouts,
                    window_start,
                    consecutive,
                    last_timeout: ctx.now,
                },
                smallvec![],
            )
        }
        HealthMsg::Success => (
            AppHealth::Slow {
                timeouts,
                window_start,
                consecutive: 0,
                last_timeout,
            },
            smallvec![],
        ),
        HealthMsg::Tick => {
            if ctx.now.saturating_duration_since(last_timeout) >= SLOW_RECOVERY {
                (
                    AppHealth::Healthy,
                    smallvec![HealthEffect::RestoreConcurrency],
                )
            } else {
                (same, smallvec![])
            }
        }
        HealthMsg::ProbeOk | HealthMsg::ProbeTimeout | HealthMsg::BacklogDrained => {
            (same, smallvec![])
        }
        HealthMsg::IngressOverWatermark { dirty } => (
            AppHealth::Flooded {
                since: ctx.now,
                dirty,
            },
            smallvec![],
        ),
        HealthMsg::GlobalShed {
            focused,
            top3_by_rate,
        } => shed(same, focused, top3_by_rate, ctx),
        HealthMsg::OwnerVanished => owner_vanished(),
    }
}

fn hung(
    since: Instant,
    probe_at: Instant,
    backoff: Duration,
    probe_in_flight: bool,
    msg: &HealthMsg,
    ctx: &HealthStepCtx,
) -> (AppHealth, HealthEffects) {
    let same = AppHealth::Hung {
        since,
        probe_at,
        backoff,
        probe_in_flight,
    };
    match msg {
        HealthMsg::Tick => {
            if !probe_in_flight && ctx.now >= probe_at {
                (
                    AppHealth::Hung {
                        since,
                        probe_at,
                        backoff,
                        probe_in_flight: true,
                    },
                    smallvec![HealthEffect::SendProbe],
                )
            } else {
                (same, smallvec![])
            }
        }
        HealthMsg::ProbeOk => (
            AppHealth::Healthy,
            smallvec![HealthEffect::RestoreConcurrency],
        ),
        HealthMsg::ProbeTimeout => {
            let backoff = backoff.saturating_mul(2).min(HUNG_PROBE_MAX);
            (
                AppHealth::Hung {
                    since,
                    probe_at: ctx.now + backoff,
                    backoff,
                    probe_in_flight: false,
                },
                smallvec![],
            )
        }
        HealthMsg::Timeout { .. }
        | HealthMsg::Success
        | HealthMsg::IngressOverWatermark { .. }
        | HealthMsg::GlobalShed { .. }
        | HealthMsg::BacklogDrained => (same, smallvec![]),
        HealthMsg::OwnerVanished => owner_vanished(),
    }
}

fn flooded(since: Instant, dirty: DirtySet, msg: HealthMsg) -> (AppHealth, HealthEffects) {
    match msg {
        HealthMsg::BacklogDrained => (
            AppHealth::Healthy,
            smallvec![HealthEffect::Resync(dirty), HealthEffect::ReRegisterEvents],
        ),
        HealthMsg::OwnerVanished => owner_vanished(),
        HealthMsg::IngressOverWatermark { dirty: more } => {
            let mut dirty = dirty;
            if more.is_all() {
                dirty.mark_all();
            } else {
                for n in more.iter() {
                    dirty.mark(n);
                }
            }
            (AppHealth::Flooded { since, dirty }, smallvec![])
        }
        HealthMsg::Timeout { .. }
        | HealthMsg::Success
        | HealthMsg::Tick
        | HealthMsg::ProbeOk
        | HealthMsg::ProbeTimeout
        | HealthMsg::GlobalShed { .. } => (AppHealth::Flooded { since, dirty }, smallvec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};

    fn ctx(now: Instant) -> HealthStepCtx {
        HealthStepCtx { now }
    }

    fn timeout() -> HealthMsg {
        HealthMsg::Timeout {
            class: BudgetClass::Interactive,
        }
    }

    fn node(n: u64) -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(n),
        }
    }

    #[test]
    fn healthy_timeout_goes_slow_and_halves_prefetch() {
        let t = Instant::now();
        let (s, e) = AppHealth::Healthy.step(timeout(), &ctx(t));
        assert!(matches!(
            s,
            AppHealth::Slow {
                timeouts: 1,
                consecutive: 1,
                ..
            }
        ));
        assert_eq!(e.as_slice(), &[HealthEffect::HalveConcurrency]);
    }

    #[test]
    fn three_consecutive_timeouts_make_the_app_hung() {
        let t = Instant::now();
        let (s, _) = AppHealth::Healthy.step(timeout(), &ctx(t));
        let (s, e) = s.step(timeout(), &ctx(t));
        assert!(matches!(s, AppHealth::Slow { consecutive: 2, .. }));
        assert!(e.is_empty());
        let (s, e) = s.step(timeout(), &ctx(t));
        assert_eq!(
            s,
            AppHealth::Hung {
                since: t,
                probe_at: t + HUNG_PROBE_MIN,
                backoff: HUNG_PROBE_MIN,
                probe_in_flight: false
            }
        );
        assert!(e.contains(&HealthEffect::RestrictToCache));
        assert!(e.contains(&HealthEffect::AnnounceNotResponding));
    }

    #[test]
    fn a_success_breaks_the_consecutive_run() {
        let t = Instant::now();
        let (s, _) = AppHealth::Healthy.step(timeout(), &ctx(t));
        let (s, _) = s.step(timeout(), &ctx(t));
        let (s, _) = s.step(HealthMsg::Success, &ctx(t));
        let (s, _) = s.step(timeout(), &ctx(t));
        assert!(matches!(s, AppHealth::Slow { consecutive: 1, .. }));
    }

    #[test]
    fn slow_recovers_after_ten_quiet_seconds() {
        let t = Instant::now();
        let (s, _) = AppHealth::Healthy.step(timeout(), &ctx(t));
        let (s, e) = s.step(HealthMsg::Tick, &ctx(t + Duration::from_secs(9)));
        assert!(matches!(s, AppHealth::Slow { .. }) && e.is_empty());
        let (s, e) = s.step(HealthMsg::Tick, &ctx(t + Duration::from_secs(10)));
        assert_eq!(s, AppHealth::Healthy);
        assert_eq!(e.as_slice(), &[HealthEffect::RestoreConcurrency]);
    }

    #[test]
    fn hung_probes_one_at_a_time_with_exponential_backoff() {
        let t = Instant::now();
        let h = AppHealth::Hung {
            since: t,
            probe_at: t + HUNG_PROBE_MIN,
            backoff: HUNG_PROBE_MIN,
            probe_in_flight: false,
        };
        // Not due yet.
        let (h, e) = h.step(HealthMsg::Tick, &ctx(t));
        assert!(e.is_empty());
        // Due: one probe. A second tick while it is in flight sends nothing (I-4).
        let due = t + HUNG_PROBE_MIN;
        let (h, e) = h.step(HealthMsg::Tick, &ctx(due));
        assert_eq!(e.as_slice(), &[HealthEffect::SendProbe]);
        let (h, e) = h.step(HealthMsg::Tick, &ctx(due + Duration::from_secs(1)));
        assert!(e.is_empty());
        // Timeout doubles the backoff, capped at 4 s.
        let (mut h, _) = h.step(HealthMsg::ProbeTimeout, &ctx(due));
        for _ in 0..6 {
            h = h.step(HealthMsg::ProbeTimeout, &ctx(due)).0;
        }
        assert!(matches!(h, AppHealth::Hung { backoff, .. } if backoff == HUNG_PROBE_MAX));
        let (h, e) = h.step(HealthMsg::ProbeOk, &ctx(due));
        assert_eq!(h, AppHealth::Healthy);
        assert_eq!(e.as_slice(), &[HealthEffect::RestoreConcurrency]);
    }

    #[test]
    fn hung_apps_admit_no_requests() {
        let t = Instant::now();
        let h = AppHealth::Hung {
            since: t,
            probe_at: t,
            backoff: HUNG_PROBE_MIN,
            probe_in_flight: false,
        };
        assert!(!h.admits_requests());
        assert!(!AppHealth::Gone.admits_requests());
        assert!(AppHealth::Healthy.admits_requests());
    }

    #[test]
    fn flood_then_drain_resyncs_the_dirty_set() {
        let t = Instant::now();
        let mut dirty = DirtySet::default();
        dirty.mark(node(1));
        let (s, e) = AppHealth::Healthy.step(
            HealthMsg::IngressOverWatermark {
                dirty: dirty.clone(),
            },
            &ctx(t),
        );
        assert!(e.is_empty());
        assert!(matches!(s, AppHealth::Flooded { .. }));
        let (s, e) = s.step(HealthMsg::BacklogDrained, &ctx(t));
        assert_eq!(s, AppHealth::Healthy);
        assert_eq!(
            e.as_slice(),
            &[HealthEffect::Resync(dirty), HealthEffect::ReRegisterEvents]
        );
    }

    #[test]
    fn global_shed_floods_only_unfocused_top_talkers() {
        let t = Instant::now();
        let (s, e) = AppHealth::Healthy.step(
            HealthMsg::GlobalShed {
                focused: false,
                top3_by_rate: true,
            },
            &ctx(t),
        );
        assert!(matches!(s, AppHealth::Flooded { .. }));
        assert_eq!(e.as_slice(), &[HealthEffect::DeregisterEvents]);
        for (f, top) in [(true, true), (false, false), (true, false)] {
            let (s, e) = AppHealth::Healthy.step(
                HealthMsg::GlobalShed {
                    focused: f,
                    top3_by_rate: top,
                },
                &ctx(t),
            );
            assert_eq!(s, AppHealth::Healthy);
            assert!(e.is_empty());
        }
    }

    #[test]
    fn owner_vanished_is_terminal_from_every_state() {
        let t = Instant::now();
        let states = [
            AppHealth::Healthy,
            AppHealth::Slow {
                timeouts: 1,
                window_start: t,
                consecutive: 1,
                last_timeout: t,
            },
            AppHealth::Hung {
                since: t,
                probe_at: t,
                backoff: HUNG_PROBE_MIN,
                probe_in_flight: true,
            },
            AppHealth::Flooded {
                since: t,
                dirty: DirtySet::default(),
            },
        ];
        for s in states {
            let (s, e) = s.step(HealthMsg::OwnerVanished, &ctx(t));
            assert_eq!(s, AppHealth::Gone);
            assert_eq!(
                e.as_slice(),
                &[HealthEffect::TombstoneAll, HealthEffect::NotifyGone]
            );
        }
        let (s, e) = AppHealth::Gone.step(HealthMsg::Success, &ctx(t));
        assert_eq!(s, AppHealth::Gone);
        assert!(e.is_empty());
    }
}
