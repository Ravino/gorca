//! Command execution (ADR-0005 6): Reader messages that yield speech intents and effects.
//!
//! Every handler is synchronous and bounded. IPC-backed answers (`info.where_am_i`,
//! `info.window_title`, `debug.node_info`) read the mirror and, on a miss, emit one Interactive
//! fetch and finish on the reply (CR-3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_config::Confirmations;
use gorca_input_api::{CmdId, KeyIntent, PressKind, is_modifier_keysym, spoken_key_name};
use gorca_model::{Epoch, Lookup, NodeKey, NodeSnapshot, Prio, PropMask};
use gorca_rt::BudgetClass;

use super::info;
use super::state::{
    ConfigRequest, ControlMsg, InputNotice, LoadCause, LoadedSettings, PendingInfo, PendingKind,
    Ring, SaveResult, SettingsMsg, SpeechCtl,
};
use super::table::{CoreCmd, planned_cmds};
use crate::SpeechIntent;
use crate::focus::FocusState;
use crate::mode::{Mode, ModeMsg, PassScope};
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// How long pass-next-key waits for the key (ADR-0005 5.3).
pub const PASS_TIMEOUT: Duration = Duration::from_secs(10);
/// Large ring steps are this many small ones.
const LARGE_STEP: u8 = 4;

impl Reader {
    /// Speak `text` now: interrupts, at the current epoch (E-4), survives nothing else.
    pub(crate) fn say(&mut self, text: &str, fx: &mut ReaderEffects) {
        let id = self.utt_id();
        let mut u = message(text, id, self.epoch.current());
        u.prio = Prio::Interrupt;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// Speak a notice that no key caused: it must not be cancelled by the next focus change.
    pub(crate) fn announce(&mut self, text: &str, fx: &mut ReaderEffects) {
        let id = self.utt_id();
        let mut u = message(text, id, self.epoch.current());
        u.survives_focus_change = true;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    fn brief(&self) -> bool {
        self.cmd.effective.verbosity.confirmations == Confirmations::Brief
    }

    /// A confirmation: the full sentence, or only its short form with `confirmations = "brief"`.
    fn confirm(&mut self, full: &str, brief: &str, fx: &mut ReaderEffects) {
        let text = if self.brief() { brief } else { full };
        self.say(text, fx);
    }

    pub(crate) fn run_command(
        &mut self,
        cmd: CmdId,
        intent: &KeyIntent,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some(c) = CoreCmd::from_id(cmd) else {
            return;
        };
        let rep = intent.repeat.max(1);
        match c {
            CoreCmd::SpeechStop => {
                // The input lane advanced the epoch on the Control press: speech is already
                // cancelled. A pending answer for the old epoch is abandoned.
                self.cmd.pending = None;
            }
            CoreCmd::SpeechPauseToggle => {
                self.cmd.paused = !self.cmd.paused;
                fx.push(ReaderEffect::SpeechCtl(if self.cmd.paused {
                    SpeechCtl::Pause
                } else {
                    SpeechCtl::Resume
                }));
            }
            CoreCmd::SpeechModeCycle => {
                if self.cmd.speech_on {
                    // Spoken before muting, so the user hears that it happened.
                    self.say("speech off", fx);
                    self.cmd.speech_on = false;
                    fx.push(ReaderEffect::SpeechCtl(SpeechCtl::Mute(true)));
                } else {
                    self.cmd.speech_on = true;
                    fx.push(ReaderEffect::SpeechCtl(SpeechCtl::Mute(false)));
                    self.say("speech on", fx);
                }
            }
            CoreCmd::SpeechRepeatLast => match self.cmd.last_spoken.clone() {
                Some(u) => {
                    let id = self.utt_id();
                    let mut u = u;
                    u.id = id;
                    u.epoch = self.epoch.current();
                    u.prio = Prio::Interrupt;
                    u.survives_focus_change = false;
                    fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                }
                None => self.say("nothing to repeat", fx),
            },
            CoreCmd::SpeechRingNext => {
                self.cmd.ring = self.cmd.ring.next();
                self.say_ring(fx);
            }
            CoreCmd::SpeechRingPrev => {
                self.cmd.ring = self.cmd.ring.prev();
                self.say_ring(fx);
            }
            CoreCmd::SpeechRingInc => self.step_ring(self.cmd.ring, 1, fx),
            CoreCmd::SpeechRingDec => self.step_ring(self.cmd.ring, -1, fx),
            CoreCmd::SpeechRingIncLarge => self.step_ring(self.cmd.ring, i16::from(LARGE_STEP), fx),
            CoreCmd::SpeechRingDecLarge => {
                self.step_ring(self.cmd.ring, -i16::from(LARGE_STEP), fx);
            }
            CoreCmd::SpeechRateUp => self.step_ring(Ring::Rate, 1, fx),
            CoreCmd::SpeechRateDown => self.step_ring(Ring::Rate, -1, fx),
            CoreCmd::SpeechPitchUp => self.step_ring(Ring::Pitch, 1, fx),
            CoreCmd::SpeechPitchDown => self.step_ring(Ring::Pitch, -1, fx),
            CoreCmd::SpeechVolumeUp => self.step_ring(Ring::Volume, 1, fx),
            CoreCmd::SpeechVolumeDown => self.step_ring(Ring::Volume, -1, fx),
            CoreCmd::InfoWhereAmI => self.ask_node(PendingKind::WhereAmI, rep, fx),
            CoreCmd::InfoWindowTitle => self.ask_node(PendingKind::WindowTitle, rep, fx),
            CoreCmd::DebugNodeInfo | CoreCmd::DebugAppInfo => {
                self.ask_node(PendingKind::NodeInfo, rep, fx);
            }
            CoreCmd::InfoTimeDate => {
                let t = self.cmd.clock.now();
                let text = if rep >= 2 {
                    info::date_text(t)
                } else {
                    info::time_text(t, self.cmd.clock_24h)
                };
                self.say(&text, fx);
            }
            CoreCmd::EchoTypedCharsToggle => {
                let v = !self.cmd.effective.echo.typed_characters;
                self.cmd.overlay.typed_characters = Some(v);
                self.toggled("speak typed characters", v, fx);
            }
            CoreCmd::EchoTypedWordsToggle => {
                let v = !self.cmd.effective.echo.typed_words;
                self.cmd.overlay.typed_words = Some(v);
                self.toggled("speak typed words", v, fx);
            }
            CoreCmd::EchoCommandKeysToggle => {
                let v = !self.cmd.effective.echo.command_keys;
                self.cmd.overlay.command_keys = Some(v);
                self.toggled("speak command keys", v, fx);
            }
            CoreCmd::VerbosityCycle => {
                let v = self.cmd.effective.verbosity.level.next();
                self.cmd.overlay.verbosity = Some(v);
                self.cmd.republish();
                let name = v.name();
                self.confirm(&format!("verbosity {name}"), name, fx);
            }
            CoreCmd::InputLearnModeToggle => {
                let entering = !matches!(self.mode, Mode::InputHelp { .. });
                self.on_mode(ModeMsg::ToggleInputHelp, fx);
                if entering {
                    self.confirm("learn mode on, Escape to exit", "learn on", fx);
                } else {
                    self.confirm("learn mode off", "learn off", fx);
                }
            }
            CoreCmd::InputPassNextKey => {
                self.on_mode(ModeMsg::StartPassThrough(PassScope::OneKey), fx);
                if matches!(self.mode, Mode::PassThrough { .. }) {
                    self.cmd.pass_deadline = Some(now + PASS_TIMEOUT);
                    fx.push(ReaderEffect::ArmTimer(now + PASS_TIMEOUT));
                    self.confirm("pass next key", "pass", fx);
                }
            }
            CoreCmd::SettingsReload => fx.push(ReaderEffect::Config(ConfigRequest::Reload)),
            CoreCmd::SettingsSave => {
                if self.cmd.newer_schema {
                    self.say("not saved: settings file is from a newer version", fx);
                } else {
                    fx.push(ReaderEffect::Config(ConfigRequest::Save(
                        self.cmd.overlay.clone(),
                    )));
                }
            }
            CoreCmd::SettingsReportProblems => {
                let n = self.cmd.problems.len();
                if n == 0 {
                    self.say("no settings problems", fx);
                } else {
                    let mut text = format!("{n} settings problem{}", plural(n));
                    for p in self.cmd.problems.iter().take(5) {
                        text.push_str(". ");
                        text.push_str(p);
                    }
                    self.say(&text, fx);
                }
            }
            planned_cmds!() => {
                // Declared but not implemented: the keymap never routes these (inert), so this
                // is only reachable through a stale table. Say so instead of staying silent.
                self.say("command not available yet", fx);
            }
        }
    }

    fn toggled(&mut self, what: &str, on: bool, fx: &mut ReaderEffects) {
        self.cmd.republish();
        let word = if on { "on" } else { "off" };
        self.confirm(&format!("{what} {word}"), word, fx);
    }

    fn say_ring(&mut self, fx: &mut ReaderEffects) {
        let ring = self.cmd.ring;
        let v = self.level(ring);
        self.say(&format!("{} {v}", ring.name()), fx);
    }

    fn level(&self, ring: Ring) -> u8 {
        let s = &self.cmd.effective.speech;
        match ring {
            Ring::Rate => s.rate,
            Ring::Pitch => s.pitch,
            Ring::Volume => s.volume,
        }
    }

    /// Change a voice parameter. The new levels go to the speech lane **before** the
    /// confirmation, so "rate 55" is spoken at rate 55 (ADR-0005 6.2).
    fn step_ring(&mut self, ring: Ring, steps: i16, fx: &mut ReaderEffects) {
        let step = i16::from(self.cmd.effective.speech.rate_step);
        let cur = i16::from(self.level(ring));
        let v = u8::try_from((cur + steps * step).clamp(0, 100)).unwrap_or(50);
        match ring {
            Ring::Rate => self.cmd.overlay.rate = Some(v),
            Ring::Pitch => self.cmd.overlay.pitch = Some(v),
            Ring::Volume => self.cmd.overlay.volume = Some(v),
        }
        self.cmd.ring = ring;
        self.cmd.republish();
        self.send_levels(fx);
        self.confirm(&format!("{} {v}", ring.name()), &v.to_string(), fx);
    }

    /// Send the effective voice levels if they differ from what the speech lane has.
    pub(crate) fn send_levels(&mut self, fx: &mut ReaderEffects) {
        let l = self.cmd.levels();
        if self.cmd.sent_levels != Some(l) {
            self.cmd.sent_levels = Some(l);
            fx.push(ReaderEffect::SpeechCtl(SpeechCtl::SetParams(l)));
        }
    }

    /// The node the user is on: the focused one, or the one about to be.
    fn current_node(&self) -> Option<NodeKey> {
        match self.focus {
            FocusState::Focused { node, .. } => Some(node),
            FocusState::Settling { candidate, .. } => Some(candidate),
            FocusState::Orphaned { last, .. } => Some(last),
            FocusState::NoFocus => None,
        }
    }

    fn ask_node(&mut self, kind: PendingKind, repeat: u8, fx: &mut ReaderEffects) {
        let Some(node) = self.current_node() else {
            self.say("no focus", fx);
            return;
        };
        match self.dir.lookup(node) {
            Lookup::Hit(snap) => self.answer_node(kind, repeat, &snap, fx),
            Lookup::Miss => {
                let epoch = self.epoch.current();
                self.cmd.pending = Some(PendingInfo {
                    kind,
                    node,
                    repeat,
                    epoch,
                });
                fx.push(ReaderEffect::Fetch {
                    node,
                    mask: PropMask::PRESENT,
                    class: BudgetClass::Interactive,
                    epoch,
                    kind: crate::reader::FetchKind::Info,
                });
            }
            Lookup::Gone => self.say("focused object is gone", fx),
        }
    }

    /// Finish a node-based command from a snapshot (mirror hit, or the fetch reply).
    pub(crate) fn answer_node(
        &mut self,
        kind: PendingKind,
        repeat: u8,
        snap: &Arc<NodeSnapshot>,
        fx: &mut ReaderEffects,
    ) {
        let chain = info::ancestry(&self.dir, snap);
        let level = self.cmd.effective.verbosity.level;
        let text = match kind {
            PendingKind::WhereAmI => info::where_am_i(&chain, repeat, level),
            PendingKind::WindowTitle => match info::window_title(&chain) {
                Some(t) if repeat >= 2 => info::spell(&t),
                Some(t) => t,
                None => "window title not available".to_owned(),
            },
            PendingKind::NodeInfo => info::node_info(&chain),
        };
        self.say(&text, fx);
    }

    /// A reply for a pending command. `true` when it was consumed by one.
    pub(crate) fn take_pending_reply(
        &mut self,
        node: NodeKey,
        epoch: Epoch,
        result: Option<Arc<NodeSnapshot>>,
        fx: &mut ReaderEffects,
    ) -> bool {
        let Some(p) = self.cmd.pending else {
            return false;
        };
        if p.node != node || p.epoch != epoch {
            return false;
        }
        self.cmd.pending = None;
        match result.or_else(|| self.dir.lookup(node).hit()) {
            Some(snap) => self.answer_node(p.kind, p.repeat, &snap, fx),
            None => self.say("application not responding", fx),
        }
        true
    }

    /// Learn mode: describe the key, do not run it (LM-2).
    pub(crate) fn describe(
        &mut self,
        intent: &KeyIntent,
        cmd: Option<CmdId>,
        fx: &mut ReaderEffects,
    ) {
        let mut text = key_description(intent);
        text.push_str(": ");
        match cmd.and_then(CoreCmd::from_id) {
            Some(c) => {
                text.push_str(c.spec().help);
                if let super::spec::Availability::Planned(_) = c.spec().available {
                    text.push_str(", not available yet");
                }
            }
            None => text.push_str("not bound"),
        }
        self.say(&text, fx);
    }

    pub(crate) fn on_control(&mut self, m: ControlMsg, now: Instant, fx: &mut ReaderEffects) {
        match m {
            ControlMsg::Mode(m) => self.on_mode(m, fx),
            ControlMsg::Settings(SettingsMsg::Loaded(l)) => self.on_loaded(&l, fx),
            ControlMsg::Settings(SettingsMsg::SaveResult(r)) => {
                let text = match r {
                    SaveResult::Saved => "settings saved",
                    SaveResult::FileChanged => "not saved: settings file changed on disk",
                    SaveResult::NewerSchema => "not saved: settings file is from a newer version",
                    SaveResult::Failed => "settings could not be saved",
                };
                self.say(text, fx);
            }
            ControlMsg::Input(n) => self.on_input_notice(n, fx),
        }
        let _ = now;
    }

    fn on_input_notice(&mut self, n: InputNotice, fx: &mut ReaderEffects) {
        match n {
            InputNotice::HeldByOtherScreenReader => {
                if !self.cmd.told_held {
                    self.cmd.told_held = true;
                    self.announce(
                        "Another screen reader holds the keyboard; gorca commands are unavailable",
                        fx,
                    );
                }
            }
            InputNotice::Unavailable => {
                if !self.cmd.told_unavailable {
                    self.cmd.told_unavailable = true;
                    self.announce("keyboard commands unavailable", fx);
                }
            }
            InputNotice::Lost => {
                if !self.cmd.told_lost {
                    self.cmd.told_lost = true;
                    self.announce("keyboard commands unavailable", fx);
                }
            }
            InputNotice::Restored => {
                self.cmd.told_unavailable = false;
                if self.cmd.told_lost {
                    self.cmd.told_lost = false;
                    self.announce("keyboard commands restored", fx);
                }
            }
        }
    }

    /// A new configuration (startup, file change, explicit reload, own save).
    fn on_loaded(&mut self, l: &Arc<LoadedSettings>, fx: &mut ReaderEffects) {
        let old = std::mem::replace(&mut self.cmd.file, l.settings.clone());
        match l.cause {
            LoadCause::Startup => {}
            LoadCause::FileChanged => self.cmd.overlay.drop_changed(&old, &l.settings),
            LoadCause::Explicit | LoadCause::Saved => {
                self.cmd.overlay = gorca_config::Overlay::default();
            }
        }
        self.cmd.file_hash = l.hash;
        self.cmd.newer_schema = l.newer_schema;
        self.cmd.clock_24h = l.clock_24h;
        let keys_changed = self.keys.modifiers != l.mode_keys.modifiers
            || self.keys.commands != l.mode_keys.commands
            || self.keys.quick_nav != l.mode_keys.quick_nav;
        self.cmd.republish();
        // The grab set follows the keymap (HR-1): re-sent only when the keys changed, and
        // always once at startup, which is when the source learns what to grab.
        self.keys = l.mode_keys.clone();
        if keys_changed || l.cause == LoadCause::Startup {
            fx.push(ReaderEffect::SetGrabSet(self.mode.grab_set(&self.keys)));
        }
        self.send_levels(fx);
        let new_problems = l.problems_hash != self.cmd.problems_hash;
        self.cmd.problems.clone_from(&l.problems);
        self.cmd.problems_hash = l.problems_hash;
        let n = l.problems.len();
        let tail = if n > 0 && new_problems {
            format!(
                ", {n} problem{}. Press gorca control shift R for details",
                plural(n)
            )
        } else {
            String::new()
        };
        match l.cause {
            LoadCause::FileChanged | LoadCause::Explicit => {
                self.announce(&format!("settings reloaded{tail}"), fx);
            }
            LoadCause::Startup => {
                if n > 0 {
                    self.announce(&format!("Settings: {n} problem{}", plural(n)), fx);
                }
            }
            LoadCause::Saved => self.announce("settings saved", fx),
        }
        if l.caps_lock_dropped && !self.cmd.told_caps {
            self.cmd.told_caps = true;
            self.announce(
                "Caps Lock is used for something else, so the gorca key is Insert",
                fx,
            );
        }
    }

    /// Pass-next-key ends on the first non-modifier press, or after the timeout.
    pub(crate) fn pass_key_seen(&mut self, intent: &KeyIntent, fx: &mut ReaderEffects) {
        if let Mode::PassThrough {
            scope: PassScope::OneKey,
            ..
        } = self.mode
            && intent.kind == PressKind::Press
            && !is_modifier_keysym(intent.event.keysym)
            && !self.keys.modifiers.contains(&intent.event.keysym)
        {
            self.cmd.pass_deadline = None;
            self.on_mode(ModeMsg::EndPassThrough, fx);
        }
    }

    pub(crate) fn pass_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        match self.cmd.pass_deadline {
            Some(d) if now >= d => {
                self.cmd.pass_deadline = None;
                if matches!(self.mode, Mode::PassThrough { .. }) {
                    self.on_mode(ModeMsg::EndPassThrough, fx);
                    self.say("pass next key cancelled", fx);
                }
            }
            Some(d) => fx.push(ReaderEffect::ArmTimer(d)),
            None => {}
        }
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// "gorca control T": the modifiers, then the key.
pub(crate) fn key_description(intent: &KeyIntent) -> String {
    use gorca_input_api::Modifiers;
    let mut parts: Vec<String> = Vec::new();
    let m = intent.chord.map_or(intent.event.modifiers, |c| c.mods);
    if m.contains(Modifiers::GORCA) {
        parts.push("gorca".to_owned());
    }
    if m.contains(Modifiers::CONTROL) {
        parts.push("control".to_owned());
    }
    if m.contains(Modifiers::MOD1) {
        parts.push("alt".to_owned());
    }
    if m.contains(Modifiers::MOD4) {
        parts.push("super".to_owned());
    }
    if m.contains(Modifiers::SHIFT) {
        parts.push("shift".to_owned());
    }
    parts.push(spoken_key_name(intent.event.keysym, intent.event.unichar));
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::{ChordKey, KeyClass, KeyEvent, KeyRef, Modifiers};

    fn intent(keysym: u32, mods: Modifiers) -> KeyIntent {
        KeyIntent {
            event: KeyEvent {
                keycode: 28,
                keysym,
                unichar: keysym,
                modifiers: Modifiers::empty(),
                pressed: true,
            },
            kind: PressKind::Press,
            class: KeyClass::Swallowed,
            repeat: 1,
            chord: Some(ChordKey {
                mods,
                key: KeyRef::Code(28),
            }),
            epoch: Epoch(1),
            at: Instant::now(),
        }
    }

    #[test]
    fn key_descriptions_list_modifiers_in_a_fixed_order() {
        assert_eq!(key_description(&intent(0x74, Modifiers::GORCA)), "gorca T");
        assert_eq!(
            key_description(&intent(
                0x74,
                Modifiers::GORCA | Modifiers::SHIFT | Modifiers::CONTROL
            )),
            "gorca control shift T"
        );
        assert_eq!(key_description(&intent(0xffc9, Modifiers::empty())), "f 12");
    }
}
