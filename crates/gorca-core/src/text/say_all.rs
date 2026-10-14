//! Say-all over a native text object (ADR-0007 section 11): read from the caret to the end in
//! sentence chunks, keeping up to three chunks queued at the speech lane so there is no gap
//! between them, following the server's progress, and moving the caret to the chunk being
//! spoken, so that any key that stops the reading acts from where the reading is.
//!
//! Stopping is the epoch (E-2): any key press or focus change advances it, speech is cancelled
//! by the speech lane without a message, and the next message the Reader handles finds the
//! epoch newer than the reading's and ends it ([`SayAll::stopped_by`]). Nothing here holds the
//! Reader: the text arrives in windows fetched by tasks, and the machine only reacts.

use gorca_model::{Epoch, NodeKey, UttId};
use gorca_speech::LaneProgress;
use gorca_text::chunk::next_chunk;
use gorca_text::space::{CharsSpace, Lookup};
use smallvec::SmallVec;
use std::ops::Range;

use super::TextReqId;

/// Chunks submitted ahead of the one being spoken (SA-2).
pub const LOOK_AHEAD: usize = 3;
/// The longest chunk, in scalars (SA-1).
pub const MAX_CHUNK: u32 = 400;
/// Scalars on each side of the fetch centre when the buffer needs more text.
pub const FETCH_RADIUS: u32 = 4096;
/// A reading that hears nothing from the speech lane for this long is over. Progress reports
/// are the reading's clock; the lane reports every chunk it loses, so this is the backstop
/// for a report lost on the way (a full channel), not the normal end.
pub const WATCHDOG: std::time::Duration = std::time::Duration::from_secs(90);
/// A chunk is cut before the buffer's end only if at least this much text lies ahead of it
/// (otherwise the sentence might continue past the buffer).
const SAFE_AHEAD: u32 = 1200;

/// A submitted chunk.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Chunk {
    pub utt: UttId,
    pub range: Range<u32>,
}

#[derive(Debug, Default)]
pub enum SayAll {
    #[default]
    Idle,
    Running(Run),
}

// Latches of one reading (secret, filled, exhausted, started); an enum would only rename them.
#[allow(clippy::struct_excessive_bools)]
pub struct Run {
    pub node: NodeKey,
    /// The epoch of the reading: a newer one ends it.
    pub epoch: Epoch,
    /// Where the reading started.
    pub from: u32,
    /// The text ahead: `buf[0]` is at scalar offset `base`.
    base: u32,
    buf: Vec<char>,
    len: u32,
    /// Offset of the next chunk to prepare.
    next: u32,
    pub queued: SmallVec<[Chunk; LOOK_AHEAD]>,
    /// The window fetch in flight.
    pub fetch: Option<TextReqId>,
    /// The chunk the server started last: the reading position.
    pub started: Option<Range<u32>>,
    /// Every chunk is submitted: the end of the text was reached.
    pub exhausted: bool,
    pub secret: bool,
    /// The first window arrived.
    filled: bool,
    /// Chunks submitted so far; the first interrupts, the rest queue behind it.
    submitted: u32,
    /// When the speech lane last said anything about this reading.
    pub last_progress: std::time::Instant,
    /// Where the caret was last put (or already was): a chunk starting there needs no move.
    pub placed: Option<u32>,
    /// The offset a window was last asked for: asking again for the same one means the
    /// application cannot give more.
    fetched_for: Option<u32>,
}

impl std::fmt::Debug for Run {
    // Lengths only: the text of a password field must not reach a log (PW-2).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("node", &self.node)
            .field("epoch", &self.epoch)
            .field("from", &self.from)
            .field("base", &self.base)
            .field("buffered", &self.buf.len())
            .field("len", &self.len)
            .field("next", &self.next)
            .field("queued", &self.queued)
            .field("started", &self.started)
            .field("exhausted", &self.exhausted)
            .finish_non_exhaustive()
    }
}

/// What the Reader must do for the reading.
#[derive(Eq, PartialEq)]
pub enum Act {
    /// Submit this chunk text as an utterance with `utt`.
    Submit {
        utt: UttId,
        range: Range<u32>,
        text: String,
        /// The first chunk of the reading: it interrupts. The others are queued.
        first: bool,
    },
    /// Fetch text around this offset.
    Fetch { around: u32 },
}

impl std::fmt::Debug for Act {
    // Lengths only, like `Run`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Act::Submit {
                utt,
                range,
                text,
                first,
            } => f
                .debug_struct("Submit")
                .field("utt", utt)
                .field("range", range)
                .field("chars", &text.chars().count())
                .field("first", first)
                .finish(),
            Act::Fetch { around } => f.debug_struct("Fetch").field("around", around).finish(),
        }
    }
}

impl Run {
    pub fn new(
        node: NodeKey,
        epoch: Epoch,
        from: u32,
        secret: bool,
        now: std::time::Instant,
    ) -> Self {
        Self {
            node,
            epoch,
            from,
            base: from,
            buf: Vec::new(),
            len: 0,
            next: from,
            queued: SmallVec::new(),
            fetch: None,
            started: None,
            exhausted: false,
            secret,
            filled: false,
            submitted: 0,
            last_progress: now,
            placed: Some(from),
            fetched_for: None,
        }
    }

    /// The text fetched for the reading arrived.
    pub fn fill(&mut self, base: u32, text: &str, len: u32) {
        self.base = base;
        self.buf.clear();
        self.buf.extend(text.chars());
        self.len = len;
        self.fetch = None;
        self.filled = true;
    }

    /// The moment the watchdog ends the reading.
    pub fn deadline(&self) -> std::time::Instant {
        self.last_progress + WATCHDOG
    }

    /// Has the lane been silent for too long?
    pub fn expired(&self, now: std::time::Instant) -> bool {
        now >= self.deadline()
    }

    /// Prepare chunks until the look-ahead is full or text is missing.
    pub fn pump(&mut self, mut new_utt: impl FnMut() -> UttId) -> SmallVec<[Act; 4]> {
        let mut acts = SmallVec::new();
        if self.fetch.is_some() {
            return acts;
        }
        while self.queued.len() < LOOK_AHEAD && !self.exhausted {
            if !self.filled {
                // Nothing fetched yet.
                acts.push(Act::Fetch { around: self.next });
                return acts;
            }
            if self.next >= self.len {
                self.exhausted = true;
                break;
            }
            let end = self
                .base
                .saturating_add(u32::try_from(self.buf.len()).unwrap_or(u32::MAX));
            let ahead = end.saturating_sub(self.next);
            if self.next < self.base || (end < self.len && ahead < SAFE_AHEAD) {
                if self.fetched_for != Some(self.next) {
                    self.fetched_for = Some(self.next);
                    acts.push(Act::Fetch { around: self.next });
                    return acts;
                }
                // Asked already and it did not help: read what there is.
                if self.next < self.base || self.next >= end {
                    self.exhausted = true;
                    break;
                }
            }
            let rel = (self.next - self.base) as usize;
            let space = CharsSpace::new(&self.buf, 0);
            let Lookup::Ready(r) = next_chunk(&space, u32::try_from(rel).unwrap_or(0), MAX_CHUNK)
            else {
                self.exhausted = true;
                break;
            };
            if r.end <= r.start {
                self.exhausted = true;
                break;
            }
            let chunk: String = self.buf[r.start as usize..r.end as usize].iter().collect();
            let range = self.base + r.start..self.base + r.end;
            self.next = range.end;
            if chunk.chars().all(char::is_whitespace) {
                continue;
            }
            let first = self.submitted == 0;
            self.submitted += 1;
            let utt = new_utt();
            self.queued.push(Chunk {
                utt,
                range: range.clone(),
            });
            acts.push(Act::Submit {
                utt,
                range,
                text: chunk,
                first,
            });
        }
        acts
    }

    /// The speech lane reported progress. Reports arrive in the order the server speaks, so a
    /// report about a chunk also settles every chunk queued before it: a lost report cannot
    /// leave a phantom chunk behind.
    pub fn progress(&mut self, p: LaneProgress, now: std::time::Instant) -> Progressed {
        let mut out = Progressed::default();
        let (u, kind) = match p {
            LaneProgress::Started(u) => (u, 0),
            LaneProgress::Ended(u) => (u, 1),
            LaneProgress::Stopped(u) => (u, 2),
        };
        let Some(i) = self.queued.iter().position(|c| c.utt == u) else {
            return out;
        };
        self.last_progress = now;
        match kind {
            0 => {
                let range = self.queued[i].range.clone();
                self.queued.drain(..i);
                out.pump = i > 0;
                if self.placed != Some(range.start) {
                    self.placed = Some(range.start);
                    out.place = Some(range.start);
                }
                self.started = Some(range);
            }
            1 => {
                let c = self.queued[i].clone();
                self.queued.drain(..=i);
                self.started = Some(c.range);
                out.pump = true;
            }
            _ => {
                self.queued.remove(i);
                out.pump = true;
            }
        }
        out
    }

    /// The reading is over because every chunk was spoken.
    pub fn is_done(&self) -> bool {
        self.exhausted && self.queued.is_empty() && self.fetch.is_none()
    }

    /// Where the caret goes after the last chunk.
    pub fn end_caret(&self) -> u32 {
        self.started.as_ref().map_or(self.from, |r| r.end)
    }
}

/// What a progress report asks of the Reader.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct Progressed {
    /// Chunks were settled: prepare more.
    pub pump: bool,
    /// Put the caret here: the chunk now being spoken starts there.
    pub place: Option<u32>,
}

impl SayAll {
    /// Has anything newer than the reading happened (a key, a focus change)?
    pub fn stopped_by(&self, current: Epoch) -> bool {
        match self {
            SayAll::Idle => false,
            SayAll::Running(r) => current > r.epoch,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId};
    use std::time::{Duration, Instant};

    fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(1),
        }
    }

    fn ids() -> impl FnMut() -> UttId {
        let mut n = 0;
        move || {
            n += 1;
            UttId(n)
        }
    }

    fn texts(acts: &[Act]) -> Vec<String> {
        acts.iter()
            .filter_map(|a| match a {
                Act::Submit { text, .. } => Some(text.clone()),
                Act::Fetch { .. } => None,
            })
            .collect()
    }

    const TEXT: &str = "One. Two. Three. Four. Five.";

    fn now() -> Instant {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *START.get_or_init(Instant::now)
    }

    fn run_with(text: &str) -> Run {
        let mut r = Run::new(key(), Epoch(5), 0, false, now());
        let len = u32::try_from(text.chars().count()).unwrap_or(0);
        r.fill(0, text, len);
        r
    }

    #[test]
    fn a_new_reading_asks_for_text_first() {
        let mut r = Run::new(key(), Epoch(1), 7, false, now());
        let acts = r.pump(ids());
        assert_eq!(acts.as_slice(), &[Act::Fetch { around: 7 }]);
    }

    #[test]
    fn three_chunks_go_at_once_only_the_first_interrupts() {
        let mut r = run_with(TEXT);
        let acts = r.pump(ids());
        assert_eq!(texts(&acts), ["One. ", "Two. ", "Three. "]);
        let firsts: Vec<bool> = acts
            .iter()
            .filter_map(|a| match a {
                Act::Submit { first, .. } => Some(*first),
                Act::Fetch { .. } => None,
            })
            .collect();
        assert_eq!(firsts, [true, false, false]);
        assert!(r.pump(ids()).is_empty(), "full until a chunk ends");
    }

    #[test]
    fn progress_pops_and_refills_and_the_end_finishes() {
        let mut r = run_with(TEXT);
        let mut next = ids();
        let _ = r.pump(&mut next);
        let p = r.progress(LaneProgress::Started(UttId(1)), now());
        assert_eq!(
            p,
            Progressed::default(),
            "the first chunk starts where the caret is"
        );
        assert_eq!(r.started, Some(0..5));
        let p = r.progress(LaneProgress::Ended(UttId(1)), now());
        assert!(p.pump);
        assert_eq!(texts(&r.pump(&mut next)), ["Four. "]);
        let p = r.progress(LaneProgress::Started(UttId(2)), now());
        assert_eq!(p.place, Some(5), "the caret follows the reading");
        let _ = r.progress(LaneProgress::Ended(UttId(2)), now());
        assert_eq!(texts(&r.pump(&mut next)), ["Five."]);
        assert!(!r.is_done());
        for u in 3..=5 {
            let _ = r.progress(LaneProgress::Ended(UttId(u)), now());
        }
        assert!(r.pump(&mut next).is_empty());
        assert!(r.is_done());
        assert_eq!(r.end_caret(), 28);
    }

    /// Probe P2 of the M3 review: a lost report must not leave a phantom chunk.
    #[test]
    fn a_report_settles_the_chunks_before_it() {
        let mut r = run_with("One. Two.");
        let _ = r.pump(ids());
        // `Ended(1)` and `Started(2)` are lost; `Ended(2)` arrives.
        let p = r.progress(LaneProgress::Ended(UttId(2)), now());
        assert!(p.pump);
        assert!(r.queued.is_empty(), "chunk 1 was settled with chunk 2");
        let _ = r.pump(ids());
        assert!(r.is_done());

        // A `Started` for a later chunk settles the earlier ones too.
        let mut r = run_with(TEXT);
        let _ = r.pump(ids());
        let p = r.progress(LaneProgress::Started(UttId(3)), now());
        assert!(p.pump);
        assert_eq!(r.queued.len(), 1);
        assert_eq!(p.place, Some(10));
    }

    #[test]
    fn a_stopped_chunk_is_forgotten() {
        let mut r = run_with(TEXT);
        let _ = r.pump(ids());
        let p = r.progress(LaneProgress::Stopped(UttId(2)), now());
        assert!(p.pump);
        assert_eq!(r.queued.len(), 2);
        // An unknown id changes nothing and is not progress.
        let before = r.last_progress;
        let _ = r.progress(
            LaneProgress::Ended(UttId(99)),
            now() + Duration::from_secs(5),
        );
        assert_eq!(r.last_progress, before);
    }

    #[test]
    fn the_watchdog_ends_a_silent_reading() {
        let r = run_with(TEXT);
        assert!(!r.expired(now() + Duration::from_secs(89)));
        assert!(r.expired(now() + WATCHDOG));
    }

    #[test]
    fn a_long_text_asks_for_more_before_the_buffer_runs_out() {
        // 3000 scalars of text in a buffer that holds the first 1500.
        let sentence = "Word word word word word. ";
        let text: String = sentence.repeat(60);
        let mut r = Run::new(key(), Epoch(1), 0, false, now());
        r.fill(0, &text.chars().take(1500).collect::<String>(), 3000);
        let mut next = ids();
        let mut fetches = 0;
        let mut spoken = 0;
        for _ in 0..400 {
            let acts = r.pump(&mut next);
            for a in &acts {
                match a {
                    Act::Submit { .. } => spoken += 1,
                    Act::Fetch { around } => {
                        fetches += 1;
                        let base = around.saturating_sub(10);
                        let t: String = text.chars().skip(base as usize).take(1500).collect();
                        r.fill(base, &t, 3000);
                    }
                }
            }
            if r.is_done() {
                break;
            }
            let utts: Vec<UttId> = r.queued.iter().map(|c| c.utt).collect();
            for u in utts {
                let _ = r.progress(LaneProgress::Ended(u), now());
            }
        }
        assert!(fetches >= 1, "the buffer was refilled");
        assert!(r.is_done(), "the whole text was read");
        assert_eq!(spoken, 60, "one chunk per sentence");
    }

    #[test]
    fn whitespace_only_chunks_are_skipped_and_stop_follows_the_epoch() {
        let mut r = run_with("a.   \n\n b.");
        let acts = r.pump(ids());
        assert!(texts(&acts).iter().all(|t| !t.trim().is_empty()));
        let s = SayAll::Running(r);
        assert!(!s.stopped_by(Epoch(5)));
        assert!(s.stopped_by(Epoch(6)));
        assert!(!SayAll::Idle.stopped_by(Epoch(99)));
    }

    #[test]
    fn an_empty_text_ends_at_once() {
        let mut r = Run::new(key(), Epoch(1), 0, false, now());
        r.fill(0, "", 0);
        let _ = r.pump(ids());
        assert!(r.is_done());
    }
}
