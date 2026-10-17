// SPDX-License-Identifier: GPL-3.0-or-later
//! F0 quick wins (docs/plan/phase-3-features.md): lock-key announcements, capital pitch, speech
//! history, battery and per-application sleep mode. Additive Reader state and handlers; the
//! keypress path only reads and writes plain fields (FP-2: no IPC on it).

use gorca_i18n::{MsgId as M, t_in, tf_in};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use gorca_input_api::{KeyClass, KeyIntent, Modifiers, PressKind};
use gorca_model::{AppId, Epoch, SegmentKind, UttId, Utterance};
use gorca_rt::BoxFuture;
use gorca_speech::{Lang, SymbolTable};
use smallvec::SmallVec;

use crate::SpeechIntent;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// Utterances the speech history keeps.
pub const HISTORY_CAP: usize = 100;
/// The UPower read gives up after this long; the user hears "unavailable".
pub const BATTERY_DEADLINE: Duration = Duration::from_millis(250);

// ---- lock keys ---------------------------------------------------------------------------

const KS_CAPS_LOCK: u32 = 0xffe5;
const KS_NUM_LOCK: u32 = 0xff7f;
const KS_SCROLL_LOCK: u32 = 0xff14;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum LockKey {
    Caps,
    Num,
    Scroll,
}

impl LockKey {
    fn of(keysym: u32) -> Option<LockKey> {
        match keysym {
            KS_CAPS_LOCK => Some(LockKey::Caps),
            KS_NUM_LOCK => Some(LockKey::Num),
            KS_SCROLL_LOCK => Some(LockKey::Scroll),
            _ => None,
        }
    }

    fn word(self) -> M {
        match self {
            LockKey::Caps => M::LockCaps,
            LockKey::Num => M::LockNum,
            LockKey::Scroll => M::LockScroll,
        }
    }
}

/// What the Reader believes of the lock keys. Caps and Num come from the modifier state of
/// every key event (Mutter reports the state before the key is applied); Scroll Lock has no
/// xkb indicator on GNOME, so it is counted from its own presses.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct LockState {
    pub caps: bool,
    pub num: bool,
    pub scroll: bool,
}

// ---- speech history ----------------------------------------------------------------------

/// Where a history step landed.
#[derive(Clone, Debug)]
pub(crate) enum HistoryStep {
    Empty,
    /// Already at the oldest or the newest end.
    Edge {
        oldest: bool,
    },
    Item(Utterance),
}

/// The last [`HISTORY_CAP`] spoken utterances, newest last, with a review cursor. Typed text
/// and secrets never get in (`Reader::after_message`).
#[derive(Debug, Default)]
pub(crate) struct SpeechHistory {
    items: VecDeque<Utterance>,
    /// Steps back from the newest item, `None` while the user is not reviewing.
    cursor: Option<usize>,
}

/// History is memory only; it is wiped when the Reader ends.
impl Drop for SpeechHistory {
    fn drop(&mut self) {
        self.items.clear();
    }
}

impl SpeechHistory {
    #[cfg(test)]
    pub(crate) fn items_len(&self) -> usize {
        self.items.len()
    }

    pub(crate) fn push(&mut self, u: Utterance) {
        if self.items.len() == HISTORY_CAP {
            self.items.pop_front();
        }
        self.items.push_back(u);
        self.cursor = None;
    }

    /// Step to the previous (older) or next (newer) item. The first step back speaks the newest.
    pub(crate) fn step(&mut self, back: bool) -> HistoryStep {
        let n = self.items.len();
        if n == 0 {
            return HistoryStep::Empty;
        }
        let next = match (self.cursor, back) {
            (None, true) => 0,
            (None | Some(0), false) => return HistoryStep::Edge { oldest: false },
            (Some(c), true) if c + 1 < n => c + 1,
            (Some(_), true) => return HistoryStep::Edge { oldest: true },
            (Some(c), false) => c - 1,
        };
        self.cursor = Some(next);
        match self.items.get(n - 1 - next) {
            Some(u) => HistoryStep::Item(u.clone()),
            None => HistoryStep::Empty,
        }
    }
}

// ---- battery -----------------------------------------------------------------------------

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum BatteryState {
    Charging,
    Discharging,
    Empty,
    FullyCharged,
    PendingCharge,
    Unknown,
}

/// What the power source (UPower's display device) reports.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PowerInfo {
    pub present: bool,
    pub percent: u8,
    pub state: BatteryState,
    /// Seconds to empty (discharging) or to full (charging), when known.
    pub secs_left: Option<u64>,
}

/// Read-only access to the battery. The daemon installs one (UPower over D-Bus); the Reader
/// never awaits it: the answer comes back as `BackgroundMsg::Battery`.
pub trait PowerSource: Send + Sync + 'static {
    fn read(&self) -> BoxFuture<'static, Option<PowerInfo>>;
}

static POWER: OnceLock<Arc<dyn PowerSource>> = OnceLock::new();

/// Install the power source (first call wins).
pub fn install_power_source(src: Arc<dyn PowerSource>) {
    let _ = POWER.set(src);
}

pub(crate) fn power_source() -> Option<Arc<dyn PowerSource>> {
    POWER.get().cloned()
}

// ---- state -------------------------------------------------------------------------------

#[derive(Debug, Default)]
pub(crate) struct QuickState {
    pub locks: LockState,
    pub history: SpeechHistory,
    pub sleeping: HashSet<AppId>,
    /// The sleep toggle's own answer, spoken even in a sleeping application.
    pub sleep_pass: Option<UttId>,
    /// The command that just ran must not enter the history (history steps).
    pub no_history: bool,
    /// Typing echo of this message: not kept in the history.
    pub echo_ids: SmallVec<[UttId; 2]>,
}

fn table(r: &Reader) -> &'static SymbolTable {
    SymbolTable::of(lang(r))
}

/// The speech language of the Reader's effective settings.
fn lang(r: &Reader) -> Lang {
    Lang::from_code(&r.cmd.effective.speech.language)
}

fn is_single_capital(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some(c), None) if c.is_uppercase())
}

impl Reader {
    // ---- capitals -------------------------------------------------------------------------

    /// A capital letter spoken alone (typed, or a character move). `speech.caps` is any
    /// combination: `word` puts "cap" before the letter here, `pitch` and `sound` mark the
    /// utterance (`SegmentKind::Capital`) and the speech lane adds the pitch change and the
    /// sound icon the settings ask for.
    pub(crate) fn mark_capital(&self, u: &mut Utterance) {
        let [seg] = u.segments.as_mut_slice() else {
            return;
        };
        if seg.kind != SegmentKind::Said {
            return;
        }
        // The text is the bare letter: the callers verbalize with `char_opts`, which leaves the
        // word to this function (so it is added once, whatever the table says about a letter).
        if !is_single_capital(&seg.text) {
            return;
        }
        let caps = self.cmd.effective.speech.caps;
        if caps.word {
            let said = format!("{} {}", table(self).cap(), seg.text);
            seg.text = Arc::from(said);
        }
        if caps.pitch || caps.sound {
            seg.kind = SegmentKind::Capital;
        }
    }

    // ---- lock keys ------------------------------------------------------------------------

    /// Watch every key event: the modifier bits of ordinary keys keep the lock picture true, a
    /// lock key that reached the application toggles its state and is announced. Caps Lock as
    /// the gorca modifier reaches the application only on the double tap (a single tap is
    /// swallowed and must stay silent).
    pub(crate) fn observe_locks(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) {
        let ev = &intent.event;
        let Some(key) = LockKey::of(ev.keysym) else {
            if ev.pressed {
                self.cmd.quick.locks.caps = ev.modifiers.contains(Modifiers::LOCK);
                self.cmd.quick.locks.num = ev.modifiers.contains(Modifiers::MOD2);
            }
            return;
        };
        let reached = matches!(
            intent.class,
            KeyClass::PassToApp | KeyClass::ModifierPassThrough
        );
        if !ev.pressed || intent.kind != PressKind::Press || !reached {
            return;
        }
        let locks = &mut self.cmd.quick.locks;
        let on = match key {
            LockKey::Caps => !ev.modifiers.contains(Modifiers::LOCK),
            LockKey::Num => !ev.modifiers.contains(Modifiers::MOD2),
            LockKey::Scroll => !locks.scroll,
        };
        match key {
            LockKey::Caps => locks.caps = on,
            LockKey::Num => locks.num = on,
            LockKey::Scroll => locks.scroll = on,
        }
        let lang = lang(self);
        let text = tf_in(
            lang,
            if on { M::LockOn } else { M::LockOff },
            &[("key", t_in(lang, key.word()).into())],
        );
        self.say(&text, fx);
    }

    // ---- speech history -------------------------------------------------------------------

    /// Speak a remembered utterance again, interrupting.
    fn respeak(&mut self, mut u: Utterance, fx: &mut ReaderEffects) {
        u.id = self.utt_id();
        u.epoch = self.epoch.current();
        u.prio = gorca_model::Prio::Interrupt;
        u.survives_focus_change = false;
        u.may_refine = false;
        u.refines = None;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// `speech.history_prev` / `speech.history_next`.
    pub(crate) fn history_step(&mut self, back: bool, fx: &mut ReaderEffects) {
        self.cmd.quick.no_history = true;
        let lang = lang(self);
        match self.cmd.quick.history.step(back) {
            HistoryStep::Empty => self.say(t_in(lang, M::HistoryEmpty), fx),
            HistoryStep::Edge { oldest } => {
                let w = if oldest {
                    M::HistoryOldest
                } else {
                    M::HistoryNewest
                };
                self.say(t_in(lang, w), fx);
            }
            HistoryStep::Item(u) => self.respeak(u, fx),
        }
    }

    // ---- sleep mode -----------------------------------------------------------------------

    /// Is the focused application in sleep mode?
    pub(crate) fn sleeping_now(&self) -> bool {
        !self.cmd.quick.sleeping.is_empty()
            && self
                .current_node()
                .is_some_and(|n| self.cmd.quick.sleeping.contains(&n.app))
    }

    /// `input.sleep_toggle`.
    pub(crate) fn sleep_toggle(&mut self, fx: &mut ReaderEffects) {
        let Some(node) = self.current_node() else {
            self.say(t_in(lang(self), M::NoFocus), fx);
            return;
        };
        let q = &mut self.cmd.quick;
        let on = !q.sleeping.remove(&node.app);
        if on {
            q.sleeping.insert(node.app);
        }
        let lang = lang(self);
        let before = fx.len();
        self.say(t_in(lang, if on { M::SleepOn } else { M::SleepOff }), fx);
        if let Some(ReaderEffect::Speak(s)) = fx.get(before) {
            self.cmd.quick.sleep_pass = Some(s.utterance.id);
        }
    }

    // ---- battery --------------------------------------------------------------------------

    /// The battery answer arrived (`BackgroundMsg::Battery`): speak it unless the user has
    /// pressed another key since.
    pub(crate) fn on_battery(
        &mut self,
        info: Option<PowerInfo>,
        epoch: Epoch,
        fx: &mut ReaderEffects,
    ) {
        if epoch < self.epoch.current() {
            return;
        }
        let text = battery_text(lang(self), info);
        self.say(&text, fx);
    }

    // ---- after every message --------------------------------------------------------------

    /// Sleep mode silences what the message produced (except the toggle's own answer); what is
    /// left is recorded in the speech history. Key echo, typing echo, the history commands'
    /// own output and secrets are not recorded.
    pub(crate) fn after_message(&mut self, record: bool, fx: &mut ReaderEffects) {
        let skip = std::mem::take(&mut self.cmd.quick.no_history) || !record;
        let echo = std::mem::take(&mut self.cmd.quick.echo_ids);
        if self.sleeping_now() {
            let pass = self.cmd.quick.sleep_pass;
            fx.retain(|e| match e {
                ReaderEffect::Speak(s) => Some(s.utterance.id) == pass,
                ReaderEffect::SayAllChunk { .. } => false,
                ReaderEffect::Fetch { .. }
                | ReaderEffect::Demand { .. }
                | ReaderEffect::FocusSettled { .. }
                | ReaderEffect::QueryActiveWindowFocus
                | ReaderEffect::ArmTimer(_)
                | ReaderEffect::SetGrabSet(_)
                | ReaderEffect::SpeechCtl(_)
                | ReaderEffect::Config(_)
                | ReaderEffect::ReviewNav { .. }
                | ReaderEffect::ReviewBuild(_)
                | ReaderEffect::ReviewRevalidate { .. }
                | ReaderEffect::ReviewStatusBar { .. }
                | ReaderEffect::ReviewAct { .. }
                | ReaderEffect::Web(_)
                | ReaderEffect::Desktop(_)
                | ReaderEffect::TextFetch { .. } => true,
            });
        }
        self.cmd.quick.sleep_pass = None;
        if skip {
            return;
        }
        // Anything said while a password field or a terminal secret prompt is in hand, and
        // anything marked sensitive, is kept as a placeholder, never as its text.
        let secret = self.text.sensitive_utt;
        let in_secret =
            self.term_secret_held() || self.text.caret.tracked().is_some_and(|t| t.kind.secret);
        for e in fx.iter() {
            let ReaderEffect::Speak(s) = e else { continue };
            let id = s.utterance.id;
            if echo.contains(&id) || s.utterance.segments.is_empty() {
                continue;
            }
            if in_secret || secret == Some(id) {
                let mut u = s.utterance.clone();
                let word = t_in(lang(self), M::HistorySecret);
                u.segments.clear();
                u.segments.push(gorca_model::Segment {
                    text: Arc::from(word),
                    kind: SegmentKind::Said,
                });
                u.lang = None;
                self.cmd.quick.history.push(u);
            } else {
                self.cmd.quick.history.push(s.utterance.clone());
            }
        }
    }
}

/// The spoken battery report, in `lang`.
pub(crate) fn battery_text(lang: Lang, info: Option<PowerInfo>) -> String {
    let Some(i) = info else {
        return t_in(lang, M::BatteryUnavailable).to_owned();
    };
    if !i.present {
        return t_in(lang, M::BatteryNone).to_owned();
    }
    let mut parts = vec![tf_in(lang, M::BatteryPercent, &[("n", i.percent.into())])];
    let state = match i.state {
        BatteryState::Charging => Some(M::BatteryCharging),
        BatteryState::Discharging => Some(M::BatteryDischarging),
        BatteryState::Empty => Some(M::BatteryEmpty),
        BatteryState::FullyCharged => Some(M::BatteryFull),
        BatteryState::PendingCharge => Some(M::BatteryPending),
        BatteryState::Unknown => None,
    };
    if let Some(w) = state {
        parts.push(t_in(lang, w).to_owned());
    }
    if let Some(secs) = i.secs_left.filter(|s| *s >= 60) {
        let (h, m) = (secs / 3600, secs % 3600 / 60);
        let dur = if h > 0 {
            tf_in(lang, M::BatteryHm, &[("h", h.into()), ("m", m.into())])
        } else {
            tf_in(lang, M::BatteryM, &[("m", m.into())])
        };
        parts.push(tf_in(lang, M::BatteryTime, &[("dur", dur.as_str().into())]));
    }
    parts.join(", ")
}
