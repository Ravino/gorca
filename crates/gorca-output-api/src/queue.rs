//! Admission to the bounded command queue in front of a speech writer (S-3, review F10).
//!
//! The rule: **a stop always gets through**. When the server stops reading and the queue is full:
//!
//! - queued plain `Speak`s are evicted (their utterances are reported back so the owner can send
//!   `Stopped`), queued `Replace`s are demoted to their `STOP` half;
//! - a `Stop`/`StopAll` that still finds no room replaces the oldest `Pause`/`Resume`/`SetParams`,
//!   or is merged into a queued stop (`StopAll` upgrades it);
//! - a `Speak` or `Replace` never removes a queued stop, and an incoming `Replace` makes the plain
//!   `Stop`s before it redundant (it contains the same `STOP`).
//!
//! The function is generic over the queue element so the failover facade, whose elements carry a
//! timestamp, shares it with the protocol clients.

use std::collections::VecDeque;

use gorca_model::UttId;

use crate::{SpeechCmd, SubmitError};

fn is_stop(c: &SpeechCmd) -> bool {
    matches!(c, SpeechCmd::Stop { .. } | SpeechCmd::StopAll)
}

fn is_droppable_control(c: &SpeechCmd) -> bool {
    matches!(
        c,
        SpeechCmd::Pause | SpeechCmd::Resume | SpeechCmd::SetParams(_)
    )
}

/// The utterance a command would start.
pub fn started_utt(c: &SpeechCmd) -> Option<UttId> {
    match c {
        SpeechCmd::Speak { id, .. } => Some(*id),
        SpeechCmd::Replace { then, .. } => started_utt(then),
        SpeechCmd::Stop { .. }
        | SpeechCmd::StopAll
        | SpeechCmd::Pause
        | SpeechCmd::Resume
        | SpeechCmd::SetParams(_)
        | SpeechCmd::Configure(_) => None,
    }
}

/// Queue `item` (whose command `cmd_of` / `cmd_mut` expose) with at most `cap` entries.
///
/// Returns the utterances that will never be spoken because they were evicted or demoted; the
/// caller reports each as `SpeechEvent::Stopped`. `Err(Full)` only when the queue holds nothing
/// that may be dropped and the command is not a stop.
pub fn admit<T>(
    q: &mut VecDeque<T>,
    item: T,
    cap: usize,
    cmd_of: impl Fn(&T) -> &SpeechCmd,
    cmd_mut: impl Fn(&mut T) -> &mut SpeechCmd,
) -> Result<Vec<UttId>, SubmitError> {
    let mut gone = Vec::new();
    // A Replace carries the STOP of every plain Stop queued before it.
    if matches!(cmd_of(&item), SpeechCmd::Replace { .. }) {
        q.retain(|e| !matches!(cmd_of(e), SpeechCmd::Stop { .. }));
    }
    if q.len() >= cap {
        // Evict plain speaks; demote replaces to their STOP half.
        let mut kept = VecDeque::with_capacity(q.len());
        for mut e in q.drain(..) {
            let evict = matches!(cmd_of(&e), SpeechCmd::Speak { .. });
            if evict {
                gone.extend(started_utt(cmd_of(&e)));
                continue;
            }
            if let SpeechCmd::Replace { stop, then } = cmd_of(&e) {
                gone.extend(started_utt(then));
                let id = *stop;
                *cmd_mut(&mut e) = SpeechCmd::Stop { id };
            }
            kept.push_back(e);
        }
        *q = kept;
    }
    if q.len() >= cap {
        match cmd_of(&item) {
            SpeechCmd::Stop { .. } | SpeechCmd::StopAll => {
                if let Some(pos) = q.iter().position(|e| is_droppable_control(cmd_of(e))) {
                    q.remove(pos);
                } else {
                    // Only stops are queued: merge into the newest one.
                    if matches!(cmd_of(&item), SpeechCmd::StopAll)
                        && let Some(last) = q.iter_mut().rev().find(|e| is_stop(cmd_of(e)))
                    {
                        *cmd_mut(last) = SpeechCmd::StopAll;
                    }
                    return Ok(gone);
                }
            }
            SpeechCmd::Speak { .. }
            | SpeechCmd::Replace { .. }
            | SpeechCmd::Pause
            | SpeechCmd::Resume
            | SpeechCmd::SetParams(_)
            | SpeechCmd::Configure(_) => return Err(SubmitError::Full),
        }
    }
    q.push_back(item);
    Ok(gone)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Verbalized, VoiceRef};
    use std::sync::Arc;

    fn speak(id: u64) -> SpeechCmd {
        SpeechCmd::Speak {
            id: UttId(id),
            text: Verbalized {
                text: Arc::from("x"),
                lang: None,
                pitch_delta: 0,
            },
            voice: VoiceRef::Default,
            marks: false,
        }
    }

    fn replace(stop: u64, id: u64) -> SpeechCmd {
        SpeechCmd::Replace {
            stop: UttId(stop),
            then: Box::new(speak(id)),
        }
    }

    fn put(q: &mut VecDeque<SpeechCmd>, c: SpeechCmd) -> Result<Vec<UttId>, SubmitError> {
        admit(q, c, 4, |c| c, |c| c)
    }

    fn full_of_speaks() -> VecDeque<SpeechCmd> {
        let mut q = VecDeque::new();
        for i in 1..=4 {
            put(&mut q, speak(i)).unwrap();
        }
        q
    }

    #[test]
    fn a_stop_evicts_queued_speaks_and_reports_them() {
        let mut q = full_of_speaks();
        let gone = put(&mut q, SpeechCmd::Stop { id: UttId(9) }).unwrap();
        assert_eq!(gone, [UttId(1), UttId(2), UttId(3), UttId(4)]);
        assert_eq!(q, [SpeechCmd::Stop { id: UttId(9) }]);
    }

    #[test]
    fn a_speak_never_removes_a_queued_stop_or_the_stop_half_of_a_replace() {
        let mut q = VecDeque::new();
        put(&mut q, SpeechCmd::Stop { id: UttId(1) }).unwrap();
        for i in 2..=4 {
            put(&mut q, speak(i)).unwrap();
        }
        let gone = put(&mut q, speak(5)).unwrap();
        assert_eq!(gone, [UttId(2), UttId(3), UttId(4)]);
        assert_eq!(q, [SpeechCmd::Stop { id: UttId(1) }, speak(5)]);

        let mut q = VecDeque::new();
        put(&mut q, replace(2, 3)).unwrap();
        for i in 4..=6 {
            put(&mut q, speak(i)).unwrap();
        }
        let gone = put(&mut q, speak(7)).unwrap();
        assert_eq!(gone, [UttId(3), UttId(4), UttId(5), UttId(6)]);
        assert_eq!(q, [SpeechCmd::Stop { id: UttId(2) }, speak(7)]);
    }

    #[test]
    fn a_replace_makes_earlier_plain_stops_redundant() {
        let mut q = VecDeque::new();
        for i in 1..=4 {
            put(&mut q, SpeechCmd::Stop { id: UttId(i) }).unwrap();
        }
        put(&mut q, replace(9, 10)).unwrap();
        assert_eq!(q, [replace(9, 10)]);
    }

    #[test]
    fn a_stop_gets_through_a_queue_of_controls_and_stops() {
        let mut q = VecDeque::new();
        put(&mut q, SpeechCmd::Pause).unwrap();
        put(&mut q, SpeechCmd::Resume).unwrap();
        put(&mut q, SpeechCmd::Stop { id: UttId(1) }).unwrap();
        put(&mut q, SpeechCmd::Pause).unwrap();
        put(&mut q, SpeechCmd::StopAll).unwrap();
        assert!(q.contains(&SpeechCmd::StopAll));
        assert!(q.len() <= 4);
        // All stops: merge, never grow, never fail.
        let mut q = VecDeque::new();
        for i in 1..=4 {
            put(&mut q, SpeechCmd::Stop { id: UttId(i) }).unwrap();
        }
        put(&mut q, SpeechCmd::StopAll).unwrap();
        assert_eq!(q.len(), 4);
        assert!(q.contains(&SpeechCmd::StopAll));
        put(&mut q, SpeechCmd::Stop { id: UttId(7) }).unwrap();
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn a_speak_is_refused_only_when_nothing_may_be_dropped() {
        let mut q = VecDeque::new();
        for _ in 0..4 {
            put(&mut q, SpeechCmd::Pause).unwrap();
        }
        assert_eq!(put(&mut q, speak(1)), Err(SubmitError::Full));
        assert_eq!(q.len(), 4);
    }

    #[test]
    fn below_capacity_nothing_is_touched() {
        let mut q = VecDeque::new();
        put(&mut q, speak(1)).unwrap();
        put(&mut q, replace(1, 2)).unwrap();
        assert_eq!(q.len(), 2);
    }
}
