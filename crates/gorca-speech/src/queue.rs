use std::time::Instant;

use gorca_model::{Epoch, Prio};

use crate::state::{SpeechEffect, SpeechMsg, SpeechState, StepCtx, UttMeta};

/// How long a queued notification waits after an epoch advance before it may start.
pub const NOTICE_GRACE: std::time::Duration = std::time::Duration::from_millis(40);

/// FIFO bound (section 6.2: "1 + 32").
pub const FIFO_CAPACITY: usize = 32;

#[derive(Copy, Clone, Debug)]
struct Queued {
    meta: UttMeta,
    at: Instant,
    seq: u64,
}

/// The state machine plus the bounded gorca-side FIFO (S-1). Pure: no I/O, no clock of its own.
#[derive(Debug)]
pub struct SpeechQueue {
    state: SpeechState,
    fifo: Vec<Queued>,
    seq: u64,
    atomic_replace: bool,
    /// After an epoch advance a queued `Notification` is not started for [`NOTICE_GRACE`]: the
    /// focus utterance that caused the advance is about to arrive and would cut it off after a
    /// blip (M4b N1).
    quiet_until: Option<Instant>,
    replace_slow: u64,
    dropped: u64,
}

impl SpeechQueue {
    pub fn new(atomic_replace: bool) -> Self {
        Self {
            state: SpeechState::Idle,
            fifo: Vec::with_capacity(FIFO_CAPACITY),
            seq: 0,
            atomic_replace,
            quiet_until: None,
            replace_slow: 0,
            dropped: 0,
        }
    }

    pub fn state(&self) -> SpeechState {
        self.state
    }

    /// When a queued notification becomes startable, if it is waiting out the grace.
    pub fn next_wake(&self, now: Instant) -> Option<Instant> {
        let until = self.quiet_until.filter(|u| now < *u)?;
        self.fifo
            .iter()
            .any(|q| q.meta.prio == Prio::Notification)
            .then_some(until)
    }

    pub fn queued(&self) -> usize {
        self.fifo.len()
    }

    pub fn replace_slow_count(&self) -> u64 {
        self.replace_slow
    }

    pub fn dropped_count(&self) -> u64 {
        self.dropped
    }

    /// Feed one message; returns the effects the speech lane must carry out (commands to the
    /// `SpeechClient`, discards, counters). FIFO bookkeeping is handled here.
    pub fn handle(&mut self, msg: SpeechMsg, now: Instant, epoch_now: Epoch) -> Vec<SpeechEffect> {
        let ctx = StepCtx {
            now,
            epoch_now,
            atomic_replace: self.atomic_replace,
        };
        let mut out = Vec::new();
        if matches!(msg, SpeechMsg::EpochAdvanced) {
            self.quiet_until = Some(now + NOTICE_GRACE);
        }
        // A fresh notification reaching an idle lane inside the grace waits like a queued one:
        // the focus utterance that caused the advance is about to cut it off (M-1). The lane
        // wakes at `next_wake` and ticks the queue.
        if let SpeechMsg::Intent(meta) = msg
            && meta.prio == Prio::Notification
            && self.state == SpeechState::Idle
            && meta.is_valid(epoch_now)
            && self.quiet_until.is_some_and(|u| now < u)
        {
            self.enqueue(meta, now, &mut out);
            return out;
        }
        let mut pending = vec![msg];
        // Bounded: each PopNext consumes one FIFO entry.
        while let Some(m) = pending.pop() {
            let (next, effects) = self.state.step(m, &ctx);
            self.state = next;
            for e in effects {
                match e {
                    SpeechEffect::Enqueue(meta) => self.enqueue(meta, now, &mut out),
                    SpeechEffect::PurgeStale => {
                        self.purge(|q| q.meta.is_valid(epoch_now), &mut out);
                    }
                    SpeechEffect::DiscardOlderThan(age) => {
                        self.purge(|q| now.saturating_duration_since(q.at) <= age, &mut out);
                    }
                    SpeechEffect::PopNext => {
                        if let Some(meta) = self.pop(epoch_now, now, &mut out) {
                            pending.push(SpeechMsg::Intent(meta));
                        }
                    }
                    SpeechEffect::CountReplaceSlow => {
                        self.replace_slow += 1;
                        out.push(e);
                    }
                    SpeechEffect::Speak(_)
                    | SpeechEffect::Replace { .. }
                    | SpeechEffect::Stop(_)
                    | SpeechEffect::StopAll
                    | SpeechEffect::Pause
                    | SpeechEffect::Resume
                    | SpeechEffect::Discard(_)
                    | SpeechEffect::Reconnect
                    | SpeechEffect::ResendVoiceParams => out.push(e),
                }
            }
        }
        out
    }

    fn enqueue(&mut self, meta: UttMeta, at: Instant, out: &mut Vec<SpeechEffect>) {
        if self.fifo.len() >= FIFO_CAPACITY {
            // Drop lowest-priority-oldest: the worst entry among queue and newcomer.
            let worst = self
                .fifo
                .iter()
                .enumerate()
                .max_by_key(|(_, q)| (q.meta.prio, std::cmp::Reverse(q.seq)))
                .map(|(i, q)| (i, q.meta));
            match worst {
                Some((i, w)) if w.prio > meta.prio => {
                    self.fifo.swap_remove(i);
                    self.dropped += 1;
                    out.push(SpeechEffect::Discard(w.id));
                }
                Some(_) | None => {
                    self.dropped += 1;
                    out.push(SpeechEffect::Discard(meta.id));
                    return;
                }
            }
        }
        self.seq += 1;
        self.fifo.push(Queued {
            meta,
            at,
            seq: self.seq,
        });
    }

    fn purge(&mut self, keep: impl Fn(&Queued) -> bool, out: &mut Vec<SpeechEffect>) {
        self.fifo.retain(|q| {
            let k = keep(q);
            if !k {
                out.push(SpeechEffect::Discard(q.meta.id));
            }
            k
        });
    }

    /// Highest priority first, oldest first within a priority; stale entries are discarded.
    fn pop(
        &mut self,
        epoch_now: Epoch,
        now: Instant,
        out: &mut Vec<SpeechEffect>,
    ) -> Option<UttMeta> {
        self.purge(|q| q.meta.is_valid(epoch_now), out);
        let quiet = self.quiet_until.is_some_and(|u| now < u);
        let (i, _) = self
            .fifo
            .iter()
            .enumerate()
            .filter(|(_, q)| !(quiet && q.meta.prio == Prio::Notification))
            .min_by_key(|(_, q)| (q.meta.prio, q.seq))?;
        Some(self.fifo.remove(i).meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{Prio, UttId};
    use std::time::Duration;

    fn meta(id: u64, prio: Prio, epoch: u64) -> UttMeta {
        UttMeta {
            id: UttId(id),
            prio,
            epoch: Epoch(epoch),
            survives_focus_change: false,
        }
    }

    #[test]
    fn queued_utterances_play_in_priority_then_arrival_order() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        q.handle(
            SpeechMsg::Intent(meta(2, Prio::Notification, 1)),
            t,
            Epoch(1),
        );
        q.handle(SpeechMsg::Intent(meta(3, Prio::Message, 1)), t, Epoch(1));
        q.handle(SpeechMsg::Intent(meta(4, Prio::Message, 1)), t, Epoch(1));
        assert_eq!(q.queued(), 3);
        let e = q.handle(SpeechMsg::Ended(UttId(1)), t, Epoch(1));
        assert_eq!(e, vec![SpeechEffect::Speak(UttId(3))]);
        let e = q.handle(SpeechMsg::Ended(UttId(3)), t, Epoch(1));
        assert_eq!(e, vec![SpeechEffect::Speak(UttId(4))]);
        let e = q.handle(SpeechMsg::Ended(UttId(4)), t, Epoch(1));
        assert_eq!(e, vec![SpeechEffect::Speak(UttId(2))]);
        assert_eq!(
            q.state(),
            SpeechState::Speaking {
                cur: meta(2, Prio::Notification, 1),
                sent_at: t,
                started: false
            }
        );
    }

    #[test]
    fn stale_queue_entries_are_never_spoken() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        q.handle(SpeechMsg::Intent(meta(2, Prio::Text, 1)), t, Epoch(1));
        // A key press advances the epoch: current is stopped, queued entry is discarded.
        let e = q.handle(SpeechMsg::EpochAdvanced, t, Epoch(2));
        assert!(e.contains(&SpeechEffect::Stop(UttId(1))));
        assert!(e.contains(&SpeechEffect::Discard(UttId(2))));
        assert!(!e.iter().any(|x| matches!(x, SpeechEffect::Speak(_))));
        assert_eq!(q.state(), SpeechState::Idle);
        assert_eq!(q.queued(), 0);
    }

    #[test]
    fn the_fifo_is_bounded_and_drops_the_least_important() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::Intent(meta(0, Prio::Text, 1)), t, Epoch(1));
        for i in 1..=FIFO_CAPACITY as u64 {
            q.handle(
                SpeechMsg::Intent(meta(i, Prio::Notification, 1)),
                t,
                Epoch(1),
            );
        }
        assert_eq!(q.queued(), FIFO_CAPACITY);
        // A more important newcomer evicts the oldest notification.
        let e = q.handle(SpeechMsg::Intent(meta(100, Prio::Message, 1)), t, Epoch(1));
        assert_eq!(e, vec![SpeechEffect::Discard(UttId(1))]);
        assert_eq!(q.queued(), FIFO_CAPACITY);
        // A less important newcomer is itself dropped.
        let e = q.handle(
            SpeechMsg::Intent(meta(101, Prio::Notification, 1)),
            t,
            Epoch(1),
        );
        assert_eq!(e, vec![SpeechEffect::Discard(UttId(101))]);
        assert_eq!(q.dropped_count(), 2);
    }

    #[test]
    fn old_intents_are_dropped_after_reconnect() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::TransportLost, t, Epoch(1));
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        let later = t + Duration::from_secs(3);
        q.handle(SpeechMsg::Intent(meta(2, Prio::Text, 1)), later, Epoch(1));
        let e = q.handle(SpeechMsg::Reconnected, later, Epoch(1));
        assert!(e.contains(&SpeechEffect::Discard(UttId(1))));
        assert!(e.contains(&SpeechEffect::Speak(UttId(2))));
    }

    #[test]
    fn replace_slow_is_counted() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        q.handle(SpeechMsg::Intent(meta(2, Prio::Interrupt, 1)), t, Epoch(1));
        q.handle(SpeechMsg::Tick, t + Duration::from_millis(40), Epoch(1));
        assert_eq!(q.replace_slow_count(), 1);
    }

    #[test]
    fn a_queued_notification_waits_out_the_grace_after_an_epoch_advance() {
        use std::time::Duration;
        let mut q = SpeechQueue::new(true);
        let t = Instant::now();
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        let mut n = meta(2, Prio::Notification, 1);
        n.survives_focus_change = true;
        q.handle(SpeechMsg::Intent(n), t, Epoch(1));
        // The epoch advances: the stale speech stops. The notification must not start in the
        // gap before the focus utterance that caused the advance arrives (it would be cut).
        let e = q.handle(SpeechMsg::EpochAdvanced, t, Epoch(2));
        assert!(
            !e.iter()
                .any(|x| matches!(x, SpeechEffect::Speak(_) | SpeechEffect::Replace { .. })),
            "{e:?}"
        );
        assert!(q.next_wake(t).is_some());
        // After the grace an idle tick starts it.
        let later = t + NOTICE_GRACE + Duration::from_millis(1);
        let e = q.handle(SpeechMsg::Tick, later, Epoch(2));
        assert!(e.contains(&SpeechEffect::Speak(UttId(2))), "{e:?}");
    }

    #[test]
    fn a_fresh_notification_inside_the_grace_waits_and_is_not_cut_by_the_focus() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::Intent(meta(1, Prio::Text, 1)), t, Epoch(1));
        q.handle(SpeechMsg::EpochAdvanced, t, Epoch(2));
        let e = q.handle(
            SpeechMsg::Intent(meta(2, Prio::Notification, 2)),
            t + Duration::from_millis(2),
            Epoch(2),
        );
        assert!(!e.contains(&SpeechEffect::Speak(UttId(2))), "{e:?}");
        assert_eq!(q.queued(), 1);
        assert!(q.next_wake(t + Duration::from_millis(2)).is_some());
        let e = q.handle(
            SpeechMsg::Intent(meta(3, Prio::Interrupt, 2)),
            t + Duration::from_millis(10),
            Epoch(2),
        );
        assert!(
            !e.iter().any(|x| matches!(x, SpeechEffect::Stop(UttId(2)))),
            "{e:?}"
        );
        assert_eq!(
            q.queued(),
            1,
            "the notice still waits behind the focus utterance"
        );
    }

    #[test]
    fn a_fresh_notification_on_an_idle_lane_after_the_grace_starts_at_once() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(SpeechMsg::EpochAdvanced, t, Epoch(2));
        let e = q.handle(
            SpeechMsg::Intent(meta(2, Prio::Notification, 2)),
            t + NOTICE_GRACE + Duration::from_millis(1),
            Epoch(2),
        );
        assert_eq!(e, vec![SpeechEffect::Speak(UttId(2))]);
    }
}

#[cfg(test)]
mod m4b_review_probe {
    use super::*;
    use gorca_model::UttId;
    use std::time::Duration;

    /// P5: a notification that arrives at an idle lane inside the grace (after the epoch advance,
    /// before the focus utterance) must wait like a queued one; else the focus cuts it off.
    #[test]
    fn probe_p5_fresh_notification_inside_grace_waits() {
        let t = Instant::now();
        let mut q = SpeechQueue::new(true);
        q.handle(
            SpeechMsg::Intent(UttMeta { id: UttId(1), prio: Prio::Text, epoch: Epoch(1), survives_focus_change: false }),
            t,
            Epoch(1),
        );
        q.handle(SpeechMsg::EpochAdvanced, t, Epoch(2));
        let n = UttMeta { id: UttId(2), prio: Prio::Notification, epoch: Epoch(2), survives_focus_change: true };
        let e = q.handle(SpeechMsg::Intent(n), t + Duration::from_millis(2), Epoch(2));
        let e2 = q.handle(
            SpeechMsg::Intent(UttMeta { id: UttId(3), prio: Prio::Interrupt, epoch: Epoch(2), survives_focus_change: false }),
            t + Duration::from_millis(10),
            Epoch(2),
        );
        assert!(
            !e.contains(&SpeechEffect::Speak(UttId(2))),
            "notification started inside the grace: {e:?}; then the focus did {e2:?}"
        );
    }
}
