use std::time::{Duration, Instant};

use gorca_input_api::GrabSet;

/// `WatchKeyboard` deadline (section 7.3 table: 500 ms).
pub const AUTHORIZE_DEADLINE: Duration = Duration::from_millis(500);
/// `SetKeyGrabs` deadline (200 ms).
pub const GRAB_DEADLINE: Duration = Duration::from_millis(200);
/// Mutter applies modifier grabs asynchronously on its input thread (5 ms).
pub const SETTLE: Duration = Duration::from_millis(5);
/// "Pending (retry x2) then Lost".
pub const GRAB_RETRIES: u8 = 2;

const LOST_MIN: Duration = Duration::from_millis(250);
const LOST_MAX: Duration = Duration::from_secs(5);

/// Reply to `RequestName("org.gnome.Orca.KeyboardMonitor", flags = 0)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum RequestNameReply {
    PrimaryOwner,
    /// Queued behind the current owner (Orca).
    InQueue,
    /// Only returned with `DO_NOT_QUEUE`; kept for robustness.
    Exists,
    AlreadyOwner,
}

/// ADR-0001 section 7.3. Deviations: `Pending` counts `retries`, and `Lost` counts `attempt`
/// (for backoff); the ADR table needs both ("retry x2", "Lost{backoff}").
// `Watching` carries two grab sets while `Pending`; the machine steps at human typing rate, so
// the size difference is irrelevant and boxing would only add noise.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonitorState {
    /// Name owned elsewhere (Orca).
    Unauthorized {
        queued: bool,
    },
    /// `WatchKeyboard` in flight.
    Authorizing {
        deadline: Instant,
    },
    Watching {
        grabs: GrabSetState,
    },
    /// Manager vanished (shell restart) or name lost.
    Lost {
        retry_at: Instant,
        attempt: u32,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GrabSetState {
    Committed {
        generation: u32,
        set: GrabSet,
    },
    Pending {
        committed: (u32, GrabSet),
        wanted: (u32, GrabSet),
        sent_at: Instant,
        retries: u8,
    },
}

impl GrabSetState {
    /// The set Mutter most likely applied (used for classification).
    pub fn committed_set(&self) -> &GrabSet {
        match self {
            GrabSetState::Committed { set, .. } => set,
            GrabSetState::Pending { committed, .. } => &committed.1,
        }
    }

    /// The in-flight set, during `Pending`.
    pub fn wanted_set(&self) -> Option<&GrabSet> {
        match self {
            GrabSetState::Committed { .. } => None,
            GrabSetState::Pending { wanted, .. } => Some(&wanted.1),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonitorMsg {
    NameReply(RequestNameReply),
    NameAcquired,
    NameLost,
    /// The Manager's owner changed to empty.
    ManagerGone,
    WatchOk,
    /// `AccessDenied` or any error reply.
    WatchDenied,
    WatchTimeout,
    SetGrabSet(GrabSet),
    GrabsOk {
        generation: u32,
    },
    GrabsTimeout {
        generation: u32,
    },
    SettleElapsed {
        generation: u32,
    },
    RetryTimer,
}

/// What the Reader says to the user, once (section 5.5, 7.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    /// "Another screen reader holds the keyboard; gorca will take over when it exits."
    AnotherScreenReaderHoldsKeyboard,
    /// "keyboard commands unavailable".
    KeyboardCommandsUnavailable,
    /// `WatchKeyboard` was refused or timed out.
    KeyboardMonitorUnavailable,
    /// The monitor is authorised again and the grabs were re-sent after a loss.
    KeyboardRestored,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MonitorEffect {
    /// Async `RequestName` with a deadline.
    RequestName,
    WatchKeyboard {
        deadline: Instant,
    },
    /// Async `SetKeyGrabs` with `GRAB_DEADLINE`.
    SendGrabs {
        generation: u32,
        set: GrabSet,
    },
    ArmSettle {
        generation: u32,
        after: Duration,
    },
    /// Ask the Reader for the current mode's grab set (it answers with `SetGrabSet`).
    ApplyModeGrabs,
    Notify(Notice),
    ArmRetry {
        at: Instant,
    },
}

#[derive(Copy, Clone, Debug)]
pub struct StepCtx {
    pub now: Instant,
}

pub fn lost_backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(16);
    LOST_MIN.saturating_mul(1u32 << shift).min(LOST_MAX)
}

fn lost(ctx: &StepCtx, attempt: u32) -> (MonitorState, Vec<MonitorEffect>) {
    let at = ctx.now + lost_backoff(attempt);
    (
        MonitorState::Lost {
            retry_at: at,
            attempt,
        },
        vec![MonitorEffect::ArmRetry { at }],
    )
}

fn authorizing(ctx: &StepCtx) -> (MonitorState, Vec<MonitorEffect>) {
    let deadline = ctx.now + AUTHORIZE_DEADLINE;
    (
        MonitorState::Authorizing { deadline },
        vec![MonitorEffect::WatchKeyboard { deadline }],
    )
}

impl MonitorState {
    /// Initial state and the first effect: request the name.
    pub fn start() -> (MonitorState, Vec<MonitorEffect>) {
        (
            MonitorState::Unauthorized { queued: false },
            vec![MonitorEffect::RequestName],
        )
    }

    /// Pure transition function. Every (state, message) pair is spelled out.
    pub fn step(self, msg: MonitorMsg, ctx: &StepCtx) -> (MonitorState, Vec<MonitorEffect>) {
        match self {
            MonitorState::Unauthorized { queued } => unauthorized(queued, &msg, ctx),
            MonitorState::Authorizing { deadline } => authorizing_step(deadline, &msg, ctx),
            MonitorState::Watching { grabs } => watching(grabs, msg, ctx),
            MonitorState::Lost { retry_at, attempt } => lost_step(retry_at, attempt, &msg, ctx),
        }
    }
}

fn unauthorized(
    queued: bool,
    msg: &MonitorMsg,
    ctx: &StepCtx,
) -> (MonitorState, Vec<MonitorEffect>) {
    let same = MonitorState::Unauthorized { queued };
    match msg {
        MonitorMsg::NameReply(RequestNameReply::PrimaryOwner | RequestNameReply::AlreadyOwner)
        | MonitorMsg::NameAcquired => authorizing(ctx),
        MonitorMsg::NameReply(RequestNameReply::InQueue) => (
            MonitorState::Unauthorized { queued: true },
            if queued {
                vec![]
            } else {
                vec![MonitorEffect::Notify(
                    Notice::AnotherScreenReaderHoldsKeyboard,
                )]
            },
        ),
        MonitorMsg::NameReply(RequestNameReply::Exists) => {
            (MonitorState::Unauthorized { queued: false }, vec![])
        }
        MonitorMsg::NameLost
        | MonitorMsg::ManagerGone
        | MonitorMsg::WatchOk
        | MonitorMsg::WatchDenied
        | MonitorMsg::WatchTimeout
        | MonitorMsg::SetGrabSet(_)
        | MonitorMsg::GrabsOk { .. }
        | MonitorMsg::GrabsTimeout { .. }
        | MonitorMsg::SettleElapsed { .. }
        | MonitorMsg::RetryTimer => (same, vec![]),
    }
}

fn authorizing_step(
    deadline: Instant,
    msg: &MonitorMsg,
    ctx: &StepCtx,
) -> (MonitorState, Vec<MonitorEffect>) {
    let same = MonitorState::Authorizing { deadline };
    match msg {
        MonitorMsg::WatchOk => (
            MonitorState::Watching {
                grabs: GrabSetState::Committed {
                    generation: 0,
                    set: GrabSet::default(),
                },
            },
            vec![MonitorEffect::ApplyModeGrabs],
        ),
        MonitorMsg::WatchDenied | MonitorMsg::WatchTimeout => {
            let (s, mut e) = lost(ctx, 1);
            e.push(MonitorEffect::Notify(Notice::KeyboardMonitorUnavailable));
            (s, e)
        }
        MonitorMsg::NameLost | MonitorMsg::ManagerGone => {
            let (s, mut e) = lost(ctx, 1);
            e.push(MonitorEffect::Notify(Notice::KeyboardCommandsUnavailable));
            (s, e)
        }
        // Grab sets requested before authorisation completes are re-requested by
        // `ApplyModeGrabs` once `WatchOk` arrives, so they can be dropped here.
        MonitorMsg::NameReply(_)
        | MonitorMsg::NameAcquired
        | MonitorMsg::SetGrabSet(_)
        | MonitorMsg::GrabsOk { .. }
        | MonitorMsg::GrabsTimeout { .. }
        | MonitorMsg::SettleElapsed { .. }
        | MonitorMsg::RetryTimer => (same, vec![]),
    }
}

fn watching(
    grabs: GrabSetState,
    msg: MonitorMsg,
    ctx: &StepCtx,
) -> (MonitorState, Vec<MonitorEffect>) {
    match msg {
        MonitorMsg::SetGrabSet(s) => set_grab_set(grabs, s, ctx),
        MonitorMsg::GrabsOk { generation } => match grabs {
            GrabSetState::Pending { ref wanted, .. } if wanted.0 == generation => {
                let e = vec![MonitorEffect::ArmSettle {
                    generation,
                    after: SETTLE,
                }];
                (MonitorState::Watching { grabs }, e)
            }
            GrabSetState::Pending { .. } | GrabSetState::Committed { .. } => {
                (MonitorState::Watching { grabs }, vec![])
            }
        },
        MonitorMsg::SettleElapsed { generation } => match grabs {
            GrabSetState::Pending { wanted, .. } if wanted.0 == generation => (
                MonitorState::Watching {
                    grabs: GrabSetState::Committed {
                        generation: wanted.0,
                        set: wanted.1,
                    },
                },
                vec![],
            ),
            GrabSetState::Pending { .. } | GrabSetState::Committed { .. } => {
                (MonitorState::Watching { grabs }, vec![])
            }
        },
        MonitorMsg::GrabsTimeout { generation } => match grabs {
            GrabSetState::Pending {
                committed,
                wanted,
                retries,
                ..
            } if wanted.0 == generation => {
                if retries < GRAB_RETRIES {
                    let e = vec![MonitorEffect::SendGrabs {
                        generation,
                        set: wanted.1.clone(),
                    }];
                    (
                        MonitorState::Watching {
                            grabs: GrabSetState::Pending {
                                committed,
                                wanted,
                                sent_at: ctx.now,
                                retries: retries + 1,
                            },
                        },
                        e,
                    )
                } else {
                    lost(ctx, 1)
                }
            }
            GrabSetState::Pending { .. } | GrabSetState::Committed { .. } => {
                (MonitorState::Watching { grabs }, vec![])
            }
        },
        MonitorMsg::NameLost | MonitorMsg::ManagerGone => {
            let (s, mut e) = lost(ctx, 1);
            e.push(MonitorEffect::Notify(Notice::KeyboardCommandsUnavailable));
            (s, e)
        }
        MonitorMsg::NameReply(_)
        | MonitorMsg::NameAcquired
        | MonitorMsg::WatchOk
        | MonitorMsg::WatchDenied
        | MonitorMsg::WatchTimeout
        | MonitorMsg::RetryTimer => (MonitorState::Watching { grabs }, vec![]),
    }
}

fn set_grab_set(
    grabs: GrabSetState,
    s: GrabSet,
    ctx: &StepCtx,
) -> (MonitorState, Vec<MonitorEffect>) {
    match grabs {
        GrabSetState::Committed { generation, set } => {
            if set == s {
                return (
                    MonitorState::Watching {
                        grabs: GrabSetState::Committed { generation, set },
                    },
                    vec![],
                );
            }
            let wanted_gen = generation.wrapping_add(1);
            let e = vec![MonitorEffect::SendGrabs {
                generation: wanted_gen,
                set: s.clone(),
            }];
            (
                MonitorState::Watching {
                    grabs: GrabSetState::Pending {
                        committed: (generation, set),
                        wanted: (wanted_gen, s),
                        sent_at: ctx.now,
                        retries: 0,
                    },
                },
                e,
            )
        }
        // The latest wins: send again with a new generation.
        GrabSetState::Pending {
            committed, wanted, ..
        } => {
            let wanted_gen = wanted.0.wrapping_add(1);
            let e = vec![MonitorEffect::SendGrabs {
                generation: wanted_gen,
                set: s.clone(),
            }];
            (
                MonitorState::Watching {
                    grabs: GrabSetState::Pending {
                        committed,
                        wanted: (wanted_gen, s),
                        sent_at: ctx.now,
                        retries: 0,
                    },
                },
                e,
            )
        }
    }
}

fn lost_step(
    retry_at: Instant,
    attempt: u32,
    msg: &MonitorMsg,
    ctx: &StepCtx,
) -> (MonitorState, Vec<MonitorEffect>) {
    let same = MonitorState::Lost { retry_at, attempt };
    match msg {
        MonitorMsg::RetryTimer => {
            if ctx.now >= retry_at {
                authorizing(ctx)
            } else {
                (same, vec![])
            }
        }
        // The name or the manager came back: retry right away.
        MonitorMsg::NameAcquired | MonitorMsg::NameReply(RequestNameReply::PrimaryOwner) => {
            authorizing(ctx)
        }
        MonitorMsg::NameReply(
            RequestNameReply::InQueue | RequestNameReply::Exists | RequestNameReply::AlreadyOwner,
        )
        | MonitorMsg::NameLost
        | MonitorMsg::ManagerGone
        | MonitorMsg::WatchOk
        | MonitorMsg::WatchDenied
        | MonitorMsg::WatchTimeout
        | MonitorMsg::SetGrabSet(_)
        | MonitorMsg::GrabsOk { .. }
        | MonitorMsg::GrabsTimeout { .. }
        | MonitorMsg::SettleElapsed { .. } => (same, vec![]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::{Chord, Modifiers};

    fn set(keysym: u32) -> GrabSet {
        let mut s = GrabSet::default();
        s.chords.insert(Chord {
            keysym,
            modifiers: Modifiers::CONTROL,
        });
        s
    }

    fn ctx(now: Instant) -> StepCtx {
        StepCtx { now }
    }

    fn watching_committed(t: Instant) -> MonitorState {
        let (s, _) = MonitorState::start();
        let (s, _) = s.step(
            MonitorMsg::NameReply(RequestNameReply::PrimaryOwner),
            &ctx(t),
        );
        let (s, e) = s.step(MonitorMsg::WatchOk, &ctx(t));
        assert_eq!(e, vec![MonitorEffect::ApplyModeGrabs]);
        s
    }

    #[test]
    fn startup_requests_the_name_then_authorizes() {
        let t = Instant::now();
        let (s, e) = MonitorState::start();
        assert_eq!(e, vec![MonitorEffect::RequestName]);
        let (s, e) = s.step(
            MonitorMsg::NameReply(RequestNameReply::PrimaryOwner),
            &ctx(t),
        );
        assert_eq!(
            s,
            MonitorState::Authorizing {
                deadline: t + AUTHORIZE_DEADLINE
            }
        );
        assert_eq!(
            e,
            vec![MonitorEffect::WatchKeyboard {
                deadline: t + AUTHORIZE_DEADLINE
            }]
        );
    }

    #[test]
    fn queued_behind_orca_speaks_once_and_takes_over_on_name_acquired() {
        let t = Instant::now();
        let (s, _) = MonitorState::start();
        let (s, e) = s.step(MonitorMsg::NameReply(RequestNameReply::InQueue), &ctx(t));
        assert_eq!(s, MonitorState::Unauthorized { queued: true });
        assert_eq!(
            e,
            vec![MonitorEffect::Notify(
                Notice::AnotherScreenReaderHoldsKeyboard
            )]
        );
        // A repeated reply does not speak again.
        let (s, e) = s.step(MonitorMsg::NameReply(RequestNameReply::InQueue), &ctx(t));
        assert_eq!(e, vec![]);
        let (s, _) = s.step(MonitorMsg::NameAcquired, &ctx(t));
        assert!(matches!(s, MonitorState::Authorizing { .. }));
    }

    #[test]
    fn denied_or_timed_out_authorisation_goes_lost_and_speaks_once() {
        let t = Instant::now();
        for msg in [MonitorMsg::WatchDenied, MonitorMsg::WatchTimeout] {
            let s = MonitorState::Authorizing { deadline: t };
            let (s, e) = s.step(msg, &ctx(t));
            assert_eq!(
                s,
                MonitorState::Lost {
                    retry_at: t + LOST_MIN,
                    attempt: 1
                }
            );
            assert!(e.contains(&MonitorEffect::Notify(Notice::KeyboardMonitorUnavailable)));
        }
    }

    #[test]
    fn grab_set_change_goes_pending_then_commits_after_ack_and_settle() {
        let t = Instant::now();
        let s = watching_committed(t);
        let (s, e) = s.step(MonitorMsg::SetGrabSet(set(1)), &ctx(t));
        assert_eq!(
            e,
            vec![MonitorEffect::SendGrabs {
                generation: 1,
                set: set(1)
            }]
        );
        let MonitorState::Watching { grabs } = &s else {
            panic!("not watching")
        };
        assert_eq!(grabs.wanted_set(), Some(&set(1)));
        assert_eq!(grabs.committed_set(), &GrabSet::default());

        let (s, e) = s.step(MonitorMsg::GrabsOk { generation: 1 }, &ctx(t));
        assert_eq!(
            e,
            vec![MonitorEffect::ArmSettle {
                generation: 1,
                after: SETTLE
            }]
        );
        // Still pending until the settle timer fires.
        let MonitorState::Watching { grabs } = &s else {
            panic!("not watching")
        };
        assert!(grabs.wanted_set().is_some());

        let (s, _) = s.step(MonitorMsg::SettleElapsed { generation: 1 }, &ctx(t));
        assert_eq!(
            s,
            MonitorState::Watching {
                grabs: GrabSetState::Committed {
                    generation: 1,
                    set: set(1)
                }
            }
        );
    }

    #[test]
    fn same_grab_set_is_a_no_op() {
        let t = Instant::now();
        let s = watching_committed(t);
        let (s2, e) = s
            .clone()
            .step(MonitorMsg::SetGrabSet(GrabSet::default()), &ctx(t));
        assert_eq!(s2, s);
        assert_eq!(e, vec![]);
    }

    #[test]
    fn newer_grab_set_supersedes_the_one_in_flight() {
        let t = Instant::now();
        let s = watching_committed(t);
        let (s, _) = s.step(MonitorMsg::SetGrabSet(set(1)), &ctx(t));
        let (s, e) = s.step(MonitorMsg::SetGrabSet(set(2)), &ctx(t));
        assert_eq!(
            e,
            vec![MonitorEffect::SendGrabs {
                generation: 2,
                set: set(2)
            }]
        );
        // The late ack and settle of generation 1 must not commit anything.
        let (s, e) = s.step(MonitorMsg::GrabsOk { generation: 1 }, &ctx(t));
        assert_eq!(e, vec![]);
        let (s, _) = s.step(MonitorMsg::SettleElapsed { generation: 1 }, &ctx(t));
        let MonitorState::Watching { grabs } = &s else {
            panic!("not watching")
        };
        assert_eq!(grabs.committed_set(), &GrabSet::default());
        assert_eq!(grabs.wanted_set(), Some(&set(2)));
        let (s, _) = s.step(MonitorMsg::SettleElapsed { generation: 2 }, &ctx(t));
        assert_eq!(
            s,
            MonitorState::Watching {
                grabs: GrabSetState::Committed {
                    generation: 2,
                    set: set(2)
                }
            }
        );
    }

    #[test]
    fn reply_timeout_retries_twice_then_loses_the_monitor() {
        let t = Instant::now();
        let s = watching_committed(t);
        let (mut s, _) = s.step(MonitorMsg::SetGrabSet(set(1)), &ctx(t));
        for _ in 0..GRAB_RETRIES {
            let (n, e) = s.step(MonitorMsg::GrabsTimeout { generation: 1 }, &ctx(t));
            assert_eq!(
                e,
                vec![MonitorEffect::SendGrabs {
                    generation: 1,
                    set: set(1)
                }]
            );
            s = n;
        }
        let (s, e) = s.step(MonitorMsg::GrabsTimeout { generation: 1 }, &ctx(t));
        assert!(matches!(s, MonitorState::Lost { attempt: 1, .. }));
        assert!(matches!(e[0], MonitorEffect::ArmRetry { .. }));
    }

    #[test]
    fn name_loss_in_any_active_state_goes_lost_and_announces() {
        let t = Instant::now();
        for s in [
            watching_committed(t),
            MonitorState::Authorizing { deadline: t },
        ] {
            let (s, e) = s.step(MonitorMsg::NameLost, &ctx(t));
            assert!(matches!(s, MonitorState::Lost { .. }));
            assert!(e.contains(&MonitorEffect::Notify(Notice::KeyboardCommandsUnavailable)));
        }
    }

    #[test]
    fn lost_retries_on_the_timer_only_when_due() {
        let t = Instant::now();
        let s = MonitorState::Lost {
            retry_at: t + LOST_MIN,
            attempt: 1,
        };
        let (s, e) = s.step(MonitorMsg::RetryTimer, &ctx(t));
        assert_eq!(e, vec![]);
        let (s, e) = s.step(MonitorMsg::RetryTimer, &ctx(t + LOST_MIN));
        assert!(matches!(s, MonitorState::Authorizing { .. }));
        assert!(matches!(e[0], MonitorEffect::WatchKeyboard { .. }));
    }

    #[test]
    fn lost_backoff_grows_to_five_seconds() {
        assert_eq!(lost_backoff(1), Duration::from_millis(250));
        assert_eq!(lost_backoff(2), Duration::from_millis(500));
        assert_eq!(lost_backoff(10), Duration::from_secs(5));
    }
}
