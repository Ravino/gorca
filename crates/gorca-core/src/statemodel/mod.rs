// SPDX-License-Identifier: GPL-3.0-or-later
//! Model-based (property) tests of the Reader's mode and state transitions.
//!
//! The Reader is driven in-process through `Reader::handle`, the way the lanes drive it, against
//! a simulated desktop (`world`): focus changes (duplicates, a stale event ahead of the real
//! one, removed nodes, dialogs closing), window activations, review and object navigation
//! commands, typing into GTK-like entries and a terminal, terminal redraws and history recall,
//! notifications, failing and held replies, lost text events. After every step the invariants
//! below are checked, and a failing sequence is shrunk to a minimal one.
//!
//! * `echo-missing`   typing a printable character into a settled, focused editable (entry, text
//!   area, terminal) is echoed: an interrupting utterance, the character itself for a letter.
//! * `review-kept`    a printable character or Backspace typed into the focused editable leaves
//!   review (the user is typing into the object, not reviewing it), and so does a
//!   review exit command.
//! * `not-tracked`    after any key (and after any focus) the focused node is tracked by the
//!   layer that owns it: the text layer for an entry, the terminal arm for a terminal; a key
//!   repairs a lost tracking, a focus event is not needed (stuck-state class).
//! * `key-name`       key names ("enter", "tab", "backspace") are spoken only when
//!   `echo.command_keys` is on.
//! * `focus-lost`     a delivered focus event ends with the Reader focused and presented on that
//!   node, and focus leaves review.
//! * `caret-silent`  Left, Right, Home and End into a settled entry or text view that the text layer
//!   tracks are answered (a key that moves nothing speaks the unit at the caret), and
//!   `caret-unit` by the character the caret is on, also when the widgets behave like GTK4's
//!   (`Op::Gtk4`: no caret event for typed text, a selection event behind every move).
//! * `review-drift`  a terminal review word or character move whose line is still on the screen
//!   stays on that line unless it runs past the line's end (ADR-0018 I1/I4).
//! * `review-reset`  a word or character move inside a terminal line lands on the next unit of
//!   that line, from the offset the cursor had, not from the line start (I2).
//! * `review-wait`   a terminal review move with a held screen speaks within the step: it never
//!   suspends on a screen read (I5).
//! * `epoch`          the epoch never goes back and no utterance carries a future epoch.
//!
//! Runs: `GORCA_STATE_CASES` sequences (default 500; CI), `GORCA_STATE_SEED`, `GORCA_STATE_LEN`
//! (steps per sequence, default 40). The long run: `GORCA_STATE_CASES=50000 cargo test --release
//! -p gorca-core statemodel::random_sequences`.
#![allow(
    clippy::wildcard_enum_match_arm,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    clippy::missing_panics_doc,
    clippy::cast_possible_truncation,
    clippy::struct_excessive_bools,
    clippy::collapsible_match,
    clippy::extend_with_drain,
    clippy::unwrap_or_default
)]

mod regressions;
mod world;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::Ordering;

use gorca_input_api::Modifiers;
use gorca_model::{Epoch, Prio};

use crate::commands::CoreCmd;
use crate::focus::FocusState;
use crate::mode::Mode;
use world::{AREA, BUTTON, DLG, DLG_BTN, DLG_ENTRY, ENTRY, ExtState, Sim, TERM, WIN, key};

/// One thing the user, an application or the desktop does.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum KeyKind {
    Letter(char),
    Space,
    Punct(char),
    Backspace,
    Enter,
    Tab,
    Left,
    Right,
    Home,
    End,
    Up,
    Down,
}

impl KeyKind {
    fn sym(self) -> (u32, u32) {
        match self {
            Self::Letter(c) | Self::Punct(c) => (c as u32, c as u32),
            Self::Space => (0x20, 0x20),
            Self::Backspace => (0xff08, 8),
            Self::Enter => (0xff0d, 13),
            Self::Tab => (0xff09, 9),
            Self::Left => (0xff51, 0),
            Self::Up => (0xff52, 0),
            Self::Right => (0xff53, 0),
            Self::Down => (0xff54, 0),
            Self::Home => (0xff50, 0),
            Self::End => (0xff57, 0),
        }
    }
    fn printable(self) -> bool {
        matches!(self, Self::Letter(_) | Self::Space | Self::Punct(_))
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum Op {
    /// The application focuses `node`; `prelude` is a late event for another node delivered
    /// first, `dups` repeats of the real one follow.
    Focus {
        node: u64,
        prelude: Option<u64>,
        dups: u8,
    },
    Remove(u64),
    OpenDialog,
    CloseDialog {
        refocus: bool,
        to: u64,
    },
    Activate(u64),
    Type(KeyKind, Modifiers),
    Cmd(CoreCmd, u8),
    Tick(u64),
    TermAsync,
    /// Streaming output scrolls the terminal screen by one row.
    TermScroll,
    Notify,
    ExtSpeech(bool),
    LoseEvents(bool),
    /// The text widgets behave like GTK4's (see `Sim::gtk4`).
    Gtk4(bool),
    FailFetches(bool),
    HoldReplies(bool),
    /// The extension slot changes state (busy, faulted, recovered, absent).
    Ext(ExtState),
    /// A removed node comes back.
    Revive(u64),
    /// Only as the first step: the terminal policy comes from the extension host alone (no
    /// native provider), starting healthy.
    WasmDeployment,
    Hung,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Step {
    /// Milliseconds since the previous step.
    pub dt: u64,
    pub op: Op,
}

/// What the harness recorded of one utterance.
#[derive(Clone, Debug)]
pub(crate) struct Spoke {
    pub text: String,
    pub prio: Prio,
    pub epoch: Epoch,
}

#[derive(Clone, Debug)]
pub(crate) struct Violation {
    pub id: &'static str,
    pub at: usize,
    pub msg: String,
}

/// A small seeded generator (the offline registry has no `rand`/`proptest`).
#[derive(Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xD1B5_4A32_D192_ED03)
    }
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn pick<T: Copy>(&mut self, xs: &[T]) -> T {
        xs[usize::try_from(self.below(xs.len() as u64)).unwrap_or(0)]
    }
}

const NODES: [u64; 7] = [ENTRY, BUTTON, TERM, AREA, DLG_ENTRY, DLG_BTN, WIN];

const REVIEW_CMDS: [CoreCmd; 24] = [
    CoreCmd::ReviewToggle,
    CoreCmd::ReviewToggle,
    CoreCmd::ReviewToFocus,
    CoreCmd::ReviewModeNext,
    CoreCmd::ReviewPrevLine,
    CoreCmd::ReviewCurLine,
    CoreCmd::ReviewNextLine,
    CoreCmd::ReviewPrevWord,
    CoreCmd::ReviewCurWord,
    CoreCmd::ReviewNextWord,
    CoreCmd::ReviewPrevChar,
    CoreCmd::ReviewCurChar,
    CoreCmd::ReviewNextChar,
    CoreCmd::ReviewTop,
    CoreCmd::ReviewBottom,
    CoreCmd::ReviewStartOfLine,
    CoreCmd::ReviewEndOfLine,
    CoreCmd::ObjectCurrent,
    CoreCmd::ObjectParent,
    CoreCmd::ObjectFirstChild,
    CoreCmd::ObjectNext,
    CoreCmd::ObjectPrev,
    CoreCmd::ObjectNextInFlow,
    CoreCmd::ObjectPrevInFlow,
];

const OTHER_CMDS: [CoreCmd; 4] = [
    CoreCmd::EchoTypedCharsToggle,
    CoreCmd::EchoCommandKeysToggle,
    CoreCmd::SpeechPunctuationCycle,
    CoreCmd::InfoWhereAmI,
];

fn gen_key(g: &mut Rng) -> KeyKind {
    match g.below(20) {
        0..=7 => KeyKind::Letter(g.pick(&['a', 'b', 'k', 'z', 'q'])),
        8..=9 => KeyKind::Space,
        10..=11 => KeyKind::Punct(g.pick(&['!', '.', ',', '-', '/', '@', '$'])),
        12..=13 => KeyKind::Backspace,
        14 => KeyKind::Enter,
        15 => KeyKind::Tab,
        16 => KeyKind::Left,
        17 => KeyKind::Right,
        18 => KeyKind::Up,
        _ => g.pick(&[KeyKind::Down, KeyKind::Home, KeyKind::End]),
    }
}

pub(crate) fn gen_step(g: &mut Rng) -> Step {
    let dt = g.pick(&[1, 5, 20, 40, 80, 200, 600, 1700, 2600]);
    let op = match g.below(110) {
        0..=37 => {
            let m = match g.below(12) {
                0 => Modifiers::CONTROL,
                1 => Modifiers::MOD1,
                2 => Modifiers::SHIFT,
                _ => Modifiers::empty(),
            };
            Op::Type(gen_key(g), m)
        }
        38..=51 => Op::Focus {
            node: g.pick(&NODES),
            prelude: (g.below(4) == 0).then(|| g.pick(&NODES)),
            dups: g.pick(&[0, 0, 0, 1, 2]),
        },
        52..=75 => Op::Cmd(g.pick(&REVIEW_CMDS), 1 + g.below(2) as u8),
        76..=78 => Op::Cmd(g.pick(&OTHER_CMDS), 1),
        79..=84 => Op::Tick(g.pick(&[10, 31, 60, 400])),
        85 => Op::Remove(g.pick(&NODES)),
        86 => Op::OpenDialog,
        87 => Op::CloseDialog {
            refocus: g.below(2) == 0,
            to: g.pick(&[ENTRY, TERM, BUTTON]),
        },
        88 => Op::Activate(g.pick(&[WIN, DLG])),
        89..=90 => Op::TermAsync,
        91 => Op::Notify,
        92 => Op::ExtSpeech(g.below(2) == 0),
        93..=94 => Op::LoseEvents(g.below(2) == 0),
        95..=96 => Op::FailFetches(g.below(2) == 0),
        97 => Op::HoldReplies(g.below(2) == 0),
        98 => Op::Hung,
        99 => Op::Revive(g.pick(&NODES)),
        104..=105 => Op::Gtk4(g.below(2) == 0),
        106..=109 => Op::TermScroll,
        _ => Op::Ext(g.pick(&[ExtState::Absent, ExtState::Healthy, ExtState::Busy])),
    };
    Step { dt, op }
}

pub(crate) fn gen_sequence(seed: u64, len: usize) -> Vec<Step> {
    let mut g = Rng::new(seed);
    let n = 1 + usize::try_from(g.below(len as u64)).unwrap_or(0);
    let mut v: Vec<Step> = Vec::with_capacity(n + 2);
    if seed % 2 == 1 {
        v.push(Step {
            dt: 0,
            op: Op::WasmDeployment,
        });
    }
    // Most sequences start from a focused field, as a session does.
    v.push(Step {
        dt: 100,
        op: Op::Focus {
            node: g.pick(&[ENTRY, TERM, AREA, BUTTON]),
            prelude: None,
            dups: 0,
        },
    });
    for _ in 0..n {
        v.push(gen_step(&mut g));
    }
    v
}

// ---- the checker -------------------------------------------------------------------------

fn editable_focus(n: u64) -> bool {
    matches!(n, ENTRY | AREA | DLG_ENTRY | TERM)
}

/// Is the Reader's own picture of the focus the settled truth, with nothing in flight?
fn settled(s: &Sim) -> Option<u64> {
    let n = s.w.focus?;
    if !s.w.told
        || !s.w.alive.contains(&n)
        || !s.held.is_empty()
        || s.hold_replies
        || s.fail_fetches.is_some()
    {
        return None;
    }
    match s.r.focus() {
        FocusState::Focused { node, .. } if node == key(n) => Some(n),
        _ => None,
    }
}

/// The world terminal's rows, trailing blanks cut.
fn world_rows(s: &Sim) -> Vec<String> {
    s.w.bufs
        .get(&TERM)
        .map(|b| {
            b.string()
                .lines()
                .map(|l| l.trim_end().to_owned())
                .collect()
        })
        .unwrap_or_default()
}

fn tracking_ok(s: &Sim, n: u64) -> Result<(), String> {
    let text = s.r.text.caret.tracked().map(|t| t.root);
    let term = s.r.term_is(key(n));
    match n {
        ENTRY | AREA | DLG_ENTRY => {
            if text != Some(key(n)) || s.r.term_tracked() {
                return Err(format!(
                    "focused entry {n} but text layer tracks {text:?}, terminal tracked: {}",
                    s.r.term_tracked()
                ));
            }
        }
        TERM => {
            // No policy at all (no native build, no extension): nothing to track with.
            if !term && (s.native || s.ext_state() != ExtState::Absent) {
                return Err(format!(
                    "focused terminal not tracked (text layer tracks {text:?})"
                ));
            }
        }
        _ => {}
    }
    Ok(())
}

const KEY_NAMES: [&str; 5] = ["enter", "tab", "backspace", "escape", "delete"];

/// Run `steps` from a fresh world; `None` when every invariant holds.
pub(crate) fn run(steps: &[Step]) -> Option<Violation> {
    let r = catch_unwind(AssertUnwindSafe(|| run_inner(steps, false)));
    match r {
        Ok(v) => v,
        Err(p) => {
            let msg = p
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| p.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_default();
            Some(Violation {
                id: "panic",
                at: steps.len(),
                msg,
            })
        }
    }
}

/// Run `steps` printing what is spoken at every step (debugging aid).
#[allow(dead_code)]
pub(crate) fn trace(steps: &[Step]) -> Option<Violation> {
    run_inner(steps, true)
}

fn run_inner(steps: &[Step], trace: bool) -> Option<Violation> {
    let native = !matches!(
        steps.first(),
        Some(Step {
            op: Op::WasmDeployment,
            ..
        })
    );
    let mut s = Sim::new(native);
    let mut last_epoch = s.epoch.current();
    let mut last_key = 0u64;
    let mut key_gap = u64::MAX;
    let mut last_focus = 0u64;
    // The terminal's rows in the world before the last change and now (trailing blanks cut): the
    // oracle for review-drift, independent of the Reader's own models.
    let mut screens: (Vec<String>, Vec<String>) = (Vec::new(), world_rows(&Sim::new(native)));
    for (i, st) in steps.iter().enumerate() {
        let fail = |id: &'static str, msg: String| Some(Violation { id, at: i, msg });
        {
            let now_rows = world_rows(&s);
            if now_rows != screens.1 {
                screens.0 = std::mem::replace(&mut screens.1, now_rows);
            }
        }
        // A violation ends the run, except when tracing: then it is printed and the run goes on.
        macro_rules! bail {
            ($id:expr, $msg:expr $(,)?) => {{
                let v = fail($id, $msg);
                if trace {
                    eprintln!("   VIOLATION {v:?}");
                } else if !skipped($id) {
                    return v;
                }
            }};
        }
        // Replies a slow application held back arrive now, in either order.
        if !s.held.is_empty() {
            s.flush_held(i % 2 == 1);
        }
        s.settle(st.dt);
        s.spoken.clear();
        s.cues.clear();
        let mode_before = s.r.mode();
        let focus_before = s.w.focus;
        let pre = settled(&s);
        let tracked_pre = pre.is_some_and(|n| tracking_ok(&s, n).is_ok());
        let presented_before = matches!(
            s.r.focus(),
            FocusState::Focused {
                presented: true,
                ..
            }
        );
        let echo = s.r.cmd.effective.echo.clone();
        let probe_before = s.r.review_probe();
        match st.op {
            Op::Focus {
                node,
                prelude,
                dups,
            } => {
                if !s.w.alive.contains(&node) {
                    continue;
                }
                if let Some(p) = prelude.filter(|p| s.w.alive.contains(p)) {
                    s.handle(crate::reader::ReaderMsg::Focus(
                        crate::reader::FocusLaneMsg::FocusChanged { node: key(p) },
                    ));
                }
                s.failed_reads = false;
                last_focus = s.at;
                s.focus_to(node, true, dups);
                s.settle(150);
            }
            Op::Remove(n) => s.remove(n),
            Op::OpenDialog => {
                // The dialog's entry gets a focus event: the grace of a fresh focus applies.
                s.failed_reads = false;
                last_focus = s.at;
                s.open_dialog();
                s.settle(150);
            }
            Op::CloseDialog { refocus, to } => {
                if refocus {
                    s.failed_reads = false;
                    last_focus = s.at;
                }
                s.close_dialog(refocus, to);
                s.settle(150);
            }
            Op::Activate(w) => {
                if s.w.alive.contains(&w) {
                    s.handle(crate::reader::ReaderMsg::Focus(
                        crate::reader::FocusLaneMsg::WindowActivated { window: key(w) },
                    ));
                }
            }
            Op::Type(k, m) => {
                key_gap = s.at.saturating_sub(last_key);
                last_key = s.at;
                s.type_key(k, m);
                s.settle(250);
            }
            Op::Cmd(c, rep) => s.command(c, rep),
            Op::Tick(ms) => s.settle(ms),
            Op::TermAsync => {
                s.terminal_async_output();
                s.settle(250);
            }
            Op::TermScroll => {
                s.terminal_scroll_output();
                s.settle(30);
            }
            Op::Notify => s.announce(),
            Op::ExtSpeech(b) => s.ext_speech(b),
            Op::LoseEvents(b) => s.lose_events = b,
            Op::Gtk4(b) => s.gtk4 = b,
            Op::FailFetches(b) => {
                s.fail_fetches = b.then_some(gorca_connector_api::BackendError::Timeout);
            }
            Op::HoldReplies(b) => s.hold_replies = b,
            Op::Ext(e) => s.set_ext(e),
            Op::WasmDeployment => {}
            Op::Revive(n) => s.revive(n),
            Op::Hung => s.handle(crate::reader::ReaderMsg::Focus(
                crate::reader::FocusLaneMsg::AppHung(world::app()),
            )),
        }

        // -- epoch ------------------------------------------------------------------------
        let now_epoch = s.epoch.current();
        if now_epoch < last_epoch {
            bail!(
                "epoch",
                format!("epoch went back {last_epoch:?} -> {now_epoch:?}")
            );
        }
        last_epoch = now_epoch;
        if let Some(u) = s.spoken.iter().find(|u| u.epoch > now_epoch) {
            bail!("epoch", format!("utterance {u:?} from the future"));
        }

        if trace {
            eprintln!(
                "{i}: {:?} -> mode {}, spoke {:?}",
                st.op,
                match s.r.mode() {
                    Mode::Focus => "focus",
                    Mode::Review { .. } => "review",
                    _ => "other",
                },
                s.spoken.iter().map(|u| u.text.as_str()).collect::<Vec<_>>()
            );
        }
        let clean = s.fail_fetches.is_none() && !s.hold_replies && s.held.is_empty();
        let mode_after = s.r.mode();

        match st.op {
            Op::Type(k, m) => {
                let command_chord = m.intersects(Modifiers::CONTROL | Modifiers::MOD1);
                // key-name
                if !echo.command_keys
                    && !command_chord
                    // A text view echoes its own characters (tab, newline) by name: text echo.
                    && focus_before != Some(AREA)
                    && pre.is_some()
                    && let Some(u) = s.spoken.iter().find(|u| KEY_NAMES.contains(&u.text.as_str()))
                {
                    bail!(
                        "key-name",
                        format!("{k:?} spoke {:?} with command_keys off", u.text)
                    );
                }
                if let (Some(n), false) = (pre, command_chord)
                    && editable_focus(n)
                    && s.w.focus == Some(n)
                {
                    // echo-missing
                    if k.printable() && echo.typed_characters {
                        let want = match k {
                            KeyKind::Letter(c) => Some(c.to_string()),
                            KeyKind::Space => Some("space".to_owned()),
                            _ => None,
                        };
                        let ok = s.spoken.iter().any(|u| {
                            u.prio == Prio::Interrupt
                                && !u.text.is_empty()
                                && want.as_ref().is_none_or(|w| &u.text == w)
                        });
                        if !ok {
                            bail!(
                                "echo-missing",
                                format!(
                                    "{k:?} typed into {n} (mode before {mode_before:?}) spoke {:?}",
                                    s.spoken.iter().map(|u| u.text.as_str()).collect::<Vec<_>>()
                                ),
                            );
                        }
                    }
                    // caret-silent / caret-unit: a caret key into a settled entry or text view that
                    // the text layer tracks is answered, and by the character at the caret.
                    if m.is_empty()
                        && matches!(mode_before, Mode::Focus)
                        && matches!(n, ENTRY | AREA)
                        && matches!(
                            k,
                            KeyKind::Left | KeyKind::Right | KeyKind::Home | KeyKind::End
                        )
                        && clean
                        && !s.lose_events
                        && tracked_pre
                    {
                        let said: Vec<&str> = s
                            .spoken
                            .iter()
                            .filter(|u| u.prio == Prio::Interrupt && !u.text.is_empty())
                            .map(|u| u.text.as_str())
                            .collect();
                        // A key that moved nothing is answered by the boundary cue.
                        if said.is_empty() && s.cues.is_empty() {
                            bail!(
                                "caret-silent",
                                format!("{k:?} in {n} (gtk4: {}) was not answered", s.gtk4),
                            );
                        }
                        let at = s.w.bufs.get(&n).and_then(|b| b.text.get(b.caret).copied());
                        // (After lost events the window is stale: the layer cannot know the character.)
                        if let Some(c) = at.filter(|c| c.is_alphanumeric())
                            && !said.is_empty()
                            && s.dropped.load(Ordering::Relaxed) == 0
                            && !said.iter().any(|t| *t == c.to_string())
                        {
                            bail!(
                                "caret-unit",
                                format!(
                                    "{k:?} in {n} (gtk4: {}) landed on {c:?} and spoke {said:?}",
                                    s.gtk4
                                ),
                            );
                        }
                    }
                    // review-kept
                    if (k.printable() || k == KeyKind::Backspace)
                        && matches!(mode_before, Mode::Review { .. })
                        && !matches!(mode_after, Mode::Focus)
                    {
                        bail!(
                            "review-kept",
                            format!(
                                "{k:?} typed into {n} while in review left mode {mode_after:?}"
                            ),
                        );
                    }
                }
                // not-tracked: after a key a focused, settled node is tracked by its layer.
                // (The re-track and re-present on a key are rate limited: 500 ms and 2 s.)
                if !command_chord
                    && clean
                    && key_gap >= 2100
                    && (presented_before || s.at.saturating_sub(last_focus) >= 1600)
                    && matches!(mode_after, Mode::Focus)
                    && let Some(n) = settled(&s)
                    && let Err(e) = tracking_ok(&s, n)
                {
                    bail!("not-tracked", format!("after {k:?}: {e}"));
                }
                // term-degraded / term-silent: a terminal that is tracked and healthy reads
                // its output; a terminal tracked by the fail-closed watcher while its policy is
                // available again repairs itself on a key, not only on a focus.
                if !command_chord
                    && clean
                    && !s.lose_events
                    && matches!(mode_after, Mode::Focus)
                    && (s.native || s.ext_state() == ExtState::Healthy)
                    && settled(&s) == Some(TERM)
                    && s.w.focus == Some(TERM)
                {
                    if s.r.term_snapshot(key(TERM)).is_none() {
                        bail!(
                            if s.failed_reads {
                                "term-baseline"
                            } else {
                                "term-degraded"
                            },
                            format!(
                                "after {k:?} the terminal runs without its policy (ext {:?}) and nothing re-tracks it",
                                s.ext_state()
                            ),
                        );
                    }
                    // Without a snapshot the terminal is degraded (reported above): its silence
                    // is the same bug, not another one.
                    let want = match k {
                        _ if s.r.term_snapshot(key(TERM)).is_none() => None,
                        KeyKind::Enter => Some("out"),
                        KeyKind::Up | KeyKind::Down => Some("cmd"),
                        _ => None,
                    };
                    if let Some(w) = want
                        && !s
                            .spoken
                            .iter()
                            .map(|u| u.text.as_str())
                            .collect::<String>()
                            .replace(' ', "")
                            .contains(w)
                    {
                        bail!(
                            if s.failed_reads {
                                "term-baseline"
                            } else if matches!(mode_before, Mode::Review { .. }) {
                                "term-silent-after-review"
                            } else {
                                "term-silent"
                            },
                            format!(
                                "{k:?} in the terminal: output {w:?} never spoken, got {:?}",
                                s.spoken.iter().map(|u| u.text.as_str()).collect::<Vec<_>>()
                            ),
                        );
                    }
                }
            }
            Op::Focus { node, .. } => {
                if clean && s.w.alive.contains(&node) && s.w.focus == Some(node) {
                    match s.r.focus() {
                        FocusState::Focused {
                            node: f,
                            presented: true,
                            ..
                        } if f == key(node) => {}
                        other => {
                            bail!(
                                "focus-lost",
                                format!("focus event for {node}, reader is {other:?}")
                            );
                        }
                    }
                    if focus_before != Some(node) && !matches!(mode_after, Mode::Focus) {
                        bail!(
                            "focus-lost",
                            format!("focus moved to {node} but mode is {mode_after:?}")
                        );
                    }
                    if matches!(mode_after, Mode::Focus)
                        && let Err(e) = tracking_ok(&s, node)
                    {
                        bail!("not-tracked", format!("after the focus of {node}: {e}"));
                    }
                }
            }
            Op::Cmd(CoreCmd::ReviewToggle | CoreCmd::ReviewToFocus, _) => {
                if matches!(mode_before, Mode::Review { .. })
                    && matches!(mode_after, Mode::Review { .. })
                    && clean
                {
                    // A toggle with an odd repeat count leaves, an even one re-enters: only
                    // ReviewToFocus must always leave.
                    if matches!(st.op, Op::Cmd(CoreCmd::ReviewToFocus, _)) {
                        bail!("review-kept", "review.to_focus left review on".to_owned());
                    }
                }
            }
            Op::Cmd(
                c @ (CoreCmd::ReviewNextWord
                | CoreCmd::ReviewPrevWord
                | CoreCmd::ReviewNextChar
                | CoreCmd::ReviewPrevChar),
                _,
            ) => {
                if matches!(mode_before, Mode::Review { .. })
                    && matches!(mode_after, Mode::Review { .. })
                    && s.r.term_snapshot(key(TERM)).is_some()
                {
                    // review-wait
                    if s.r.review_waits_for_terminal() {
                        bail!(
                            "review-wait",
                            format!("{c:?} suspended on a screen read with a held screen")
                        );
                    }
                    if clean
                        && let (Some((t0, o0, _)), Some((t1, o1, on_screen))) =
                            (probe_before.as_ref(), s.r.review_probe())
                    {
                        use crate::review::segment::{Dir, TextUnits, Unit};
                        let (unit, dir) = match c {
                            CoreCmd::ReviewNextWord => (Unit::Word, Dir::Fwd),
                            CoreCmd::ReviewPrevWord => (Unit::Word, Dir::Back),
                            CoreCmd::ReviewNextChar => (Unit::Char, Dir::Fwd),
                            _ => (Unit::Char, Dir::Back),
                        };
                        // The unit this move has to land on while the line has room for it.
                        let room = s.r.term_units().next(t0, *o0, unit, dir);
                        // review-drift: the line is (still) on the screen, the move had room in
                        // it, yet the cursor is on another line. A line the world no longer has
                        // was rewritten or replaced: the cursor may then be on the row the old
                        // line had (ADR-0018 decision 1, a clamp to the rewritten line), and
                        // nowhere else. Both are read from the world's rows, not from the
                        // Reader's model.
                        let t0_trim = t0.trim_end();
                        let gone = !screens.1.iter().any(|l| l == t0_trim);
                        let carried = screens
                            .0
                            .iter()
                            .position(|l| l == t0_trim)
                            .and_then(|r| screens.1.get(r));
                        let drifted = if gone {
                            carried.is_some_and(|l| l != t1.trim_end())
                        } else {
                            true
                        };
                        if t1 != *t0 && on_screen && room.is_some() && drifted {
                            bail!(
                                "review-drift",
                                format!("{c:?} at {o0} of {t0:?} ended on {t1:?} at {o1}")
                            );
                        }
                        // review-reset: on the same line the cursor lands on that unit, not
                        // on the one after the line start.
                        if t1 == *t0
                            && let Some(r) = room
                            && r.start != o1
                        {
                            bail!(
                                "review-reset",
                                format!("{c:?} at {o0} of {t0:?} landed on {o1}, want {}", r.start)
                            );
                        }
                    }
                }
            }
            Op::Remove(_)
            | Op::OpenDialog
            | Op::CloseDialog { .. }
            | Op::Activate(_)
            | Op::Cmd(..)
            | Op::Tick(_)
            | Op::TermAsync
            | Op::TermScroll
            | Op::Notify
            | Op::ExtSpeech(_)
            | Op::LoseEvents(_)
            | Op::Gtk4(_)
            | Op::FailFetches(_)
            | Op::HoldReplies(_)
            | Op::Ext(_)
            | Op::WasmDeployment
            | Op::Revive(_)
            | Op::Hung => {}
        }
    }
    None
}

/// Remove chunks of the sequence while the same invariant still fails (delta debugging).
pub(crate) fn shrink(steps: &[Step], v: &Violation) -> (Vec<Step>, Violation) {
    let mut cur: Vec<Step> = steps[..(v.at + 1).min(steps.len())].to_vec();
    let mut best = v.clone();
    let mut chunk = (cur.len() / 2).max(1);
    loop {
        let mut progress = false;
        let mut i = 0;
        while i < cur.len() {
            let end = (i + chunk).min(cur.len());
            let mut cand = cur.clone();
            cand.drain(i..end);
            match run(&cand) {
                Some(nv) if nv.id == v.id => {
                    cur = cand;
                    best = nv;
                    progress = true;
                }
                Some(_) | None => i += chunk,
            }
        }
        if !progress {
            if chunk == 1 {
                break;
            }
            chunk = (chunk / 2).max(1);
        }
    }
    // Drop the time gaps that do not matter.
    for i in 0..cur.len() {
        let mut cand = cur.clone();
        cand[i].dt = 1;
        if run(&cand).is_some_and(|nv| nv.id == v.id) {
            cur = cand;
        }
    }
    let at = run(&cur).map_or(best.at, |nv| nv.at);
    best.at = at;
    (cur, best)
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Invariant ids that are known to fail on the current tree: each has an `#[ignore]`d regression
/// test in `regressions.rs` and a note in `docs/reports/reader-state-proptest.md`. Setting
/// `GORCA_STATE_STRICT=1` removes the allowance.
const KNOWN: &[&str] = &[];

thread_local! {
    /// While set, the checks of the `KNOWN` invariants are switched off (so that they do not
    /// hide the others); the `#[ignore]`d regressions run with it clear.
    static SKIP_KNOWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn skipped(id: &str) -> bool {
    SKIP_KNOWN.with(std::cell::Cell::get) && KNOWN.contains(&id)
}

#[test]
fn random_sequences() {
    let cases = env_u64("GORCA_STATE_CASES", 500);
    let seed0 = env_u64("GORCA_STATE_SEED", 1);
    let len = env_u64("GORCA_STATE_LEN", 40) as usize;
    let strict = std::env::var("GORCA_STATE_STRICT").is_ok();
    SKIP_KNOWN.with(|c| c.set(!strict));
    let mut found: std::collections::BTreeMap<&'static str, (u64, usize)> =
        std::collections::BTreeMap::default();
    let mut firsts: Vec<(u64, Violation)> = Vec::new();
    for c in 0..cases {
        let seed = seed0.wrapping_add(c);
        let steps = gen_sequence(seed, len);
        if let Some(v) = run(&steps) {
            let e = found.entry(v.id).or_insert((seed, 0));
            e.1 += 1;
            if e.1 == 1 {
                firsts.push((seed, v));
            }
        }
    }
    let mut unexpected = String::new();
    for (seed, v) in &firsts {
        let steps = gen_sequence(*seed, len);
        let (min, nv) = shrink(&steps, v);
        let known = !strict && KNOWN.contains(&v.id);
        let line = format!(
            "\n== {} ({} of {cases} sequences; first seed {seed}){}\n   {}\n   minimal sequence ({} steps, fails at step {}):\n{}",
            v.id,
            found[v.id].1,
            if known { " [known]" } else { "" },
            nv.msg,
            min.len(),
            nv.at,
            min.iter()
                .enumerate()
                .map(|(i, s)| format!("     {i}: +{}ms {:?}", s.dt, s.op))
                .collect::<Vec<_>>()
                .join("\n")
        );
        eprintln!("{line}");
        if !known {
            unexpected.push_str(&line);
        }
    }
    assert!(unexpected.is_empty(), "invariants violated:{unexpected}");
}
