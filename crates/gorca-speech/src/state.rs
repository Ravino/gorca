use std::time::{Duration, Instant};

use gorca_model::{Epoch, Prio, UttId, Utterance};
use smallvec::{SmallVec, smallvec};

/// Replace must show `Started` or `Ended(from)` within this time (section 7.4: 30 ms).
pub const REPLACE_DEADLINE: Duration = Duration::from_millis(30);
/// Reconnect backoff bounds (section 8.3: 50 ms to 2 s).
const BACKOFF_MIN: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(2);

/// What the state machine needs to know about an utterance.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct UttMeta {
    pub id: UttId,
    pub prio: Prio,
    pub epoch: Epoch,
    pub survives_focus_change: bool,
}

impl UttMeta {
    /// E-2.
    pub fn is_valid(&self, current: Epoch) -> bool {
        self.epoch >= current || self.survives_focus_change
    }

    fn interrupts(&self, cur_epoch: Epoch) -> bool {
        self.prio == Prio::Interrupt || self.epoch > cur_epoch
    }
}

impl From<&Utterance> for UttMeta {
    fn from(u: &Utterance) -> Self {
        Self {
            id: u.id,
            prio: u.prio,
            epoch: u.epoch,
            survives_focus_change: u.survives_focus_change,
        }
    }
}

/// ADR-0001 section 7.4. Deviations: `Speaking` keeps the utterance policy in `UttMeta`
/// (prio, epoch and the E-2 survival flag); `Paused` keeps the full `UttMeta` of the paused
/// utterance so that `Resume` can return to `Speaking`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpeechState {
    Idle,
    Speaking {
        cur: UttMeta,
        sent_at: Instant,
        started: bool,
    },
    /// Atomic replace or stop+speak in flight.
    Replacing {
        from: UttId,
        to: UttMeta,
        deadline: Instant,
    },
    Paused {
        resume: Option<UttMeta>,
    },
    Disconnected {
        retry_at: Instant,
        attempt: u32,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpeechMsg {
    /// A new utterance from the Reader or a `PresentJob`.
    Intent(UttMeta),
    /// The `EpochCell` moved; `StepCtx::epoch_now` holds the new value.
    EpochAdvanced,
    Started(UttId),
    Ended(UttId),
    Stopped(UttId),
    /// `SpeechEvent::Error` for this utterance.
    Failed(UttId),
    Pause,
    Resume,
    /// Timer: replace deadline, reconnect timer.
    Tick,
    TransportLost,
    Reconnected,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct StepCtx {
    pub now: Instant,
    pub epoch_now: Epoch,
    pub atomic_replace: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpeechEffect {
    /// Send `SpeechCmd::Speak`.
    Speak(UttId),
    /// Send `SpeechCmd::Replace` (atomic backends).
    Replace {
        stop: UttId,
        then: UttId,
    },
    Stop(UttId),
    StopAll,
    Pause,
    Resume,
    /// Park the utterance in the gorca FIFO (S-1).
    Enqueue(UttMeta),
    /// The utterance will never be spoken; release its resources.
    Discard(UttId),
    /// Drop queued utterances that fail E-2.
    PurgeStale,
    /// Start the next valid queued utterance, if any.
    PopNext,
    /// `replace_slow` counter and health penalty.
    CountReplaceSlow,
    Reconnect,
    ResendVoiceParams,
    /// On reconnect, intents older than this are discarded (section 7.4: 2 s).
    DiscardOlderThan(Duration),
}

pub type Effects = SmallVec<[SpeechEffect; 3]>;

/// 50 ms doubling to 2 s. `attempt` starts at 1.
pub fn backoff(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(16);
    BACKOFF_MIN.saturating_mul(1u32 << shift).min(BACKOFF_MAX)
}

fn disconnected(ctx: &StepCtx, attempt: u32) -> SpeechState {
    SpeechState::Disconnected {
        retry_at: ctx.now + backoff(attempt),
        attempt,
    }
}

fn speak(u: UttMeta, ctx: &StepCtx) -> (SpeechState, Effects) {
    (
        SpeechState::Speaking {
            cur: u,
            sent_at: ctx.now,
            started: false,
        },
        smallvec![SpeechEffect::Speak(u.id)],
    )
}

/// Replace `current` by `u`: atomic if the backend can, else Stop + Speak pipelined without
/// waiting for the ack.
fn replace(current: UttId, u: UttMeta, ctx: &StepCtx) -> (SpeechState, Effects) {
    let effects: Effects = if ctx.atomic_replace {
        smallvec![SpeechEffect::Replace {
            stop: current,
            then: u.id
        }]
    } else {
        smallvec![SpeechEffect::Stop(current), SpeechEffect::Speak(u.id)]
    };
    (
        SpeechState::Replacing {
            from: current,
            to: u,
            deadline: ctx.now + REPLACE_DEADLINE,
        },
        effects,
    )
}

impl SpeechState {
    /// Pure transition function. Every (state, message) pair is spelled out.
    pub fn step(self, msg: SpeechMsg, ctx: &StepCtx) -> (SpeechState, Effects) {
        match self {
            SpeechState::Idle => idle(msg, ctx),
            SpeechState::Speaking {
                cur,
                sent_at,
                started,
            } => speaking(cur, sent_at, started, msg, ctx),
            SpeechState::Replacing { from, to, deadline } => {
                replacing(from, to, deadline, msg, ctx)
            }
            SpeechState::Paused { resume } => paused(resume, msg, ctx),
            SpeechState::Disconnected { retry_at, attempt } => {
                disconnected_step(retry_at, attempt, msg, ctx)
            }
        }
    }
}

fn idle(msg: SpeechMsg, ctx: &StepCtx) -> (SpeechState, Effects) {
    match msg {
        SpeechMsg::Intent(u) => {
            if u.is_valid(ctx.epoch_now) {
                speak(u, ctx)
            } else {
                (SpeechState::Idle, smallvec![SpeechEffect::Discard(u.id)])
            }
        }
        SpeechMsg::EpochAdvanced => (SpeechState::Idle, smallvec![SpeechEffect::PurgeStale]),
        // Late events for utterances we no longer track.
        SpeechMsg::Started(_)
        | SpeechMsg::Ended(_)
        | SpeechMsg::Stopped(_)
        | SpeechMsg::Failed(_)
        | SpeechMsg::Resume
        | SpeechMsg::Tick
        | SpeechMsg::Reconnected => (SpeechState::Idle, smallvec![]),
        SpeechMsg::Pause => (
            SpeechState::Paused { resume: None },
            smallvec![SpeechEffect::Pause],
        ),
        SpeechMsg::TransportLost => (disconnected(ctx, 1), smallvec![]),
    }
}

fn speaking(
    cur: UttMeta,
    sent_at: Instant,
    started: bool,
    msg: SpeechMsg,
    ctx: &StepCtx,
) -> (SpeechState, Effects) {
    let same = SpeechState::Speaking {
        cur,
        sent_at,
        started,
    };
    match msg {
        SpeechMsg::Intent(u) => {
            if !u.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::Discard(u.id)])
            } else if u.interrupts(cur.epoch) {
                replace(cur.id, u, ctx)
            } else {
                (same, smallvec![SpeechEffect::Enqueue(u)])
            }
        }
        SpeechMsg::EpochAdvanced => {
            if cur.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::PurgeStale])
            } else {
                (
                    SpeechState::Idle,
                    smallvec![
                        SpeechEffect::Stop(cur.id),
                        SpeechEffect::PurgeStale,
                        SpeechEffect::PopNext
                    ],
                )
            }
        }
        SpeechMsg::Started(id) => {
            let started = started || id == cur.id;
            (
                SpeechState::Speaking {
                    cur,
                    sent_at,
                    started,
                },
                smallvec![],
            )
        }
        SpeechMsg::Ended(id) | SpeechMsg::Stopped(id) | SpeechMsg::Failed(id) => {
            if id == cur.id {
                (SpeechState::Idle, smallvec![SpeechEffect::PopNext])
            } else {
                (same, smallvec![])
            }
        }
        SpeechMsg::Pause => (
            SpeechState::Paused { resume: Some(cur) },
            smallvec![SpeechEffect::Pause],
        ),
        SpeechMsg::Resume | SpeechMsg::Tick | SpeechMsg::Reconnected => (same, smallvec![]),
        SpeechMsg::TransportLost => (disconnected(ctx, 1), smallvec![]),
    }
}

fn replacing(
    from: UttId,
    to: UttMeta,
    deadline: Instant,
    msg: SpeechMsg,
    ctx: &StepCtx,
) -> (SpeechState, Effects) {
    let same = SpeechState::Replacing { from, to, deadline };
    let now_speaking = |started| SpeechState::Speaking {
        cur: to,
        sent_at: ctx.now,
        started,
    };
    match msg {
        SpeechMsg::Intent(u) => {
            if !u.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::Discard(u.id)])
            } else if u.interrupts(to.epoch) {
                replace(to.id, u, ctx)
            } else {
                (same, smallvec![SpeechEffect::Enqueue(u)])
            }
        }
        SpeechMsg::EpochAdvanced => {
            if to.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::PurgeStale])
            } else {
                (
                    SpeechState::Idle,
                    smallvec![
                        SpeechEffect::Stop(to.id),
                        SpeechEffect::PurgeStale,
                        SpeechEffect::PopNext
                    ],
                )
            }
        }
        SpeechMsg::Started(id) => {
            if id == to.id {
                (now_speaking(true), smallvec![])
            } else {
                (same, smallvec![])
            }
        }
        SpeechMsg::Ended(id) | SpeechMsg::Stopped(id) | SpeechMsg::Failed(id) => {
            if id == from {
                (now_speaking(false), smallvec![])
            } else if id == to.id {
                (SpeechState::Idle, smallvec![SpeechEffect::PopNext])
            } else {
                (same, smallvec![])
            }
        }
        SpeechMsg::Tick => {
            if ctx.now >= deadline {
                (
                    now_speaking(false),
                    smallvec![SpeechEffect::CountReplaceSlow],
                )
            } else {
                (same, smallvec![])
            }
        }
        SpeechMsg::Pause => (
            SpeechState::Paused { resume: Some(to) },
            smallvec![SpeechEffect::Pause],
        ),
        SpeechMsg::Resume | SpeechMsg::Reconnected => (same, smallvec![]),
        SpeechMsg::TransportLost => (disconnected(ctx, 1), smallvec![]),
    }
}

fn paused(resume: Option<UttMeta>, msg: SpeechMsg, ctx: &StepCtx) -> (SpeechState, Effects) {
    let same = SpeechState::Paused { resume };
    match msg {
        SpeechMsg::Intent(u) => {
            if !u.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::Discard(u.id)])
            } else if u.prio == Prio::Interrupt {
                let (s, mut e) = speak(u, ctx);
                e.insert(0, SpeechEffect::Resume);
                e.insert(0, SpeechEffect::StopAll);
                (s, e)
            } else {
                (same, smallvec![SpeechEffect::Enqueue(u)])
            }
        }
        SpeechMsg::EpochAdvanced => match resume {
            Some(cur) if !cur.is_valid(ctx.epoch_now) => (
                SpeechState::Paused { resume: None },
                smallvec![SpeechEffect::Stop(cur.id), SpeechEffect::PurgeStale],
            ),
            Some(_) | None => (same, smallvec![SpeechEffect::PurgeStale]),
        },
        SpeechMsg::Ended(id) | SpeechMsg::Stopped(id) | SpeechMsg::Failed(id) => match resume {
            Some(cur) if cur.id == id => (SpeechState::Paused { resume: None }, smallvec![]),
            Some(_) | None => (same, smallvec![]),
        },
        SpeechMsg::Started(_) | SpeechMsg::Pause | SpeechMsg::Tick | SpeechMsg::Reconnected => {
            (same, smallvec![])
        }
        SpeechMsg::Resume => match resume {
            Some(cur) => (
                SpeechState::Speaking {
                    cur,
                    sent_at: ctx.now,
                    started: true,
                },
                smallvec![SpeechEffect::Resume],
            ),
            None => (
                SpeechState::Idle,
                smallvec![SpeechEffect::Resume, SpeechEffect::PopNext],
            ),
        },
        SpeechMsg::TransportLost => (disconnected(ctx, 1), smallvec![]),
    }
}

fn disconnected_step(
    retry_at: Instant,
    attempt: u32,
    msg: SpeechMsg,
    ctx: &StepCtx,
) -> (SpeechState, Effects) {
    let same = SpeechState::Disconnected { retry_at, attempt };
    match msg {
        SpeechMsg::Intent(u) => {
            if u.is_valid(ctx.epoch_now) {
                (same, smallvec![SpeechEffect::Enqueue(u)])
            } else {
                (same, smallvec![SpeechEffect::Discard(u.id)])
            }
        }
        SpeechMsg::EpochAdvanced => (same, smallvec![SpeechEffect::PurgeStale]),
        SpeechMsg::Started(_)
        | SpeechMsg::Ended(_)
        | SpeechMsg::Stopped(_)
        | SpeechMsg::Failed(_)
        | SpeechMsg::Pause
        | SpeechMsg::Resume => (same, smallvec![]),
        SpeechMsg::Tick => {
            if ctx.now >= retry_at {
                // Next Tick waits for the next backoff step, so Reconnect is not repeated.
                let next = attempt.saturating_add(1);
                (
                    SpeechState::Disconnected {
                        retry_at: ctx.now + backoff(next),
                        attempt: next,
                    },
                    smallvec![SpeechEffect::Reconnect],
                )
            } else {
                (same, smallvec![])
            }
        }
        SpeechMsg::TransportLost => (disconnected(ctx, attempt.saturating_add(1)), smallvec![]),
        SpeechMsg::Reconnected => (
            SpeechState::Idle,
            smallvec![
                SpeechEffect::ResendVoiceParams,
                SpeechEffect::DiscardOlderThan(Duration::from_secs(2)),
                SpeechEffect::PopNext
            ],
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(id: u64, prio: Prio, epoch: u64) -> UttMeta {
        UttMeta {
            id: UttId(id),
            prio,
            epoch: Epoch(epoch),
            survives_focus_change: false,
        }
    }

    fn ctx(now: Instant, epoch: u64, atomic: bool) -> StepCtx {
        StepCtx {
            now,
            epoch_now: Epoch(epoch),
            atomic_replace: atomic,
        }
    }

    fn speaking_state(u: UttMeta, now: Instant) -> SpeechState {
        SpeechState::Speaking {
            cur: u,
            sent_at: now,
            started: false,
        }
    }

    #[test]
    fn idle_valid_intent_speaks_and_stale_intent_is_discarded() {
        let t = Instant::now();
        let (s, e) =
            SpeechState::Idle.step(SpeechMsg::Intent(meta(1, Prio::Text, 5)), &ctx(t, 5, false));
        assert_eq!(s, speaking_state(meta(1, Prio::Text, 5), t));
        assert_eq!(e.as_slice(), &[SpeechEffect::Speak(UttId(1))]);

        let (s, e) =
            SpeechState::Idle.step(SpeechMsg::Intent(meta(2, Prio::Text, 4)), &ctx(t, 5, false));
        assert_eq!(s, SpeechState::Idle);
        assert_eq!(e.as_slice(), &[SpeechEffect::Discard(UttId(2))]);
    }

    #[test]
    fn interrupt_replaces_atomically_when_supported() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (s, e) = s.step(
            SpeechMsg::Intent(meta(2, Prio::Interrupt, 5)),
            &ctx(t, 5, true),
        );
        assert_eq!(
            s,
            SpeechState::Replacing {
                from: UttId(1),
                to: meta(2, Prio::Interrupt, 5),
                deadline: t + REPLACE_DEADLINE
            }
        );
        assert_eq!(
            e.as_slice(),
            &[SpeechEffect::Replace {
                stop: UttId(1),
                then: UttId(2)
            }]
        );
    }

    #[test]
    fn interrupt_lowers_to_stop_plus_speak_without_atomic_replace() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (_, e) = s.step(
            SpeechMsg::Intent(meta(2, Prio::Interrupt, 5)),
            &ctx(t, 5, false),
        );
        assert_eq!(
            e.as_slice(),
            &[SpeechEffect::Stop(UttId(1)), SpeechEffect::Speak(UttId(2))]
        );
    }

    #[test]
    fn newer_epoch_interrupts_even_at_low_priority() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (s, _) = s.step(
            SpeechMsg::Intent(meta(2, Prio::Notification, 6)),
            &ctx(t, 6, true),
        );
        assert!(matches!(s, SpeechState::Replacing { .. }));
    }

    #[test]
    fn same_epoch_lower_priority_is_queued() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let u = meta(2, Prio::Notification, 5);
        let (s2, e) = s.step(SpeechMsg::Intent(u), &ctx(t, 5, true));
        assert_eq!(s2, s);
        assert_eq!(e.as_slice(), &[SpeechEffect::Enqueue(u)]);
    }

    #[test]
    fn epoch_advance_stops_stale_speech_without_a_message() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (s, e) = s.step(SpeechMsg::EpochAdvanced, &ctx(t, 6, true));
        assert_eq!(s, SpeechState::Idle);
        assert_eq!(e[0], SpeechEffect::Stop(UttId(1)));
        assert!(e.contains(&SpeechEffect::PopNext));
    }

    #[test]
    fn epoch_advance_spares_survivors() {
        let t = Instant::now();
        let mut m = meta(1, Prio::Progress, 5);
        m.survives_focus_change = true;
        let s = speaking_state(m, t);
        let (s2, _) = s.step(SpeechMsg::EpochAdvanced, &ctx(t, 9, true));
        assert_eq!(s2, s);
    }

    #[test]
    fn ended_returns_to_idle_and_pops() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (s, e) = s.step(SpeechMsg::Ended(UttId(1)), &ctx(t, 5, true));
        assert_eq!(s, SpeechState::Idle);
        assert_eq!(e.as_slice(), &[SpeechEffect::PopNext]);
    }

    #[test]
    fn events_for_other_utterances_are_ignored() {
        let t = Instant::now();
        let s = speaking_state(meta(2, Prio::Text, 5), t);
        let (s2, e) = s.step(SpeechMsg::Ended(UttId(1)), &ctx(t, 5, true));
        assert_eq!(s2, s);
        assert!(e.is_empty());
    }

    #[test]
    fn started_marks_the_current_utterance() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (s, _) = s.step(SpeechMsg::Started(UttId(1)), &ctx(t, 5, true));
        assert!(matches!(s, SpeechState::Speaking { started: true, .. }));
    }

    #[test]
    fn replacing_resolves_on_started_ended_or_deadline() {
        let t = Instant::now();
        let to = meta(2, Prio::Interrupt, 5);
        let r = SpeechState::Replacing {
            from: UttId(1),
            to,
            deadline: t + REPLACE_DEADLINE,
        };

        let (s, _) = r.step(SpeechMsg::Started(UttId(2)), &ctx(t, 5, true));
        assert!(matches!(s, SpeechState::Speaking { started: true, .. }));

        let (s, _) = r.step(SpeechMsg::Ended(UttId(1)), &ctx(t, 5, true));
        assert!(matches!(s, SpeechState::Speaking { started: false, .. }));

        let (s, e) = r.step(
            SpeechMsg::Tick,
            &ctx(t + Duration::from_millis(10), 5, true),
        );
        assert_eq!(s, r);
        assert!(e.is_empty());
        let (s, e) = r.step(
            SpeechMsg::Tick,
            &ctx(t + Duration::from_millis(31), 5, true),
        );
        assert!(matches!(s, SpeechState::Speaking { .. }));
        assert_eq!(e.as_slice(), &[SpeechEffect::CountReplaceSlow]);
    }

    #[test]
    fn pause_and_resume_round_trip() {
        let t = Instant::now();
        let u = meta(1, Prio::Text, 5);
        let (p, e) = speaking_state(u, t).step(SpeechMsg::Pause, &ctx(t, 5, true));
        assert_eq!(p, SpeechState::Paused { resume: Some(u) });
        assert_eq!(e.as_slice(), &[SpeechEffect::Pause]);
        let (s, e) = p.step(SpeechMsg::Resume, &ctx(t, 5, true));
        assert!(matches!(s, SpeechState::Speaking { cur, .. } if cur == u));
        assert_eq!(e.as_slice(), &[SpeechEffect::Resume]);
    }

    #[test]
    fn interrupt_cuts_through_a_pause() {
        let t = Instant::now();
        let p = SpeechState::Paused {
            resume: Some(meta(1, Prio::Text, 5)),
        };
        let (s, e) = p.step(
            SpeechMsg::Intent(meta(2, Prio::Interrupt, 5)),
            &ctx(t, 5, true),
        );
        assert!(matches!(s, SpeechState::Speaking { .. }));
        assert_eq!(
            e.as_slice(),
            &[
                SpeechEffect::StopAll,
                SpeechEffect::Resume,
                SpeechEffect::Speak(UttId(2))
            ]
        );
    }

    #[test]
    fn transport_loss_backs_off_and_recovers() {
        let t = Instant::now();
        let s = speaking_state(meta(1, Prio::Text, 5), t);
        let (d, _) = s.step(SpeechMsg::TransportLost, &ctx(t, 5, true));
        assert_eq!(
            d,
            SpeechState::Disconnected {
                retry_at: t + Duration::from_millis(50),
                attempt: 1
            }
        );

        // Intents are parked while disconnected.
        let u = meta(2, Prio::Text, 5);
        let (d2, e) = d.step(SpeechMsg::Intent(u), &ctx(t, 5, true));
        assert_eq!(d2, d);
        assert_eq!(e.as_slice(), &[SpeechEffect::Enqueue(u)]);

        // Early tick does nothing; a due tick asks for one reconnect and re-arms the timer.
        let (d3, e) = d.step(
            SpeechMsg::Tick,
            &ctx(t + Duration::from_millis(10), 5, true),
        );
        assert_eq!(d3, d);
        assert!(e.is_empty());
        let due = t + Duration::from_millis(60);
        let (d4, e) = d.step(SpeechMsg::Tick, &ctx(due, 5, true));
        assert_eq!(e.as_slice(), &[SpeechEffect::Reconnect]);
        assert_eq!(
            d4,
            SpeechState::Disconnected {
                retry_at: due + Duration::from_millis(100),
                attempt: 2
            }
        );

        let (s, e) = d4.step(SpeechMsg::Reconnected, &ctx(due, 5, true));
        assert_eq!(s, SpeechState::Idle);
        assert!(e.contains(&SpeechEffect::ResendVoiceParams));
        assert!(e.contains(&SpeechEffect::DiscardOlderThan(Duration::from_secs(2))));
    }

    #[test]
    fn backoff_doubles_from_50ms_to_2s() {
        assert_eq!(backoff(1), Duration::from_millis(50));
        assert_eq!(backoff(2), Duration::from_millis(100));
        assert_eq!(backoff(6), Duration::from_millis(1600));
        assert_eq!(backoff(7), Duration::from_secs(2));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(2));
    }
}
