//! Message bookkeeping shared by the SSIP-family speech clients (voicego binary framing and text
//! SSIP): replies come back in command order, events (`7xx`) carry the server's message id, and
//! gorca addresses utterances by its own `UttId`.
//!
//! The tracker is pure (no I/O): a client feeds it parsed replies and events and sends out the
//! [`SpeechEvent`]s it produces. It encodes the protocol facts of
//! `docs/platform/voicego-protocol.md` section 5 and 6:
//!
//! - A `7xx` frame is an event, anything else is the reply to the oldest outstanding command.
//! - `SPEAK` yields `230` (ignored) and then `225-<id>`; the id binds to the oldest unbound
//!   `UttId`. A `701` may arrive before its `225`: such events are parked and replayed on bind.
//! - A `5xx` reply with nothing outstanding is an unsolicited failure of the oldest unfinished
//!   message.
//! - `703 CANCELED` for an utterance that was replaced by our own `STOP` is swallowed. If the
//!   *new* utterance is cancelled right after a pipelined `STOP` before it ever started, a
//!   pre-fix voicegod lost it: the tracker asks the client to resend it once.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;

use gorca_model::UttId;

use crate::{SpeechErr, SpeechEvent};

/// Live message ids kept at most (a hostile or buggy server must not grow memory).
const MAX_LIVE: usize = 64;
/// Events parked while waiting for their `225`.
const MAX_EARLY: usize = 16;
/// Remembered replaced utterances.
const MAX_REPLACED: usize = 8;

/// What the oldest outstanding command is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Expect {
    /// `SET`, `STOP`, `PAUSE`, ...: one reply.
    Simple,
    /// `SPEAK`: `230` and then `225` (or an error) for this utterance.
    Speak(UttId),
}

/// A `7xx` event after parsing.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ServerEvent {
    Begin,
    End,
    Canceled,
    Mark(u32),
}

/// What the client must do besides forwarding events.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Reaction {
    None,
    /// Write these bytes again (a whole `SPEAK` command); the client registers
    /// `Expect::Speak` for it as for any other speak.
    Resend {
        utt: UttId,
        bytes: Vec<u8>,
    },
}

#[derive(Debug)]
struct Guard {
    utt: UttId,
    bytes: Vec<u8>,
    started: bool,
    resent: bool,
}

#[derive(Debug, Default)]
pub struct MsgTracker {
    expects: VecDeque<Expect>,
    ids: HashMap<u64, UttId>,
    order: VecDeque<u64>,
    early: VecDeque<(u64, ServerEvent)>,
    replaced: VecDeque<UttId>,
    guard: Option<Guard>,
    resends: u64,
}

impl MsgTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a command just before its bytes are written.
    pub fn expect(&mut self, e: Expect) {
        self.expects.push_back(e);
    }

    /// `utt` was replaced by our own `STOP`; its `703` is not news.
    pub fn mark_replaced(&mut self, utt: UttId) {
        if self.replaced.len() >= MAX_REPLACED {
            self.replaced.pop_front();
        }
        self.replaced.push_back(utt);
    }

    /// `utt` was written right after a `STOP`. If it is cancelled before it starts, `bytes` (the
    /// complete `SPEAK` command) are sent once more.
    pub fn arm_guard(&mut self, utt: UttId, bytes: Vec<u8>) {
        self.guard = Some(Guard {
            utt,
            bytes,
            started: false,
            resent: false,
        });
    }

    /// Resends requested so far.
    pub fn resends(&self) -> u64 {
        self.resends
    }

    pub fn outstanding(&self) -> usize {
        self.expects.len()
    }

    pub fn live(&self) -> usize {
        self.ids.len()
    }

    /// A non-`7xx` reply frame.
    pub fn on_reply(
        &mut self,
        code: u16,
        msg_id: Option<u64>,
        out: &mut Vec<SpeechEvent>,
    ) -> Reaction {
        match code {
            // Intermediate reply of SPEAK ("OK RECEIVING DATA").
            230 => Reaction::None,
            225 => match self.expects.pop_front() {
                Some(Expect::Speak(u)) => match msg_id {
                    Some(id) => self.bind(u, id, out),
                    None => Reaction::None,
                },
                // Desynchronised (a reply nobody asked for): ignore rather than guess.
                Some(Expect::Simple) | None => Reaction::None,
            },
            400..=599 => {
                match self.expects.front().copied() {
                    Some(Expect::Speak(u)) => {
                        self.expects.pop_front();
                        out.push(SpeechEvent::Error(
                            u,
                            SpeechErr::Other(Arc::from(format!("reply {code}"))),
                        ));
                    }
                    Some(Expect::Simple) => {
                        self.expects.pop_front();
                    }
                    None => {
                        // Unsolicited: terminal failure of the oldest unfinished message.
                        if let Some(id) = self.order.pop_front()
                            && let Some(u) = self.ids.remove(&id)
                        {
                            out.push(SpeechEvent::Error(
                                u,
                                SpeechErr::Other(Arc::from(format!("unsolicited {code}"))),
                            ));
                        }
                    }
                }
                Reaction::None
            }
            _ => {
                if self.expects.front() == Some(&Expect::Simple) {
                    self.expects.pop_front();
                }
                Reaction::None
            }
        }
    }

    fn bind(&mut self, utt: UttId, id: u64, out: &mut Vec<SpeechEvent>) -> Reaction {
        if self.order.len() >= MAX_LIVE
            && let Some(old) = self.order.pop_front()
        {
            self.ids.remove(&old);
        }
        self.ids.insert(id, utt);
        self.order.push_back(id);
        out.push(SpeechEvent::Accepted(utt));
        let mut reaction = Reaction::None;
        let mut i = 0;
        while i < self.early.len() {
            if self.early.get(i).is_some_and(|(m, _)| *m == id) {
                if let Some((_, ev)) = self.early.remove(i) {
                    let r = self.apply(utt, id, ev, out);
                    if r != Reaction::None {
                        reaction = r;
                    }
                }
            } else {
                i += 1;
            }
        }
        reaction
    }

    /// A `7xx` event for server message `msg`.
    pub fn on_event(&mut self, ev: ServerEvent, msg: u64, out: &mut Vec<SpeechEvent>) -> Reaction {
        if let Some(u) = self.ids.get(&msg).copied() {
            return self.apply(u, msg, ev, out);
        }
        if self.early.len() >= MAX_EARLY {
            self.early.pop_front();
        }
        self.early.push_back((msg, ev));
        Reaction::None
    }

    fn forget(&mut self, msg: u64) {
        self.ids.remove(&msg);
        self.order.retain(|m| *m != msg);
    }

    fn apply(
        &mut self,
        utt: UttId,
        msg: u64,
        ev: ServerEvent,
        out: &mut Vec<SpeechEvent>,
    ) -> Reaction {
        match ev {
            ServerEvent::Begin => {
                if let Some(g) = self.guard.as_mut()
                    && g.utt == utt
                {
                    g.started = true;
                }
                out.push(SpeechEvent::Started(utt));
                Reaction::None
            }
            ServerEvent::End => {
                self.forget(msg);
                if self.guard.as_ref().is_some_and(|g| g.utt == utt) {
                    self.guard = None;
                }
                out.push(SpeechEvent::Ended(utt));
                Reaction::None
            }
            ServerEvent::Canceled => {
                self.forget(msg);
                if let Some(i) = self.replaced.iter().position(|u| *u == utt) {
                    self.replaced.remove(i);
                    return Reaction::None;
                }
                if let Some(g) = self.guard.as_mut()
                    && g.utt == utt
                    && !g.started
                    && !g.resent
                {
                    g.resent = true;
                    self.resends += 1;
                    return Reaction::Resend {
                        utt,
                        bytes: g.bytes.clone(),
                    };
                }
                out.push(SpeechEvent::Stopped(utt));
                Reaction::None
            }
            ServerEvent::Mark(n) => {
                out.push(SpeechEvent::Mark(utt, n));
                Reaction::None
            }
        }
    }

    /// The transport died: everything unfinished is stopped, state is reset.
    pub fn fail_all(&mut self, out: &mut Vec<SpeechEvent>) {
        for e in self.expects.drain(..) {
            if let Expect::Speak(u) = e {
                out.push(SpeechEvent::Stopped(u));
            }
        }
        for id in self.order.drain(..) {
            if let Some(u) = self.ids.remove(&id) {
                out.push(SpeechEvent::Stopped(u));
            }
        }
        self.ids.clear();
        self.early.clear();
        self.guard = None;
    }
}

/// Parse the status code at the start of a reply or event block.
pub fn status_code(block: &[u8]) -> Option<u16> {
    let d = block.get(..3)?;
    if !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    Some(u16::from(d[0] - b'0') * 100 + u16::from(d[1] - b'0') * 10 + u16::from(d[2] - b'0'))
}

/// A classified server block (one reply or one event).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Block {
    Reply { code: u16, msg_id: Option<u64> },
    Event { ev: ServerEvent, msg: u64 },
    Garbage,
}

/// Classify the payload of a voicego frame or an assembled SSIP block (`NNN-line\r\n...NNN text`).
pub fn classify(block: &[u8]) -> Block {
    let Some(code) = status_code(block) else {
        return Block::Garbage;
    };
    let text = String::from_utf8_lossy(block);
    let mut lines = text.split("\r\n").filter(|l| !l.is_empty());
    let first = lines.next().unwrap_or("");
    let first_arg = first.get(4..).unwrap_or("").trim();
    if (700..=705).contains(&code) {
        let Ok(msg) = first_arg.parse::<u64>() else {
            return Block::Garbage;
        };
        let ev = match code {
            701 => ServerEvent::Begin,
            702 => ServerEvent::End,
            703 => ServerEvent::Canceled,
            700 => {
                // 700-<msg>, 700-<client>, 700-<mark name>, 700 INDEX MARK
                let name = text
                    .split("\r\n")
                    .filter(|l| !l.is_empty())
                    .nth(2)
                    .and_then(|l| l.get(4..))
                    .unwrap_or("");
                ServerEvent::Mark(name.trim().parse().unwrap_or(0))
            }
            // 704/705 (paused/resumed) carry no state we track.
            _ => return Block::Garbage,
        };
        return Block::Event { ev, msg };
    }
    // `225-<id>` then `225 OK MESSAGE QUEUED`.
    let msg_id = if first.as_bytes().get(3) == Some(&b'-') {
        first_arg.parse::<u64>().ok()
    } else {
        None
    };
    Block::Reply { code, msg_id }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u64) -> UttId {
        UttId(n)
    }

    #[test]
    fn classify_replies_and_events() {
        assert_eq!(
            classify(b"225-17\r\n225 OK MESSAGE QUEUED\r\n"),
            Block::Reply {
                code: 225,
                msg_id: Some(17)
            }
        );
        assert_eq!(
            classify(b"701-3\r\n701-1\r\n701 BEGIN\r\n"),
            Block::Event {
                ev: ServerEvent::Begin,
                msg: 3
            }
        );
        assert_eq!(
            classify(b"703-3\r\n703-1\r\n703 CANCELED\r\n"),
            Block::Event {
                ev: ServerEvent::Canceled,
                msg: 3
            }
        );
        assert_eq!(
            classify(b"700-3\r\n700-1\r\n700-5\r\n700 INDEX MARK\r\n"),
            Block::Event {
                ev: ServerEvent::Mark(5),
                msg: 3
            }
        );
        assert_eq!(
            classify(b"210 OK STOPPED\r\n"),
            Block::Reply {
                code: 210,
                msg_id: None
            }
        );
        assert_eq!(classify(b"xx"), Block::Garbage);
        assert_eq!(classify(b"701-notanumber\r\n701 BEGIN\r\n"), Block::Garbage);
    }

    fn speak(t: &mut MsgTracker, utt: u64, id: u64, out: &mut Vec<SpeechEvent>) {
        t.expect(Expect::Speak(u(utt)));
        t.on_reply(230, None, out);
        t.on_reply(225, Some(id), out);
    }

    #[test]
    fn happy_path_maps_msg_ids_to_utterances() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        speak(&mut t, 1, 100, &mut out);
        t.on_event(ServerEvent::Begin, 100, &mut out);
        t.on_event(ServerEvent::End, 100, &mut out);
        assert_eq!(
            out,
            [
                SpeechEvent::Accepted(u(1)),
                SpeechEvent::Started(u(1)),
                SpeechEvent::Ended(u(1))
            ]
        );
        assert_eq!(t.live(), 0);
    }

    #[test]
    fn events_before_the_225_are_replayed_in_order() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        t.expect(Expect::Speak(u(1)));
        t.on_event(ServerEvent::Begin, 7, &mut out);
        t.on_event(ServerEvent::End, 7, &mut out);
        assert!(out.is_empty());
        t.on_reply(225, Some(7), &mut out);
        assert_eq!(
            out,
            [
                SpeechEvent::Accepted(u(1)),
                SpeechEvent::Started(u(1)),
                SpeechEvent::Ended(u(1))
            ]
        );
    }

    #[test]
    fn replies_are_matched_in_command_order() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        t.expect(Expect::Simple); // STOP
        speak(&mut t, 2, 9, &mut out); // would mis-bind if STOP's reply were skipped
        assert!(out.is_empty());
        t.on_reply(210, None, &mut out);
        t.on_reply(225, Some(9), &mut out);
        assert_eq!(out, [SpeechEvent::Accepted(u(2))]);
    }

    #[test]
    fn replaced_utterance_cancel_is_swallowed() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        speak(&mut t, 1, 1, &mut out);
        out.clear();
        t.mark_replaced(u(1));
        assert_eq!(
            t.on_event(ServerEvent::Canceled, 1, &mut out),
            Reaction::None
        );
        assert!(out.is_empty());
    }

    #[test]
    fn lost_speech_after_stop_is_resent_exactly_once() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        t.arm_guard(u(2), b"SPEAK".to_vec());
        speak(&mut t, 2, 5, &mut out);
        out.clear();
        let r = t.on_event(ServerEvent::Canceled, 5, &mut out);
        assert_eq!(
            r,
            Reaction::Resend {
                utt: u(2),
                bytes: b"SPEAK".to_vec()
            }
        );
        assert!(out.is_empty(), "the cancel is not reported when resending");
        assert_eq!(t.resends(), 1);
        // The resend gets a new id and is cancelled again: now it is reported.
        speak(&mut t, 2, 6, &mut out);
        out.clear();
        assert_eq!(
            t.on_event(ServerEvent::Canceled, 6, &mut out),
            Reaction::None
        );
        assert_eq!(out, [SpeechEvent::Stopped(u(2))]);
    }

    #[test]
    fn a_started_utterance_is_not_resent() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        t.arm_guard(u(2), b"x".to_vec());
        speak(&mut t, 2, 5, &mut out);
        t.on_event(ServerEvent::Begin, 5, &mut out);
        out.clear();
        t.on_event(ServerEvent::Canceled, 5, &mut out);
        assert_eq!(out, [SpeechEvent::Stopped(u(2))]);
        assert_eq!(t.resends(), 0);
    }

    #[test]
    fn errors_attach_to_the_right_message() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        // Error as the reply of a SPEAK.
        t.expect(Expect::Speak(u(1)));
        t.on_reply(502, None, &mut out);
        assert!(matches!(out[0], SpeechEvent::Error(id, _) if id == u(1)));
        out.clear();
        // Unsolicited 5xx after the 225: oldest unfinished message fails.
        speak(&mut t, 2, 8, &mut out);
        out.clear();
        t.on_reply(502, None, &mut out);
        assert!(matches!(out[0], SpeechEvent::Error(id, _) if id == u(2)));
        assert_eq!(t.live(), 0);
    }

    #[test]
    fn fail_all_stops_everything_unfinished() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        speak(&mut t, 1, 1, &mut out);
        t.expect(Expect::Speak(u(2)));
        out.clear();
        t.fail_all(&mut out);
        assert_eq!(out.len(), 2);
        assert!(out.contains(&SpeechEvent::Stopped(u(1))));
        assert!(out.contains(&SpeechEvent::Stopped(u(2))));
        assert_eq!(t.live(), 0);
        assert_eq!(t.outstanding(), 0);
    }

    #[test]
    fn tables_stay_bounded() {
        let mut t = MsgTracker::new();
        let mut out = Vec::new();
        for i in 0..1000 {
            t.on_event(ServerEvent::Begin, i, &mut out);
        }
        for i in 0..1000 {
            speak(&mut t, i, 10_000 + i, &mut out);
        }
        assert!(t.live() <= MAX_LIVE);
    }
}
