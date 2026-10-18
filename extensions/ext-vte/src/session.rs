// SPDX-License-Identifier: GPL-3.0-or-later
//! The terminal presenter policy (ADR-0001 T-2): one [`Session`] per terminal widget.
//!
//! A session is a pure state machine over [`TermInput`] and a millisecond clock the host owns.
//! It keeps two screens: `base`, the screen as of the last reading, and `live`, the same screen
//! with every `text-changed` of the meantime applied when the payloads allow it. Every event
//! only marks the terminal dirty; speech about screen content happens once the output has been
//! quiet for the settle time (T-1), by comparing `base` with the new screen line by line (T-2).
//! What a key explains (typed characters the shell echoes, an erase, a cursor motion, a history
//! recall) is spoken from the key, and the matching screen change is then recognised and not
//! spoken a second time.

use std::collections::VecDeque;

use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermEdit, TermInput, TermKey, TermSnapshot,
    TerminalHooks, Unit, Urgency,
};

use gorca_ext_api::terminal::{DEFAULT_WORD_LEVEL, WordLevel, word_at, word_motion};

use crate::prompt::Prompt;
use crate::screen::{Diff, DiffBuf, Screen, diff, diff_with};

/// A screen read that takes longer than this failed.
const FETCH_TIMEOUT_MS: Ms = 400;
/// How long a key is believed to explain what follows it.
const EXPECT_MS: Ms = 1500;
/// How long a typed character waits for the shell's echo.
const PENDING_MS: Ms = 1000;
const PENDING_MAX: usize = 64;
/// A screen is read in full at least this often while it changes, to catch drift.
const VERIFY_MS: Ms = 1500;
/// Speech of program output is paced by an estimate of how long it takes to say.
const OUT_GATE_MIN_MS: Ms = 150;
const OUT_GATE_MAX_MS: Ms = 2500;
const MS_PER_CHAR: Ms = 55;
/// Lines of program output waiting for their turn, at most.
const OUT_LINES_MAX: usize = 400;
/// A single `text-changed` bigger than this is not applied to the live screen (re-read instead).
const APPLY_MAX_CHARS: u32 = 4096;
/// How long after Ctrl+C the session still treats output as the tail of an interrupted program.
const INTERRUPT_MS: Ms = 5000;
/// Lines of burst output still spoken after Ctrl+C (the `^C` and the next prompt).
const AFTER_INTERRUPT_TAIL: usize = 2;
/// A reading that replaces this many lines, and at least 70 % of the screen, is heavy; two heavy
/// readings less than `HEAVY_GAP_MS` apart are a flood.
const HEAVY_MIN_LINES: usize = 8;
const HEAVY_GAP_MS: Ms = 1000;
/// A key and the screen change it causes travel on different paths (the keyboard monitor, the
/// accessibility bus): the change can be seen first. A few characters of unexplained text wait
/// this long for their key before they are read as text nobody typed.
const LATE_KEY_MS: Ms = 60;
/// Most characters held for a late key, and how far into the typed characters a match looks
/// (older ones that never showed up on the screen are dropped).
const LATE_MAX: usize = 4;
const LATE_SCAN: usize = 4;
/// A motion key's caret event follows within a frame or two; one that has not come after this
/// long was a motion that moved nothing (Left at the start of the line).
const MOTION_MS: Ms = 250;
const MOTIONS_MAX: usize = 8;
/// A motion key that provably cannot move the caret (Left at the start of the command, Right at
/// its end) speaks the unit at the caret after this long: like every other editor (ADR-0007 CR-3).
const EDGE_MS: Ms = 100;
/// Any other motion key whose caret event has not come after this long (a slow link, or a move
/// that was lost) speaks the unit at the caret; before that the caret event may still answer it,
/// and speaking at once would read the OLD position first.
const EDGE_SLOW_MS: Ms = 240;
/// A caret event this soon after a text change belongs to it.
const EDIT_CARET_MS: Ms = 2;
/// Fetch retries before the session waits for the next event.
const RETRIES_MAX: u8 = 2;
/// A failed baseline read is retried after `BASE_RETRY_MS << attempt` (capped), this many times;
/// after that the next screen event starts one.
const BASE_RETRIES_MAX: u8 = 5;
const BASE_RETRY_MS: Ms = 200;
const BASE_RETRY_CAP_MS: Ms = 1600;
/// A history key whose redraw has not come after this long found nothing to change (the same
/// command twice, the end of the history): the line is read anyway.
const VERTICAL_MS: Ms = 250;
/// Most history keys counted while their redraws are outstanding.
const VERTICAL_MAX: u8 = 8;
/// Rows above the cursor row searched for the start of a wrapped command.
const WRAP_ROWS_MAX: usize = 32;

/// Which way a motion key moves the caret.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Dir {
    Back,
    Forward,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Phase {
    /// No screen is known (never focused, or a read failed).
    Cold,
    /// The screen is being read for the first time since focus.
    Baselining { since: Ms, speak: bool },
    /// The screen is known and nothing is pending.
    Idle,
    /// Events arrived; the screen is read when they stop.
    Settling { first: Ms, last: Ms },
    /// A read is in flight; `again` when more events arrived meanwhile.
    Fetching { since: Ms, again: bool },
}

/// What the last key leads one to expect on the screen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Expect {
    None,
    Typing,
    Erase,
    Complete,
    Enter,
    Motion(Unit),
    Vertical,
}

#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // independent flags of one state machine
pub struct Session {
    cfg: TermConfig,
    prompt: Prompt,
    phase: Phase,
    /// The screen as of the last reading, and the caret then.
    base: Screen,
    base_caret: u32,
    /// The screen with later edits applied; trusted only while `live_ok`.
    live: Screen,
    live_ok: bool,
    caret: u32,
    last_row: usize,
    last_fetch: Ms,
    retries: u8,
    /// Events arrived while the baseline was being read.
    dirty_baseline: bool,
    /// A baseline read failed and none has succeeded since: nothing is known about the screen.
    base_failed: bool,
    /// The baseline in flight was started by a screen event (not by the focus or a retry).
    base_by_event: bool,
    /// Retry of a failed baseline: when, and how many were made.
    base_retry: Option<Ms>,
    base_attempts: u8,
    /// Whether the baseline being retried speaks the line it reads (the focus presentation).
    base_speak: bool,
    pending: VecDeque<(char, Ms)>,
    /// Characters that appeared on the screen before the key that typed them was seen.
    late: VecDeque<(char, Ms)>,
    expect: Expect,
    expect_at: Ms,
    /// History keys pressed whose redraws have not all been read (two quick presses can have
    /// their redraws split across readings), and whether the newest one is still unanswered.
    vertical_keys: u8,
    vertical_open: bool,
    /// A caret event arrived that the last motion key explains and that has not been read yet
    /// (the screen was changing under it).
    caret_keyed: bool,
    /// Typed characters erased before their echo was read: when the echo turns up it is not
    /// text nobody typed.
    popped: VecDeque<(char, Ms)>,
    /// Characters BackSpace and Delete removed (already spoken by their keys) that the screen
    /// has not shown gone yet.
    erased: VecDeque<(char, Ms)>,
    /// The analysis under way only held characters for a late key (it said nothing).
    held_now: bool,
    /// When the last `text-changed` arrived: the caret event right behind one belongs to it.
    last_edit_at: Option<Ms>,
    /// Motion keys whose caret events have not been read yet, oldest first (two arrow keys can
    /// be pressed before the first one's caret event arrives).
    motions: VecDeque<(Unit, Ms, Dir)>,
    /// A password prompt is active: typed characters are secret.
    secret: bool,
    /// Ctrl+C was pressed at this time: output that is still flowing is not read; the first
    /// reading after it has gone quiet says the last lines (`^C`, the next prompt) at once.
    interrupt_at: Option<Ms>,
    /// The settle that led to the reading under way was the quiet time, not the cap.
    quiet: bool,
    /// Consecutive readings that replaced most of the screen, and when the last one was.
    heavy_streak: u8,
    heavy_at: Ms,
    outq: VecDeque<String>,
    out_gate: Ms,
    armed: Option<Ms>,
    /// Bumped whenever the screen or the caret changes.
    generation: u64,
    /// The time of the reading being analysed (speech pacing is relative to it).
    reading_at: Ms,
    buf: DiffBuf,
    counters: TermCounters,
}

impl Session {
    pub fn new(cfg: &TermConfig) -> Self {
        Self {
            prompt: Prompt::new(cfg),
            cfg: cfg.clone(),
            phase: Phase::Cold,
            base: Screen::default(),
            base_caret: 0,
            live: Screen::default(),
            live_ok: false,
            caret: 0,
            last_row: 0,
            last_fetch: 0,
            retries: 0,
            dirty_baseline: false,
            base_failed: false,
            base_by_event: false,
            base_retry: None,
            base_attempts: 0,
            base_speak: false,
            pending: VecDeque::new(),
            late: VecDeque::new(),
            expect: Expect::None,
            expect_at: 0,
            vertical_keys: 0,
            vertical_open: false,
            caret_keyed: false,
            popped: VecDeque::new(),
            erased: VecDeque::new(),
            held_now: false,
            last_edit_at: None,
            motions: VecDeque::new(),
            secret: false,
            interrupt_at: None,
            quiet: false,
            heavy_streak: 0,
            heavy_at: 0,
            outq: VecDeque::new(),
            out_gate: 0,
            armed: None,
            generation: 0,
            reading_at: 0,
            buf: DiffBuf::default(),
            counters: TermCounters::default(),
        }
    }

    /// Drop everything the session knows about the terminal, secrets included.
    fn reset(&mut self) {
        self.phase = Phase::Cold;
        self.base = Screen::default();
        self.live = Screen::default();
        self.live_ok = false;
        self.base_caret = 0;
        self.caret = 0;
        self.last_row = 0;
        self.retries = 0;
        self.dirty_baseline = false;
        self.base_failed = false;
        self.base_by_event = false;
        self.base_retry = None;
        self.base_attempts = 0;
        self.base_speak = false;
        self.pending.clear();
        self.late.clear();
        self.erased.clear();
        self.popped.clear();
        self.caret_keyed = false;
        self.motions.clear();
        self.expect = Expect::None;
        self.vertical_keys = 0;
        self.vertical_open = false;
        self.secret = false;
        self.interrupt_at = None;
        self.quiet = false;
        self.heavy_streak = 0;
        self.outq.clear();
        self.out_gate = 0;
        self.armed = None;
    }

    // ---- the entry point ---------------------------------------------------------------------

    pub fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        match input {
            TermInput::Key(k) => self.on_key(now, k, out),
            TermInput::Raw(k) => self.on_key(now, crate::keys::classify(&k), out),
            TermInput::Edit(e) => self.on_edit(now, &e, out),
            TermInput::Caret(o) => self.on_caret(now, o, out),
            TermInput::Screen { text, caret } => self.on_screen(now, &text, caret, out),
            TermInput::ScreenFailed => self.on_failed(now),
            TermInput::Tick => {
                self.armed = None;
                self.on_tick(now, out);
            }
            TermInput::Focus => {
                self.reset();
                self.base_speak = true;
                self.phase = Phase::Baselining {
                    since: now,
                    speak: true,
                };
                out.push(TermAction::FetchScreen);
            }
            TermInput::Blur => self.reset(),
        }
        self.arm(now, out);
    }

    pub fn counters(&self) -> TermCounters {
        self.counters
    }

    /// The screen as last known, for review.
    pub fn snapshot(&self) -> Option<TermSnapshot> {
        let s = if self.live_ok && !self.live.is_empty() {
            &self.live
        } else {
            &self.base
        };
        if s.is_empty() {
            return None;
        }
        Some(TermSnapshot {
            text: s.text().to_owned(),
            caret: self.caret.min(s.char_len()),
            generation: self.generation,
        })
    }

    pub fn configure(&mut self, cfg: &TermConfig) {
        self.prompt = Prompt::new(cfg);
        self.cfg = cfg.clone();
    }

    // ---- timers ------------------------------------------------------------------------------

    fn settle_due(&self, first: Ms, last: Ms) -> Ms {
        let by_quiet = last.saturating_add(Ms::from(self.cfg.settle_ms));
        let by_cap = first.saturating_add(Ms::from(self.cfg.max_wait_ms));
        by_quiet.min(by_cap)
    }

    fn deadline(&self) -> Option<Ms> {
        let phase = match self.phase {
            Phase::Cold => self.base_retry,
            Phase::Idle => None,
            Phase::Baselining { since, .. } | Phase::Fetching { since, .. } => {
                Some(since.saturating_add(FETCH_TIMEOUT_MS))
            }
            Phase::Settling { first, last } => Some(self.settle_due(first, last)),
        };
        let gate = (!self.outq.is_empty()).then_some(self.out_gate);
        let late = self
            .late
            .front()
            .map(|&(_, at)| at.saturating_add(LATE_KEY_MS));
        let vertical = (self.vertical_open
            && matches!(self.phase, Phase::Idle)
            && self.expect == Expect::Vertical)
            .then(|| self.expect_at.saturating_add(VERTICAL_MS));
        let edge = (matches!(self.phase, Phase::Idle | Phase::Settling { .. })
            && self.edge_ready())
        .then(|| {
            self.motions
                .front()
                .map(|&(_, at, dir)| at.saturating_add(self.edge_after(dir)))
        })
        .flatten();
        [phase, gate, late, vertical, edge]
            .into_iter()
            .flatten()
            .min()
    }

    fn arm(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        // One timer in flight at the earliest deadline: a later deadline (events pushing the
        // settle back) needs no new one, the tick that fires at the earlier time arms again.
        match self.deadline() {
            Some(d) => {
                if self.armed.is_none_or(|a| d < a) {
                    self.armed = Some(d);
                    out.push(TermAction::ArmTimer(d.max(now)));
                }
            }
            None => self.armed = None,
        }
    }

    fn on_tick(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        match self.phase {
            Phase::Settling { first, last } => {
                if now >= self.settle_due(first, last) {
                    self.quiet = now >= last.saturating_add(Ms::from(self.cfg.settle_ms));
                    self.settle(now, out);
                }
            }
            Phase::Fetching { since, .. } => {
                if now >= since.saturating_add(FETCH_TIMEOUT_MS) {
                    self.fetch_lost(now);
                }
            }
            Phase::Baselining { since, .. } => {
                if now >= since.saturating_add(FETCH_TIMEOUT_MS) {
                    self.baseline_lost(now);
                }
            }
            Phase::Cold => {
                if self.base_retry.is_some_and(|at| now >= at) {
                    self.base_retry = None;
                    self.begin_baseline(now, self.base_speak, false, out);
                }
            }
            Phase::Idle => {}
        }
        if !self.outq.is_empty() && now >= self.out_gate {
            self.flush_output(now, out);
        }
        self.flush_late(now, out);
        self.flush_vertical(now, out);
        self.flush_edge(now, out);
    }

    /// Is there a screen the caret can be read on?
    /// How long a motion key waits for its caret event before the timer speaks for it: the short
    /// time when the key cannot move the caret (the caret is at the start of the command for a
    /// backward motion, behind the last character for a forward one), else the long one.
    fn edge_after(&self, dir: Dir) -> Ms {
        if self.cannot_move(dir) {
            EDGE_MS
        } else {
            EDGE_SLOW_MS
        }
    }

    /// Is the caret provably at the end of the input in the direction `dir`?
    fn cannot_move(&self, dir: Dir) -> bool {
        if !self.edge_ready() {
            return false;
        }
        let (row, col) = self.live.locate(self.caret);
        let line = self.live.line(row);
        match dir {
            // Only the first row of the command, with nothing below it, has a start the caret
            // cannot go behind: a `# comment` row of an editor, or the `> out.txt` row a long
            // command wrapped onto, matches a prompt marker and is neither.
            Dir::Back => {
                self.prompt.end(line).is_some_and(|pe| col <= pe)
                    && !self.continues_command(row)
                    && self.blank_below(row)
            }
            Dir::Forward => {
                col as usize >= line.trim_end().chars().count() && self.blank_below(row)
            }
        }
    }

    /// Are all rows below `row` of the live screen blank?
    fn blank_below(&self, row: usize) -> bool {
        (row + 1..self.live.line_count()).all(|r| self.live.line(r).trim().is_empty())
    }

    /// Is `row` the continuation of a command that wrapped from a row above? The rows above are
    /// full rows (the terminal's width is the longest row on the screen) up to one with a prompt.
    fn continues_command(&self, row: usize) -> bool {
        let width = (0..self.live.line_count())
            .map(|r| self.live.line(r).chars().count())
            .max()
            .unwrap_or(0);
        let mut r = row;
        while r > 0 && row - r < WRAP_ROWS_MAX {
            let above = self.live.line(r - 1);
            if above.chars().count() != width {
                return false;
            }
            if self.prompt.end(above).is_some() {
                return true;
            }
            r -= 1;
        }
        false
    }

    fn edge_ready(&self) -> bool {
        self.live_ok && !self.live.is_empty()
    }

    /// A motion key whose caret event never came moved nothing: speak the unit at the caret.
    fn flush_edge(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        if !self.edge_ready() {
            return;
        }
        while let Some(&(unit, at, dir)) = self.motions.front() {
            if now < at.saturating_add(self.edge_after(dir)) {
                break;
            }
            self.motions.pop_front();
            // The key moved nothing: the setting decides what the boundary is.
            let b = self.cfg.boundary;
            if b.cue {
                out.push(TermAction::Cue);
            }
            if b.say {
                let key = match dir {
                    Dir::Back => "boundary-start",
                    Dir::Forward => "boundary-end",
                };
                let word = gorca_ext_api::i18n::msg(crate::NAME, key, &[]);
                self.say(out, &word, Unit::Text, Urgency::Key);
            }
            if b.repeat {
                let (row, col) = self.live.locate(self.caret);
                self.say_at(&self.live, row, col, unit, None, out);
            }
        }
    }

    /// A history key that changed nothing on the screen: read the line it left, so that a
    /// recall is never silent.
    fn flush_vertical(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        if !self.vertical_open || !matches!(self.phase, Phase::Idle) {
            return;
        }
        // The expectation ran out while the screen was being read (a slow or failing read keeps
        // the phase busy past `EXPECT_MS`): the request is stale. Dropping it also drops its
        // deadline, which would otherwise stay due and re-arm the timer for ever.
        if self.expect_valid(now) != Expect::Vertical {
            self.vertical_open = false;
            self.vertical_keys = 0;
            return;
        }
        if now < self.expect_at.saturating_add(VERTICAL_MS) {
            return;
        }
        self.vertical_open = false;
        self.vertical_keys = 0;
        self.expect = Expect::None;
        let screen = if self.live_ok && !self.live.is_empty() {
            &self.live
        } else {
            &self.base
        };
        if screen.is_empty() {
            return;
        }
        let (row, _) = screen.locate(self.caret);
        let cmd = self
            .command_at(screen, row, "")
            .unwrap_or_else(|| screen.line(row).trim_end().to_owned());
        self.counters.history_read += 1;
        self.say_command(&cmd, out);
    }

    fn say_command(&self, cmd: &str, out: &mut Vec<TermAction>) {
        if cmd.trim().is_empty() {
            self.say(out, &crate::blank(), Unit::Text, Urgency::Key);
        } else {
            self.say(out, cmd.trim_end(), Unit::Line, Urgency::Key);
        }
    }

    /// The command being edited on the logical line that ends on `row` of `screen`: the prompt
    /// is dropped and wrapped rows are joined (VTE shows each row of a wrapped line on its own
    /// line). `prefix` is the prompt text known from an earlier reading, which finds the first
    /// row better than the prompt heuristic does.
    fn command_at(&self, screen: &Screen, row: usize, prefix: &str) -> Option<String> {
        // VTE trims trailing blanks: a prompt with nothing after it lacks its last space.
        let known = prefix.trim_end();
        let lowest = row.saturating_sub(WRAP_ROWS_MAX);
        for r in (lowest..=row).rev() {
            let line = screen.line(r);
            let start = if !known.is_empty() && line.starts_with(known) {
                known.chars().count()
            } else if known.is_empty() {
                match self.prompt.end(line) {
                    Some(pe) => pe as usize,
                    None => continue,
                }
            } else {
                continue;
            };
            // Rows above the cursor row belong to the command only when each is a full row
            // (a wrapped row has the terminal's width; output and older prompts do not).
            let width = line.chars().count();
            if (r..row).any(|i| screen.line(i).chars().count() != width) {
                return None;
            }
            let mut cmd: String = line.chars().skip(start).collect();
            for i in r + 1..=row {
                cmd.push_str(screen.line(i));
            }
            // A prompt with nothing after it lost its trailing blank in the earlier reading.
            return Some(cmd.trim().to_owned());
        }
        // The remembered prompt is not on screen (a different one): ask the heuristic.
        if known.is_empty() {
            None
        } else {
            self.command_at(screen, row, "")
        }
    }

    /// The prompt text of the cursor line of `screen`, when it has one.
    fn prompt_text(&self, screen: &Screen, row: usize) -> String {
        let line = screen.line(row);
        let pe = self.prompt.end(line).unwrap_or(0);
        line.chars().take(pe as usize).collect()
    }

    /// Held characters whose key never came: text nobody typed, read as such.
    fn flush_late(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        let mut text = String::new();
        while let Some(&(c, at)) = self.late.front() {
            if now < at.saturating_add(LATE_KEY_MS) {
                break;
            }
            text.push(c);
            self.late.pop_front();
        }
        // Still on the screen? An echo that a later erase took away is nothing to read.
        let (row, _) = self.live.locate(self.caret);
        let still_there = self.live.line(row).trim_end().ends_with(text.trim());
        if !text.trim().is_empty() && still_there {
            self.say(out, text.trim(), Unit::Text, Urgency::Output);
        }
    }

    /// A read failed or never came back: try again twice, then wait for the next event.
    fn fetch_lost(&mut self, now: Ms) {
        self.counters.fetch_failed += 1;
        self.live_ok = false;
        if self.retries < RETRIES_MAX {
            self.retries += 1;
            self.phase = Phase::Settling {
                first: now,
                last: now,
            };
        } else {
            self.retries = 0;
            self.phase = Phase::Idle;
        }
    }

    fn on_failed(&mut self, now: Ms) {
        match self.phase {
            Phase::Fetching { .. } => self.fetch_lost(now),
            Phase::Baselining { .. } => self.baseline_lost(now),
            Phase::Cold | Phase::Idle | Phase::Settling { .. } => {}
        }
    }

    /// The baseline read failed or never came back: nothing is known about the screen. Read it
    /// again after a growing pause (a few times); until one succeeds, whatever the screen shows
    /// is not a baseline, see `on_screen`.
    fn baseline_lost(&mut self, now: Ms) {
        self.counters.fetch_failed += 1;
        self.phase = Phase::Cold;
        self.base_failed = true;
        self.base_retry = (self.base_attempts < BASE_RETRIES_MAX).then(|| {
            let wait = (BASE_RETRY_MS << self.base_attempts).min(BASE_RETRY_CAP_MS);
            self.base_attempts += 1;
            now.saturating_add(wait)
        });
    }

    // ---- keys --------------------------------------------------------------------------------

    fn expect_valid(&self, now: Ms) -> Expect {
        if now.saturating_sub(self.expect_at) <= EXPECT_MS {
            self.expect
        } else {
            Expect::None
        }
    }

    fn say(&self, out: &mut Vec<TermAction>, text: &str, unit: Unit, urgency: Urgency) {
        out.push(TermAction::Say {
            text: text.to_owned(),
            unit,
            urgency,
            sensitive: self.secret,
        });
    }

    fn on_key(&mut self, now: Ms, key: TermKey, out: &mut Vec<TermAction>) {
        let prev_expect = self.expect_valid(now);
        self.expect_at = now;
        // Any key press cancels what is queued in the speech lane (a newer epoch): output waiting
        // for its turn is as stale as that, and the pacing that protects the lane starts over.
        self.counters.output_lines_skipped += self.outq.len() as u64;
        self.outq.clear();
        self.out_gate = 0;
        if !matches!(key, TermKey::LineUp | TermKey::LineDown) {
            self.vertical_keys = 0;
            self.vertical_open = false;
        }
        match key {
            TermKey::Typed(c) => {
                out.push(TermAction::ClaimKey);
                if !self.secret {
                    self.check_secret_live();
                }
                self.say(out, c.encode_utf8(&mut [0; 4]), Unit::Char, Urgency::Key);
                // Its echo may have been seen already (see `LATE_KEY_MS`).
                let seen = self
                    .late
                    .iter()
                    .take(LATE_SCAN)
                    .position(|&(l, _)| l == c)
                    .inspect(|&pos| {
                        self.late.drain(..=pos);
                    })
                    .is_some();
                if seen {
                    self.counters.typed_explained += 1;
                } else if !self.secret {
                    if self.pending.len() >= PENDING_MAX {
                        self.pending.pop_front();
                    }
                    self.pending.push_back((c, now));
                }
                self.counters.typed_echoed += 1;
                self.expect = Expect::Typing;
            }
            TermKey::Enter => {
                out.push(TermAction::ClaimKey);
                // The typed characters stay: their echo may not have been read yet. They expire.
                self.secret = false;
                self.expect = Expect::Enter;
            }
            TermKey::Backspace => {
                out.push(TermAction::ClaimKey);
                self.expect = Expect::Erase;
                if self.secret {
                    return;
                }
                // A character typed a moment ago whose echo has not arrived: that one goes.
                if let Some((c, _)) = self.pending.pop_back() {
                    self.say(out, c.encode_utf8(&mut [0; 4]), Unit::Char, Urgency::Key);
                    if self.popped.len() >= PENDING_MAX {
                        self.popped.pop_front();
                    }
                    self.popped.push_back((c, now));
                    return;
                }
                if self.live.is_empty() {
                    return;
                }
                let (row, col) = self.live.locate(self.caret);
                let line = self.live.line(row);
                let pe = self.prompt.end(line).unwrap_or(0);
                if col > pe
                    && let Some(c) = line.chars().nth((col - 1) as usize)
                {
                    self.say(out, c.encode_utf8(&mut [0; 4]), Unit::Char, Urgency::Key);
                    self.note_erased(now, c);
                }
            }
            TermKey::Delete => {
                out.push(TermAction::ClaimKey);
                self.expect = Expect::Erase;
                if self.secret || self.live.is_empty() {
                    return;
                }
                let (row, col) = self.live.locate(self.caret);
                if let Some(c) = self.live.line(row).chars().nth(col as usize) {
                    self.say(out, c.encode_utf8(&mut [0; 4]), Unit::Char, Urgency::Key);
                    self.note_erased(now, c);
                }
            }
            TermKey::Tab => {
                out.push(TermAction::ClaimKey);
                self.expect = Expect::Complete;
            }
            TermKey::CharLeft | TermKey::CharRight | TermKey::LineStart | TermKey::LineEnd => {
                out.push(TermAction::ClaimKey);
                self.expect = Expect::Motion(Unit::Char);
                let dir = if matches!(key, TermKey::CharLeft | TermKey::LineStart) {
                    Dir::Back
                } else {
                    Dir::Forward
                };
                self.push_motion(now, Unit::Char, dir);
            }
            TermKey::WordLeft | TermKey::WordRight => {
                out.push(TermAction::ClaimKey);
                self.expect = Expect::Motion(Unit::Word);
                let dir = if matches!(key, TermKey::WordLeft) {
                    Dir::Back
                } else {
                    Dir::Forward
                };
                self.push_motion(now, Unit::Word, dir);
            }
            TermKey::LineUp | TermKey::LineDown => {
                out.push(TermAction::ClaimKey);
                if prev_expect != Expect::Vertical {
                    self.vertical_keys = 0;
                }
                self.vertical_keys = self.vertical_keys.saturating_add(1).min(VERTICAL_MAX);
                self.vertical_open = true;
                self.expect = Expect::Vertical;
            }
            TermKey::Interrupt => {
                out.push(TermAction::ClaimKey);
                self.pending.clear();
                self.outq.clear();
                self.secret = false;
                self.interrupt_at = Some(now);
                self.out_gate = 0;
                self.expect = Expect::None;
            }
            TermKey::Other => {
                self.expect = Expect::None;
            }
        }
    }

    /// Is the cursor line of the screen last read a password prompt? (A character typed before
    /// the settle still finds out.)
    fn check_secret_live(&mut self) {
        if self.live.is_empty() {
            return;
        }
        let (row, col) = self.live.locate(self.caret);
        if self.secret_at(&self.live, row, col) {
            self.secret = true;
        }
    }

    fn secret_at(&self, screen: &Screen, row: usize, col: u32) -> bool {
        let line = screen.line(row);
        let len = u32::try_from(line.trim_end().chars().count()).unwrap_or(u32::MAX);
        if col < len {
            return false;
        }
        self.prompt.is_secret_prompt(line)
    }

    // ---- events ------------------------------------------------------------------------------

    /// A screen event found the session cold: read the screen.
    fn start_baseline(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        self.base_retry = None;
        self.begin_baseline(now, false, true, out);
    }

    fn begin_baseline(&mut self, now: Ms, speak: bool, by_event: bool, out: &mut Vec<TermAction>) {
        self.phase = Phase::Baselining { since: now, speak };
        self.dirty_baseline = false;
        self.base_by_event = by_event;
        out.push(TermAction::FetchScreen);
    }

    fn on_edit(&mut self, now: Ms, e: &TermEdit, out: &mut Vec<TermAction>) {
        self.last_edit_at = Some(now);
        match self.phase {
            Phase::Cold => {
                self.start_baseline(now, out);
                return;
            }
            Phase::Baselining { .. } => {
                self.dirty_baseline = true;
                return;
            }
            Phase::Idle | Phase::Settling { .. } | Phase::Fetching { .. } => {}
        }
        self.apply(e);
        self.generation += 1;
        self.phase = match self.phase {
            Phase::Idle => Phase::Settling {
                first: now,
                last: now,
            },
            Phase::Settling { first, .. } => Phase::Settling { first, last: now },
            Phase::Fetching { since, .. } => Phase::Fetching { since, again: true },
            Phase::Cold | Phase::Baselining { .. } => self.phase,
        };
    }

    /// Apply one edit to the live screen, or mark the live screen untrusted.
    fn apply(&mut self, e: &TermEdit) {
        if !self.live_ok {
            return;
        }
        let ok = match e {
            TermEdit::Insert {
                offset,
                len,
                text: Some(t),
            } => {
                *len <= APPLY_MAX_CHARS
                    && t.chars().count() == *len as usize
                    && self.live.apply_insert(*offset, t)
            }
            TermEdit::Delete { offset, len } => {
                *len <= APPLY_MAX_CHARS && self.live.apply_delete(*offset, *len)
            }
            TermEdit::Insert { text: None, .. } | TermEdit::Replaced => false,
        };
        if !ok {
            self.live_ok = false;
        }
    }

    fn on_caret(&mut self, now: Ms, off: u32, out: &mut Vec<TermAction>) {
        let moved = off != self.caret;
        let prev = self.caret;
        self.caret = off;
        // A caret event in the same frame as a text change is that change's (VTE sends the text
        // first): it is not an answer to a motion key.
        let with_edit = self
            .last_edit_at
            .is_some_and(|at| now.saturating_sub(at) <= EDIT_CARET_MS);
        if moved && with_edit {
            self.generation += 1;
        } else if moved {
            self.generation += 1;
            self.prune_motions(now);
            if !self.motions.is_empty() || self.expect_valid(now) == Expect::Vertical {
                self.caret_keyed = true;
            }
        }
        match self.phase {
            Phase::Cold => {
                self.start_baseline(now, out);
                return;
            }
            Phase::Baselining { .. } => {
                self.dirty_baseline = true;
                return;
            }
            Phase::Fetching { .. } => return,
            // Edits applied to the live screen so far are all there is to know: a motion key is
            // answered now, not after the settle (the screen cannot change under a caret move).
            Phase::Settling { .. } if !self.live_ok => return,
            Phase::Settling { .. } | Phase::Idle => {}
        }
        // Two motion keys whose caret events the ingress folded into one (last writer wins):
        // the caret came back to where it was. The last key's answer is the character there.
        self.prune_motions(now);
        let folded = !moved && !with_edit && self.motions.len() >= 2;
        if (!moved && !folded) || with_edit || !self.live_ok || self.live.is_empty() {
            return;
        }
        if folded {
            while self.motions.len() > 1 {
                self.motions.pop_front();
            }
        }
        let (row, col) = self.live.locate(off);
        if let Some((unit, _, _)) = self.motions.pop_front() {
            self.caret_keyed = !self.motions.is_empty();
            let (prow, pcol) = self.live.locate(prev);
            self.say_at(
                &self.live,
                row,
                col,
                unit,
                (prow == row).then_some(pcol),
                out,
            );
            self.last_row = row;
            return;
        }
        match self.expect_valid(now) {
            Expect::Vertical => {
                if row != self.last_row {
                    self.expect = Expect::None;
                    self.vertical_open = false;
                    self.caret_keyed = false;
                    self.say_line(&self.live, row, out);
                }
            }
            Expect::Motion(_)
            | Expect::None
            | Expect::Typing
            | Expect::Erase
            | Expect::Complete
            | Expect::Enter => {}
        }
        self.last_row = row;
    }

    // ---- reading the screen ------------------------------------------------------------------

    fn settle(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        let verify_due = now.saturating_sub(self.last_fetch) >= VERIFY_MS;
        if self.live_ok && !verify_due && !self.live.is_empty() {
            self.counters.settled_local += 1;
            self.phase = Phase::Idle;
            let new = self.live.clone();
            self.analyze(now, new, out);
        } else {
            self.phase = Phase::Fetching {
                since: now,
                again: false,
            };
            out.push(TermAction::FetchScreen);
        }
    }

    fn on_screen(&mut self, now: Ms, text: &str, caret: Option<u32>, out: &mut Vec<TermAction>) {
        let new = Screen::new(text);
        self.generation += 1;
        match self.phase {
            Phase::Baselining { speak, .. } => {
                self.counters.fetches += 1;
                self.retries = 0;
                self.last_fetch = now;
                if let Some(c) = caret {
                    self.caret = c;
                }
                self.base_caret = self.caret;
                let (row, col) = new.locate(self.caret);
                self.last_row = row;
                if self.secret_at(&new, row, col) {
                    self.secret = true;
                    self.counters.secret_prompts += 1;
                }
                self.live = new.clone();
                self.live_ok = true;
                // Output that came while no baseline was known cannot be told from what was
                // there before: say what is above the prompt rather than swallow it.
                let orphan = self.base_failed && (self.base_by_event || self.dirty_baseline);
                if orphan {
                    let lines = self.tail_above(&new, row);
                    self.dirty_baseline = false;
                    self.emit_output(now, lines, out);
                } else if speak && !new.is_empty() {
                    self.say_line(&new, row, out);
                }
                self.base_failed = false;
                self.base_attempts = 0;
                self.base_retry = None;
                self.base = new;
                self.phase = if self.dirty_baseline {
                    self.dirty_baseline = false;
                    Phase::Settling {
                        first: now,
                        last: now,
                    }
                } else {
                    Phase::Idle
                };
            }
            Phase::Fetching { again, .. } => {
                self.counters.fetches += 1;
                self.retries = 0;
                self.last_fetch = now;
                if self.live_ok && self.live != new {
                    self.counters.resync_mismatch += 1;
                }
                if let Some(c) = caret {
                    self.caret = c;
                }
                self.live = new.clone();
                self.live_ok = true;
                self.phase = if again {
                    Phase::Settling {
                        first: now,
                        last: now,
                    }
                } else {
                    Phase::Idle
                };
                // More events arrived while the screen was being read: it is not over yet.
                self.quiet = self.quiet && !again;
                self.analyze(now, new, out);
            }
            // A reply nobody waits for (the terminal lost focus, or the read timed out).
            Phase::Cold | Phase::Idle | Phase::Settling { .. } => {}
        }
    }

    fn analyze(&mut self, now: Ms, new: Screen, out: &mut Vec<TermAction>) {
        self.reading_at = now;
        self.held_now = false;
        let (cl, col) = new.locate(self.caret);
        let secret_now = self.secret_at(&new, cl, col);
        if secret_now && !self.secret {
            self.counters.secret_prompts += 1;
        }
        self.secret = secret_now;
        let mut df = diff(&self.base, &new, &mut self.buf);
        // Output above a cursor line that looks unchanged: the prompt is a new one, printed
        // after the output (the old prompt scrolled up, or is the line it still was above).
        if cl + 1 == new.line_count()
            && df.old_of.get(cl).copied().flatten().is_some()
            && (df.inserted.iter().any(|&u| u < cl)
                || df.changed.iter().filter(|&&u| u < cl).count() >= 2)
        {
            df = diff_with(&self.base, &new, Some(cl), &mut self.buf);
        }
        self.pending
            .retain(|&(_, at)| now.saturating_sub(at) <= PENDING_MS);
        self.popped
            .retain(|&(_, at)| now.saturating_sub(at) <= PENDING_MS);
        let exp = self.expect_valid(now);
        let editing = !self.pending.is_empty()
            || matches!(
                exp,
                Expect::Typing
                    | Expect::Erase
                    | Expect::Complete
                    | Expect::Vertical
                    | Expect::Motion(_)
            );
        let unmatched = df.unmatched();
        // The screen changed under a history key (a recall, or a pager scrolling): the change is
        // its answer, whatever it said, and the key has nothing left to read on its own.
        if !unmatched.is_empty() {
            self.vertical_open = false;
        }
        let mut spoke = false;
        // After Ctrl+C the output may still be flowing: those screens are not read. The first
        // one that has gone quiet is (see `emit_output`), and so are five seconds later on.
        let after_interrupt = self
            .interrupt_at
            .is_some_and(|at| now.saturating_sub(at) <= INTERRUPT_MS);
        if !after_interrupt {
            self.interrupt_at = None;
        }
        if after_interrupt && !self.quiet {
            // Not read, and not taken as the screen to compare with either: the first quiet
            // reading must still see everything that happened since the interrupt.
            self.counters.output_lines_skipped += unmatched.len() as u64;
            self.last_row = cl;
            return;
        } else if !unmatched.is_empty()
            && exp == Expect::Vertical
            && let Some(cmd) = self.recalled(&new, cl, &df)
        {
            spoke = true;
            self.history_done();
            self.say_command(&cmd, out);
        } else if !unmatched.is_empty() {
            if Self::is_line_edit(&df, &unmatched, cl, editing) {
                spoke = self.line_edit(&new, cl, &df, &unmatched, exp, out);
            } else if is_repaint(&df, &unmatched) {
                self.counters.repaints += 1;
                self.counters.output_lines_skipped += unmatched.len() as u64;
            } else {
                let heavy = unmatched.len() >= HEAVY_MIN_LINES
                    && unmatched.len() * 10 >= new.line_count() * 7;
                if heavy && now.saturating_sub(self.heavy_at) <= HEAVY_GAP_MS {
                    self.heavy_streak = self.heavy_streak.saturating_add(1);
                } else {
                    self.heavy_streak = u8::from(heavy);
                }
                if heavy {
                    self.heavy_at = now;
                }
                let lines: Vec<String> = unmatched
                    .iter()
                    .map(|&i| new.line(i).trim().to_owned())
                    .collect();
                spoke = self.emit_output(now, lines, out);
            }
        }
        // A motion key whose caret event came while the screen was still changing: read where
        // the caret is now. (One whose event has not come yet is read when it does.)
        if (!spoke || self.held_now) && self.caret_keyed {
            self.prune_motions(now);
            if let Some((unit, _, _)) = self.motions.pop_front() {
                self.say_at(&new, cl, col, unit, None, out);
                self.expect = Expect::None;
            }
            match exp {
                Expect::Vertical if cl != self.last_row => {
                    self.say_line(&new, cl, out);
                    self.expect = Expect::None;
                    self.vertical_open = false;
                }
                Expect::Vertical
                | Expect::Motion(_)
                | Expect::None
                | Expect::Typing
                | Expect::Erase
                | Expect::Complete
                | Expect::Enter => {}
            }
            self.caret_keyed = false;
        }
        self.last_row = cl;
        self.base = new;
        self.base_caret = self.caret;
    }

    /// A history key's redraw has been read: one fewer is outstanding. When another is still
    /// to come (two quick presses), its redraw is recognised too.
    fn history_done(&mut self) {
        self.counters.history_read += 1;
        self.vertical_open = false;
        self.vertical_keys = self.vertical_keys.saturating_sub(1);
        if self.vertical_keys == 0 {
            self.expect = Expect::None;
        }
    }

    /// The command now on the cursor line when a history key explains the change and the
    /// command is not the one the last reading showed. Works for commands that wrap onto
    /// several rows and for screens that scrolled under them.
    fn recalled(&self, new: &Screen, cl: usize, df: &Diff) -> Option<String> {
        if df.bulk {
            return None;
        }
        let (brow, _) = self.base.locate(self.base_caret);
        let prefix = self.prompt_text(&self.base, brow);
        let old = self.command_at(&self.base, brow, &prefix)?;
        let cmd = self.command_at(new, cl, &prefix)?;
        (cmd != old).then_some(cmd)
    }

    /// Is the change confined to the cursor line (and the wrapped rows above it) while a key
    /// explains it, or a single in-place edit of the cursor line?
    fn is_line_edit(df: &Diff, unmatched: &[usize], cl: usize, editing: bool) -> bool {
        if df.bulk {
            return false;
        }
        let near = unmatched.iter().all(|&u| u <= cl && u + 2 >= cl);
        if !near {
            return false;
        }
        editing || (unmatched.len() == 1 && df.changed.contains(&cl))
    }

    /// The cursor line changed while editing: what the key explains stays silent, the rest is read.
    fn line_edit(
        &mut self,
        new: &Screen,
        cl: usize,
        df: &Diff,
        unmatched: &[usize],
        exp: Expect,
        out: &mut Vec<TermAction>,
    ) -> bool {
        let first = unmatched.first().copied().unwrap_or(cl);
        // The rows of the new screen that changed, and the old rows they replace.
        let new_text = join_rows(new, first..=cl);
        let mut old_rows: Vec<usize> = (first..=cl)
            .filter_map(|i| df.old_of.get(i).copied().flatten())
            .collect();
        old_rows.dedup();
        let old_text = old_rows
            .iter()
            .map(|&o| self.base.line(o).trim_end())
            .collect::<Vec<_>>()
            .join("\n");
        if old_text == new_text {
            return false;
        }
        // History recall: the command now on the line, without the prompt.
        if exp == Expect::Vertical && first == cl {
            let line = new.line(cl);
            let pe = self.prompt.end(line).unwrap_or(0);
            let cmd: String = line.chars().skip(pe as usize).collect();
            self.history_done();
            self.say_command(&cmd, out);
            return true;
        }
        let (ins, del) = edit_between(&old_text, &new_text);
        let mut ins: Vec<char> = ins.chars().collect();
        // What the typed characters explain. VTE trims trailing blanks and joins wrapped rows
        // with a newline, so a blank or a newline in front of an echoed character is not text.
        let mut k = 0usize;
        while k < ins.len() {
            let c = ins[k];
            if let Some(pos) = self.popped.iter().position(|&(p, _)| p == c) {
                // Typed and erased before its echo was read: neither the echo nor the erase is news.
                self.popped.remove(pos);
                k += 1;
            } else if let Some(pos) = self
                .pending
                .iter()
                .take(LATE_SCAN)
                .position(|&(p, _)| p == c)
            {
                // Typed characters in front of it never showed up (an echo that changed
                // nothing, a key whose text went elsewhere): they are dropped, not waited for.
                self.pending.drain(..=pos);
                self.counters.typed_explained += 1;
                k += 1;
            } else if c.is_whitespace() {
                k += 1;
            } else {
                break;
            }
        }
        ins.drain(..k);
        let caused = if matches!(exp, Expect::None | Expect::Enter) {
            Urgency::Output
        } else {
            Urgency::Key
        };
        if !ins.is_empty() {
            let text: String = ins.iter().collect();
            if text.trim().is_empty() {
                return false;
            }
            if text.contains('\n') {
                // New rows, not an edit of the line: candidates of a completion, a message.
                let rows: Vec<String> = text.lines().map(|l| l.trim().to_owned()).collect();
                return self.emit_output(self.reading_at, rows, out);
            }
            // A few characters nobody has typed yet: their key may be on its way.
            let holdable = !matches!(exp, Expect::Complete | Expect::Enter | Expect::Vertical)
                && ins.len() <= LATE_MAX
                && !self.secret;
            if holdable {
                self.held_now = true;
                for &c in &ins {
                    if self.late.len() >= PENDING_MAX {
                        self.late.pop_front();
                    }
                    self.late.push_back((c, self.reading_at));
                }
                return true;
            }
            // Tab completion: the word that was completed, not the characters that were added.
            let completed = if exp == Expect::Complete {
                let (_, col) = new.locate(self.caret);
                word_at(new.line(cl), col.saturating_sub(1))
            } else {
                None
            };
            match completed {
                Some(w) => self.say(out, &w, Unit::Word, caused),
                None => self.say(out, text.trim(), Unit::Text, caused),
            }
            return true;
        }
        // Blanks do not count: a trimmed trailing blank is not a deleted character.
        let mut del: Vec<char> = del.chars().filter(|c| *c != '\n').collect();
        // What BackSpace and Delete removed was said by their keys.
        self.erased
            .retain(|&(_, at)| self.reading_at.saturating_sub(at) <= PENDING_MS);
        while let Some((c, _)) = self.erased.pop_front() {
            if let Some(p) = del.iter().rposition(|d| *d == c) {
                del.remove(p);
            }
        }
        let del: String = del.into_iter().collect();
        if exp != Expect::Erase
            && del.chars().filter(|c| !c.is_whitespace()).count() >= 2
            && !self.secret
        {
            self.say(out, del.trim(), Unit::Text, caused);
            return true;
        }
        false
    }

    fn note_erased(&mut self, now: Ms, c: char) {
        if self.erased.len() >= PENDING_MAX {
            self.erased.pop_front();
        }
        self.erased.push_back((c, now));
    }

    fn push_motion(&mut self, now: Ms, unit: Unit, dir: Dir) {
        self.prune_motions(now);
        if self.motions.len() >= MOTIONS_MAX {
            self.motions.pop_front();
        }
        self.motions.push_back((unit, now, dir));
    }

    /// How coarse a word is: the user's choice, else the terminal default (medium).
    fn word_level(&self) -> WordLevel {
        self.cfg.word_level.unwrap_or(DEFAULT_WORD_LEVEL)
    }

    fn prune_motions(&mut self, now: Ms) {
        while self
            .motions
            .front()
            .is_some_and(|&(_, at, _)| now.saturating_sub(at) > MOTION_MS)
        {
            self.motions.pop_front();
        }
    }

    /// Speak what a motion key moved the caret over, with the presenter every editor shares:
    /// a character is the one at the caret, a word is the word just passed or the one at the
    /// caret (`from` is the column before the move, when it was on this row).
    fn say_at(
        &self,
        s: &Screen,
        row: usize,
        col: u32,
        unit: Unit,
        from: Option<u32>,
        out: &mut Vec<TermAction>,
    ) {
        let line = s.line(row);
        let text = match unit {
            Unit::Word => {
                let chars: Vec<char> = line.trim_end().chars().collect();
                word_motion(&chars, from, col, self.word_level())
                    .map(|r| chars[r].iter().collect::<String>().trim().to_owned())
                    .filter(|w| !w.is_empty())
            }
            Unit::Line | Unit::Text => word_at(line, col),
            Unit::Char => line.chars().nth(col as usize).map(String::from),
        };
        match (text, unit) {
            (Some(t), Unit::Char) => self.say(out, &t, Unit::Char, Urgency::Key),
            (Some(t), _) => self.say(out, &t, Unit::Word, Urgency::Key),
            (None, _) => self.say(out, &crate::blank(), Unit::Text, Urgency::Key),
        }
    }

    /// The last non-blank lines above `row`, at most `burst_lines`, oldest first.
    fn tail_above(&self, s: &Screen, row: usize) -> Vec<String> {
        let mut v: Vec<String> = (0..row)
            .rev()
            .map(|i| s.line(i).trim_end().to_owned())
            .filter(|l| !l.trim().is_empty())
            .take(self.cfg.burst_lines as usize)
            .collect();
        v.reverse();
        v
    }

    fn say_line(&self, s: &Screen, row: usize, out: &mut Vec<TermAction>) {
        let line = s.line(row).trim_end();
        if line.trim().is_empty() {
            self.say(out, &crate::blank(), Unit::Text, Urgency::Queued);
        } else {
            self.say(out, line, Unit::Line, Urgency::Queued);
        }
    }

    // ---- program output ----------------------------------------------------------------------

    /// Queue `lines` of program output and speak what the pacing allows. True when something
    /// was queued.
    fn emit_output(&mut self, now: Ms, lines: Vec<String>, out: &mut Vec<TermAction>) -> bool {
        let mut lines: Vec<String> = lines.into_iter().filter(|l| !l.is_empty()).collect();
        let total = lines.len();
        if total == 0 {
            return false;
        }
        let keep = if self.interrupt_at.take().is_some() {
            self.outq.clear();
            self.out_gate = 0;
            AFTER_INTERRUPT_TAIL.min(total)
        } else if total > self.cfg.burst_lines as usize || self.heavy_streak >= 2 {
            // A flood: the newest lines are the only ones worth saying; the queue is stale.
            self.counters.output_lines_skipped += self.outq.len() as u64;
            self.outq.clear();
            (self.cfg.burst_tail as usize).max(1).min(total)
        } else {
            total
        };
        let skipped = total - keep;
        lines.drain(..skipped);
        self.counters.output_lines_skipped += skipped as u64;
        self.outq.extend(lines);
        let cap = self.cfg.max_output_chars as usize;
        // Older lines give way to newer ones when the speech cannot keep up.
        let mut chars: usize = self.outq.iter().map(|l| l.chars().count() + 1).sum();
        while self.outq.len() > OUT_LINES_MAX
            || (chars > cap.saturating_mul(2) && self.outq.len() > 1)
        {
            if let Some(l) = self.outq.pop_front() {
                chars = chars.saturating_sub(l.chars().count() + 1);
                self.counters.output_lines_skipped += 1;
            }
        }
        if now >= self.out_gate {
            self.flush_output(now, out);
        }
        true
    }

    /// Speak queued output from the front, up to the utterance cap.
    fn flush_output(&mut self, now: Ms, out: &mut Vec<TermAction>) {
        let cap = (self.cfg.max_output_chars as usize).max(16);
        let mut text = String::new();
        let mut taken = 0u64;
        while let Some(front) = self.outq.front() {
            let n = front.chars().count();
            let have = text.chars().count();
            if taken > 0 && have + 1 + n > cap {
                break;
            }
            let Some(line) = self.outq.pop_front() else {
                break;
            };
            if taken > 0 {
                text.push('\n');
            }
            if n > cap {
                text.extend(line.chars().take(cap));
            } else {
                text.push_str(&line);
            }
            taken += 1;
        }
        if text.is_empty() {
            return;
        }
        let chars = text.chars().count() as u64;
        self.counters.output_spoken += 1;
        self.counters.output_lines_spoken += taken;
        self.out_gate =
            now.saturating_add((chars * MS_PER_CHAR).clamp(OUT_GATE_MIN_MS, OUT_GATE_MAX_MS));
        out.push(TermAction::Say {
            text,
            unit: Unit::Text,
            urgency: Urgency::Output,
            sensitive: false,
        });
    }
}

impl TerminalHooks for Session {
    fn configure(&mut self, cfg: &TermConfig) {
        Session::configure(self, cfg);
    }

    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        Session::step(self, now, input, out);
    }

    fn counters(&self) -> TermCounters {
        Session::counters(self)
    }

    fn snapshot(&self) -> Option<TermSnapshot> {
        Session::snapshot(self)
    }
}

/// A program redrawing its screen (htop, mc, top): the changed lines are scattered, in at least
/// three separate runs of rows, around lines that stayed. One block of new lines (a scroll, a
/// page of `less`, a listing, a progress line with output below it) is output, not a repaint.
fn is_repaint(df: &Diff, unmatched: &[usize]) -> bool {
    if df.bulk || unmatched.len() < 3 {
        return false;
    }
    let runs = 1 + unmatched.windows(2).filter(|w| w[1] != w[0] + 1).count();
    runs >= 3
}

/// Rows `r` of `s` with trailing blanks trimmed, joined by newlines.
fn join_rows(s: &Screen, r: std::ops::RangeInclusive<usize>) -> String {
    let mut t = String::new();
    for i in r.clone() {
        if i > *r.start() {
            t.push('\n');
        }
        t.push_str(s.line(i).trim_end());
    }
    t
}

/// What was inserted and what was deleted between `old` and `new`: the common head and tail go.
fn edit_between(old: &str, new: &str) -> (String, String) {
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    let m = o.len().min(n.len());
    let mut head = 0usize;
    while head < m && o[head] == n[head] {
        head += 1;
    }
    let mut tl = 0usize;
    while tl < m - head && o[o.len() - 1 - tl] == n[n.len() - 1 - tl] {
        tl += 1;
    }
    (
        n[head..n.len() - tl].iter().collect(),
        o[head..o.len() - tl].iter().collect(),
    )
}

#[cfg(test)]
mod tests;
