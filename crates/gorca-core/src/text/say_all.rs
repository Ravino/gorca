//! Say-all over a native text object (ADR-0007 section 11): read from the caret to the end in
//! sentence chunks, keeping up to three chunks queued at the speech server so there is no gap
//! between them, following the server's progress, and leaving the caret where the reading
//! stopped.
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
/// The first chunk goes alone: the speech lane gives an utterance of a newer epoch the
/// interrupting slot, and chunks submitted in the same breath would replace each other there.
/// The rest follow once the server reports the first one started, or after this long (a
/// backend without progress reports), when the lane has certainly taken it.
pub const FIRST_CHUNK_GATE: std::time::Duration = std::time::Duration::from_millis(150);
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
    /// When the first chunk was submitted.
    pub first_at: Option<std::time::Instant>,
    /// The server reported a chunk started (or ended).
    pub started_any: bool,
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
    },
    /// Fetch text around this offset.
    Fetch { around: u32 },
}

impl std::fmt::Debug for Act {
    // Lengths only, like `Run`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Act::Submit { utt, range, text } => f
                .debug_struct("Submit")
                .field("utt", utt)
                .field("range", range)
                .field("chars", &text.chars().count())
                .finish(),
            Act::Fetch { around } => f.debug_struct("Fetch").field("around", around).finish(),
        }
    }
}

impl Run {
    pub fn new(node: NodeKey, epoch: Epoch, from: u32, secret: bool) -> Self {
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
            first_at: None,
            started_any: false,
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

    /// May more than the first chunk be submitted at `now`?
    pub fn gate_open(&self, now: std::time::Instant) -> bool {
        self.started_any || self.first_at.is_some_and(|t| now >= t + FIRST_CHUNK_GATE)
    }

    /// When the gate opens, if it is shut and waiting on the clock.
    pub fn gate_deadline(&self) -> Option<std::time::Instant> {
        if self.started_any || self.queued.is_empty() {
            None
        } else {
            self.first_at.map(|t| t + FIRST_CHUNK_GATE)
        }
    }

    /// Prepare chunks until the look-ahead is full or text is missing.
    pub fn pump(
        &mut self,
        now: std::time::Instant,
        mut new_utt: impl FnMut() -> UttId,
    ) -> SmallVec<[Act; 4]> {
        let mut acts = SmallVec::new();
        if self.fetch.is_some() {
            return acts;
        }
        let room = if self.gate_open(now) { LOOK_AHEAD } else { 1 };
        while self.queued.len() < room && !self.exhausted {
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
            if self.first_at.is_none() {
                self.first_at = Some(now);
            }
            let utt = new_utt();
            self.queued.push(Chunk {
                utt,
                range: range.clone(),
            });
            acts.push(Act::Submit {
                utt,
                range,
                text: chunk,
            });
        }
        acts
    }

    /// The speech server reported progress.
    pub fn progress(&mut self, p: LaneProgress) -> bool {
        match p {
            LaneProgress::Started(u) => {
                if let Some(c) = self.queued.iter().find(|c| c.utt == u) {
                    self.started = Some(c.range.clone());
                    // The first chunk is out of the lane's slot: the others may follow.
                    self.started_any = true;
                    return true;
                }
                false
            }
            LaneProgress::Ended(u) => {
                if let Some(i) = self.queued.iter().position(|c| c.utt == u) {
                    self.started_any = true;
                    let c = self.queued.remove(i);
                    self.started = Some(c.range.start..c.range.end);
                    return true;
                }
                false
            }
            LaneProgress::Stopped(u) => {
                self.queued.retain(|c| c.utt != u);
                false
            }
        }
    }

    /// Where the caret goes when the reading stops: the start of the chunk being spoken.
    pub fn stop_caret(&self) -> u32 {
        self.started.as_ref().map_or(self.from, |r| r.start)
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

    /// A clock that moves a second with every reading, so the first-chunk gate is open at the
    /// second pump of every test that is not about the gate.
    fn t0() -> std::time::Instant {
        use std::sync::atomic::{AtomicU64, Ordering};
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        static TICKS: AtomicU64 = AtomicU64::new(0);
        *START.get_or_init(std::time::Instant::now)
            + std::time::Duration::from_secs(TICKS.fetch_add(1, Ordering::Relaxed))
    }

    fn run_with(text: &str) -> Run {
        let mut r = Run::new(key(), Epoch(5), 0, false);
        let len = u32::try_from(text.chars().count()).unwrap_or(0);
        r.fill(0, text, len);
        r
    }

    #[test]
    fn a_new_reading_asks_for_text_first() {
        let mut r = Run::new(key(), Epoch(1), 7, false);
        let acts = r.pump(t0(), ids());
        assert_eq!(acts.as_slice(), &[Act::Fetch { around: 7 }]);
    }

    #[test]
    fn the_first_chunk_goes_alone_and_the_look_ahead_follows_its_start() {
        let mut r = run_with(TEXT);
        let mut next = ids();
        let now = t0();
        let acts = r.pump(now, &mut next);
        assert_eq!(
            texts(&acts),
            ["One. "],
            "alone: the lane's slot takes only one"
        );
        assert!(r.pump(now, &mut next).is_empty(), "the gate is shut");
        assert_eq!(r.gate_deadline(), Some(now + FIRST_CHUNK_GATE));
        // The server started it: the look-ahead of three follows.
        assert!(r.progress(LaneProgress::Started(UttId(1))));
        let acts = r.pump(now, &mut next);
        assert_eq!(texts(&acts), ["Two. ", "Three. "]);
        assert_eq!(r.queued.len(), 3);
        assert!(r.pump(now, &mut next).is_empty(), "full until a chunk ends");
    }

    #[test]
    fn without_progress_reports_the_gate_opens_by_the_clock() {
        let mut r = run_with(TEXT);
        let mut next = ids();
        let now = t0();
        let _ = r.pump(now, &mut next);
        let later = now + FIRST_CHUNK_GATE;
        assert!(r.gate_open(later));
        let acts = r.pump(later, &mut next);
        assert_eq!(texts(&acts), ["Two. ", "Three. "]);
    }

    #[test]
    fn progress_pops_and_refills_and_the_end_finishes() {
        let mut r = run_with(TEXT);
        let mut next = ids();
        let _ = r.pump(t0(), &mut next);
        // `Started` sets the position and opens the gate; it does not pop.
        assert!(r.progress(LaneProgress::Started(UttId(1))));
        assert_eq!(r.started, Some(0..5));
        let _ = r.pump(t0(), &mut next);
        assert!(r.progress(LaneProgress::Ended(UttId(1))));
        let acts = r.pump(t0(), &mut next);
        assert_eq!(texts(&acts), ["Four. "]);
        assert!(r.progress(LaneProgress::Ended(UttId(2))));
        let acts = r.pump(t0(), &mut next);
        assert_eq!(texts(&acts), ["Five."]);
        assert!(!r.is_done());
        for u in 3..=5 {
            assert!(r.progress(LaneProgress::Ended(UttId(u))));
        }
        // The Reader pumps after every `Ended`; the pump notices there is no more text.
        assert!(r.pump(t0(), &mut next).is_empty());
        assert!(r.is_done());
        assert_eq!(r.end_caret(), 28);
    }

    #[test]
    fn started_marks_the_caret_for_a_stop() {
        let mut r = run_with(TEXT);
        let mut next = ids();
        let _ = r.pump(t0(), &mut next);
        let _ = r.progress(LaneProgress::Started(UttId(1)));
        let _ = r.pump(t0(), &mut next);
        let _ = r.progress(LaneProgress::Started(UttId(2)));
        assert_eq!(r.stop_caret(), 5);
        // A reading that never started a chunk returns to where it began.
        let fresh = Run::new(key(), Epoch(1), 9, false);
        assert_eq!(fresh.stop_caret(), 9);
    }

    #[test]
    fn a_long_text_asks_for_more_before_the_buffer_runs_out() {
        // 3000 scalars of text in a buffer that holds the first 1500.
        let sentence = "Word word word word word. ";
        let text: String = sentence.repeat(60);
        let mut r = Run::new(key(), Epoch(1), 0, false);
        r.fill(0, &text.chars().take(1500).collect::<String>(), 3000);
        let mut next = ids();
        let mut fetches = 0;
        let mut spoken = 0;
        for _ in 0..400 {
            let acts = r.pump(t0(), &mut next);
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
            // Drain the queue as the server would.
            let utts: Vec<UttId> = r.queued.iter().map(|c| c.utt).collect();
            for u in utts {
                let _ = r.progress(LaneProgress::Ended(u));
            }
        }
        assert!(fetches >= 1, "the buffer was refilled");
        assert!(r.is_done(), "the whole text was read");
        assert_eq!(spoken, 60, "one chunk per sentence");
    }

    #[test]
    fn whitespace_only_chunks_are_skipped_and_stop_follows_the_epoch() {
        let mut r = run_with("a.   \n\n b.");
        let acts = r.pump(t0(), ids());
        assert!(texts(&acts).iter().all(|t| !t.trim().is_empty()));
        let s = SayAll::Running(r);
        assert!(!s.stopped_by(Epoch(5)));
        assert!(s.stopped_by(Epoch(6)));
        assert!(!SayAll::Idle.stopped_by(Epoch(99)));
    }

    #[test]
    fn an_empty_text_ends_at_once() {
        let mut r = Run::new(key(), Epoch(1), 0, false);
        r.fill(0, "", 0);
        let _ = r.pump(t0(), ids());
        assert!(r.is_done());
    }
}
