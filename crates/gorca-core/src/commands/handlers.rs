// SPDX-License-Identifier: GPL-3.0-or-later
//! Command execution (ADR-0005 6): Reader messages that yield speech intents and effects.
//!
//! Every handler is synchronous and bounded. IPC-backed answers (`info.where_am_i`,
//! `info.window_title`, `debug.node_info`) read the mirror and, on a miss, emit one Interactive
//! fetch and finish on the reply (CR-3).

use gorca_i18n::{MsgId as M, msg, t};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_config::Confirmations;
use gorca_input_api::{CmdId, KeyIntent, PressKind, is_modifier_keysym, spoken_key_name};
use gorca_mirror::fill::{DemandOutcome, DemandWhy};
use gorca_model::{Epoch, Lookup, NodeKey, NodeSnapshot, Prio, PropMask, Role};
use gorca_rt::BudgetClass;

use super::info;
use super::profiles::Change;
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
use crate::text::caret::Spell;
use crate::text::{line_spell_for, spell_for};
use gorca_text::caret::Say;

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

    /// Speak a short notice behind what is being said, without cutting it.
    pub(crate) fn say_queued(&mut self, text: &str, fx: &mut ReaderEffects) {
        let id = self.utt_id();
        let mut u = message(text, id, self.epoch.current());
        u.prio = Prio::Message;
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// Speak text that holds what an application said (a window title, a node description):
    /// its symbols are named by the punctuation level on the way to the lane.
    pub(crate) fn say_app(&mut self, text: &str, fx: &mut ReaderEffects) {
        let id = self.utt_id();
        let mut u = message(text, id, self.epoch.current());
        u.prio = Prio::Interrupt;
        for s in &mut u.segments {
            s.kind = gorca_model::SegmentKind::Text;
        }
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

    /// A confirmation that also says where the change went ("for Firefox"), when it is worth
    /// saying (PE-3).
    fn confirm_noted(&mut self, full: &str, brief: &str, note: &str, fx: &mut ReaderEffects) {
        let text = if self.brief() { brief } else { full };
        if note.is_empty() {
            self.say(text, fx);
        } else {
            let both = msg!(ConfirmNoted, text = text, note = note);
            self.say(&both, fx);
        }
    }

    /// An extension command: queued to the host's worker, never run here (the Reader must not
    /// wait for an extension). The answer arrives as `BackgroundMsg::ExtSpeech`.
    fn run_ext_command(&mut self, cmd: CmdId) {
        let Some(spec) = crate::ExtCommands::installed().and_then(|e| e.get(cmd)) else {
            return;
        };
        let Some(node) = self.current_node() else {
            return;
        };
        if let Some(app) = self.ext.as_ref().and_then(|s| s.for_app(node.app)) {
            app.command(&spec.ext, spec.local);
        }
    }

    pub(crate) fn run_command(
        &mut self,
        cmd: CmdId,
        intent: &KeyIntent,
        now: Instant,
        fx: &mut ReaderEffects,
    ) {
        let Some(c) = CoreCmd::from_id(cmd) else {
            self.run_ext_command(cmd);
            return;
        };
        let rep = intent.repeat.max(1);
        // Sleep mode: the focused application owns its keys and gorca stays silent, except for
        // the toggle (and quit, so a sleeping reader can still be ended).
        if self.sleeping_now() && !matches!(c, CoreCmd::InputSleepToggle | CoreCmd::GorcaQuit) {
            return;
        }
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
                    self.say(t(M::SpeechOff), fx);
                    self.cmd.speech_on = false;
                    fx.push(ReaderEffect::SpeechCtl(SpeechCtl::Mute(true)));
                } else {
                    self.cmd.speech_on = true;
                    fx.push(ReaderEffect::SpeechCtl(SpeechCtl::Mute(false)));
                    self.say(t(M::SpeechOn), fx);
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
                None => self.say(t(M::NothingToRepeat), fx),
            },
            CoreCmd::SpeechHistoryPrev => self.history_step(true, fx),
            CoreCmd::SpeechHistoryNext => self.history_step(false, fx),
            CoreCmd::ProfilesSaveGlobal => self.profiles_save_global(rep, fx),
            CoreCmd::ProfilesCycle => self.profiles_cycle(fx),
            CoreCmd::ProfilesToggleEnabled => self.profiles_toggle_enabled(fx),
            CoreCmd::ProfilesEditTargetCycle => self.profiles_edit_target_cycle(fx),
            CoreCmd::ProfilesAnnounce => self.profiles_announce(rep, fx),
            CoreCmd::ProfilesSuspend => self.profiles_suspend(fx),
            CoreCmd::InputSleepToggle => self.sleep_toggle(fx),
            CoreCmd::InfoBattery => {
                fx.push(ReaderEffect::Desktop(
                    crate::present::DesktopEffect::Battery {
                        epoch: self.epoch.current(),
                    },
                ));
            }
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
            CoreCmd::DebugExplainLast => self.explain_last(fx),
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
                if let Some(note) = self.apply_change(&Change::TypedChars(v), fx) {
                    self.toggled(M::CmdTypedCharsOn, M::CmdTypedCharsOff, v, &note, fx);
                }
            }
            CoreCmd::EchoTypedWordsToggle => {
                let v = !self.cmd.effective.echo.typed_words;
                if let Some(note) = self.apply_change(&Change::TypedWords(v), fx) {
                    self.toggled(M::CmdTypedWordsOn, M::CmdTypedWordsOff, v, &note, fx);
                }
            }
            CoreCmd::EchoCommandKeysToggle => {
                let v = !self.cmd.effective.echo.command_keys;
                if let Some(note) = self.apply_change(&Change::CommandKeys(v), fx) {
                    self.toggled(M::CmdCommandKeysOn, M::CmdCommandKeysOff, v, &note, fx);
                }
            }
            CoreCmd::VerbosityCycle => {
                let v = self.cmd.effective.verbosity.level.next();
                let Some(note) = self.apply_change(&Change::Verbosity(v), fx) else {
                    return;
                };
                self.cmd.republish();
                let name = match v {
                    gorca_config::Verbosity::Brief => t(M::VerbosityBrief),
                    gorca_config::Verbosity::Normal => t(M::VerbosityNormal),
                    gorca_config::Verbosity::Verbose => t(M::VerbosityVerbose),
                };
                self.confirm_noted(&msg!(VerbosityConfirm, name = name), name, &note, fx);
            }
            CoreCmd::InputLearnModeToggle => {
                let entering = !matches!(self.mode, Mode::InputHelp { .. });
                self.on_mode(ModeMsg::ToggleInputHelp, fx);
                if entering {
                    self.confirm(t(M::LearnOn), t(M::LearnOnBrief), fx);
                } else {
                    self.confirm(t(M::LearnOff), t(M::LearnOffBrief), fx);
                }
            }
            CoreCmd::InputPassNextKey => {
                self.on_mode(ModeMsg::StartPassThrough(PassScope::OneKey), fx);
                if matches!(self.mode, Mode::PassThrough { .. }) {
                    self.cmd.pass_deadline = Some(now + PASS_TIMEOUT);
                    fx.push(ReaderEffect::ArmTimer(now + PASS_TIMEOUT));
                    self.confirm(t(M::PassNext), t(M::PassNextBrief), fx);
                }
            }
            CoreCmd::TextSayAll => self.say_all_start(intent.epoch, fx),
            CoreCmd::InfoSelection => self.text_say_selection(intent.epoch, fx),
            CoreCmd::InfoFormatting => self.text_say_formatting(intent.epoch, fx),
            CoreCmd::TextSayChar => self.text_say_unit(Say::Char, Spell::No, intent.epoch, fx),
            CoreCmd::TextSayWord => self.text_say_unit(Say::Word, Spell::No, intent.epoch, fx),
            CoreCmd::TextSayLine => self.text_say_unit(Say::Line, Spell::No, intent.epoch, fx),
            CoreCmd::TextSpellChar => {
                self.text_say_unit(Say::Char, spell_for(rep), intent.epoch, fx);
            }
            CoreCmd::TextSpellWord => {
                self.text_say_unit(Say::Word, spell_for(rep), intent.epoch, fx);
            }
            CoreCmd::TextSpellLine => {
                self.text_say_unit(Say::Line, spell_for(rep), intent.epoch, fx);
            }
            CoreCmd::InfoCurrentLine => {
                self.text_say_unit(Say::Line, line_spell_for(rep), intent.epoch, fx);
            }
            CoreCmd::SpeechPunctuationCycle => {
                let v = self.cmd.effective.speech.punctuation.next();
                let Some(note) = self.apply_change(&Change::Punctuation(v), fx) else {
                    return;
                };
                self.cmd.republish();
                // Said in the speech language, with the level named as the symbol table does.
                let table = gorca_speech::SymbolTable::of(gorca_speech::Lang::from_code(
                    &self.cmd.effective.speech.language,
                ));
                let rank = match v {
                    gorca_config::Punct::None => 0,
                    gorca_config::Punct::Some => 1,
                    gorca_config::Punct::Most => 2,
                    gorca_config::Punct::All => 3,
                };
                let (full, brief) = (
                    table.level_confirmation(rank, false),
                    table.level_confirmation(rank, true),
                );
                self.confirm_noted(&full, &brief, &note, fx);
            }
            CoreCmd::SettingsReload => fx.push(ReaderEffect::Config(ConfigRequest::Reload)),
            CoreCmd::SettingsSave => {
                if self.cmd.newer_schema {
                    self.say(t(M::SettingsNotSavedNewer), fx);
                } else if self.cmd.profiles.is_some() {
                    self.profiles_save(fx);
                } else {
                    fx.push(ReaderEffect::Config(ConfigRequest::Save(
                        self.cmd.overlay.clone(),
                    )));
                }
            }
            CoreCmd::SettingsReportProblems => {
                let n = self.cmd.problems.len();
                if n == 0 {
                    self.say(t(M::SettingsNoProblems), fx);
                } else {
                    let mut text = msg!(SettingsProblems, n = n);
                    for p in self.cmd.problems.iter().take(5) {
                        text.push_str(". ");
                        text.push_str(p);
                    }
                    self.say(&text, fx);
                }
            }
            CoreCmd::ObjectCurrent
            | CoreCmd::ObjectParent
            | CoreCmd::ObjectFirstChild
            | CoreCmd::ObjectNext
            | CoreCmd::ObjectPrev
            | CoreCmd::ObjectNextInFlow
            | CoreCmd::ObjectPrevInFlow
            | CoreCmd::ReviewPrevLine
            | CoreCmd::ReviewCurLine
            | CoreCmd::ReviewNextLine
            | CoreCmd::ReviewPrevWord
            | CoreCmd::ReviewCurWord
            | CoreCmd::ReviewNextWord
            | CoreCmd::ReviewPrevChar
            | CoreCmd::ReviewCurChar
            | CoreCmd::ReviewNextChar
            | CoreCmd::ReviewTop
            | CoreCmd::ReviewBottom
            | CoreCmd::ReviewStartOfLine
            | CoreCmd::ReviewEndOfLine
            | CoreCmd::ReviewModeNext
            | CoreCmd::ReviewModePrev
            | CoreCmd::ReviewToFocus
            | CoreCmd::ReviewMoveFocus
            | CoreCmd::ReviewActivate
            | CoreCmd::ReviewToggle
            | CoreCmd::ReviewSimpleToggle
            | CoreCmd::ReviewFocusFollowsToggle
            | CoreCmd::InfoStatusBar => self.review_command(c, rep, now, fx),
            CoreCmd::BrowseToggle
            | CoreCmd::BrowseActivate
            | CoreCmd::BrowseEscape
            | CoreCmd::BrowseCursorNextLine
            | CoreCmd::BrowseCursorPrevLine
            | CoreCmd::BrowseCursorNextChar
            | CoreCmd::BrowseCursorPrevChar
            | CoreCmd::BrowseCursorNextWord
            | CoreCmd::BrowseCursorPrevWord
            | CoreCmd::BrowseCursorLineStart
            | CoreCmd::BrowseCursorLineEnd
            | CoreCmd::BrowseCursorTop
            | CoreCmd::BrowseCursorBottom
            | CoreCmd::BrowseCursorPageUp
            | CoreCmd::BrowseCursorPageDown
            | CoreCmd::QuicknavNextHeading
            | CoreCmd::QuicknavPrevHeading
            | CoreCmd::QuicknavNextHeading1
            | CoreCmd::QuicknavNextHeading2
            | CoreCmd::QuicknavNextHeading3
            | CoreCmd::QuicknavNextHeading4
            | CoreCmd::QuicknavNextHeading5
            | CoreCmd::QuicknavNextHeading6
            | CoreCmd::QuicknavPrevHeading1
            | CoreCmd::QuicknavPrevHeading2
            | CoreCmd::QuicknavPrevHeading3
            | CoreCmd::QuicknavPrevHeading4
            | CoreCmd::QuicknavPrevHeading5
            | CoreCmd::QuicknavPrevHeading6
            | CoreCmd::QuicknavNextLink
            | CoreCmd::QuicknavNextVisited
            | CoreCmd::QuicknavNextUnvisited
            | CoreCmd::QuicknavNextFormField
            | CoreCmd::QuicknavNextList
            | CoreCmd::QuicknavNextListItem
            | CoreCmd::QuicknavNextButton
            | CoreCmd::QuicknavNextEdit
            | CoreCmd::QuicknavNextSeparator
            | CoreCmd::QuicknavNextRadio
            | CoreCmd::QuicknavNextCombobox
            | CoreCmd::QuicknavNextCheckbox
            | CoreCmd::QuicknavNextGraphic
            | CoreCmd::QuicknavNextBlockquote
            | CoreCmd::QuicknavNextLandmark
            | CoreCmd::QuicknavNextEmbedded
            | CoreCmd::QuicknavNextTable
            | CoreCmd::QuicknavPrevLink
            | CoreCmd::QuicknavPrevVisited
            | CoreCmd::QuicknavPrevUnvisited
            | CoreCmd::QuicknavPrevFormField
            | CoreCmd::QuicknavPrevList
            | CoreCmd::QuicknavPrevListItem
            | CoreCmd::QuicknavPrevButton
            | CoreCmd::QuicknavPrevEdit
            | CoreCmd::QuicknavPrevSeparator
            | CoreCmd::QuicknavPrevRadio
            | CoreCmd::QuicknavPrevCombobox
            | CoreCmd::QuicknavPrevCheckbox
            | CoreCmd::QuicknavPrevGraphic
            | CoreCmd::QuicknavPrevBlockquote
            | CoreCmd::QuicknavPrevLandmark
            | CoreCmd::QuicknavPrevEmbedded
            | CoreCmd::QuicknavPrevTable
            | CoreCmd::QuicknavNextFrame
            | CoreCmd::QuicknavPrevFrame
            | CoreCmd::QuicknavNextArticle
            | CoreCmd::TableNextRow
            | CoreCmd::TablePrevRow
            | CoreCmd::TableNextColumn
            | CoreCmd::TablePrevColumn
            | CoreCmd::TableFirstRow
            | CoreCmd::TableLastRow
            | CoreCmd::TableFirstColumn
            | CoreCmd::TableLastColumn
            | CoreCmd::TableSayRow
            | CoreCmd::TableSayColumn
            | CoreCmd::TableSayAllRow
            | CoreCmd::TableSayAllColumn
            | CoreCmd::BrowseContainerStart
            | CoreCmd::BrowseContainerEnd
            | CoreCmd::BrowseSingleLetterToggle
            | CoreCmd::QuicknavNextNotLinkBlock
            | CoreCmd::QuicknavPrevNotLinkBlock
            | CoreCmd::QuicknavNextAnnotation
            | CoreCmd::QuicknavPrevAnnotation
            | CoreCmd::QuicknavNextError
            | CoreCmd::QuicknavPrevError
            | CoreCmd::QuicknavNextParagraph
            | CoreCmd::QuicknavPrevParagraph
            | CoreCmd::BrowseSelectNextChar
            | CoreCmd::BrowseSelectPrevChar
            | CoreCmd::BrowseSelectNextWord
            | CoreCmd::BrowseSelectPrevWord
            | CoreCmd::BrowseSelectNextLine
            | CoreCmd::BrowseSelectPrevLine
            | CoreCmd::BrowseSelectLineStart
            | CoreCmd::BrowseSelectLineEnd
            | CoreCmd::BrowseSelectTop
            | CoreCmd::BrowseSelectBottom
            | CoreCmd::BrowseSelectPageUp
            | CoreCmd::BrowseSelectPageDown
            | CoreCmd::BrowseSelectAll
            | CoreCmd::BrowseCursorNextParagraph
            | CoreCmd::BrowseCursorPrevParagraph
            | CoreCmd::BrowseCursorNextSentence
            | CoreCmd::BrowseCursorPrevSentence
            | CoreCmd::BrowseTabNext
            | CoreCmd::BrowseTabPrev
            | CoreCmd::BrowseNativeSelectionToggle
            | CoreCmd::QuicknavPrevArticle => self.browse_command(c, rep, now, fx),
            CoreCmd::MouseLeftClick => {
                self.mouse_command(super::mouse::MouseCmd::LeftClick, rep, fx);
            }
            CoreCmd::MouseRightClick => {
                self.mouse_command(super::mouse::MouseCmd::RightClick, rep, fx);
            }
            CoreCmd::MouseMiddleClick => {
                self.mouse_command(super::mouse::MouseCmd::MiddleClick, rep, fx);
            }
            CoreCmd::MouseLeftClickItem => {
                self.mouse_command(super::mouse::MouseCmd::LeftClickItem, rep, fx);
            }
            CoreCmd::MouseRightClickItem => {
                self.mouse_command(super::mouse::MouseCmd::RightClickItem, rep, fx);
            }
            CoreCmd::MouseLeftLockToggle => {
                self.mouse_command(super::mouse::MouseCmd::LeftLockToggle, rep, fx);
            }
            CoreCmd::MouseRightLockToggle => {
                self.mouse_command(super::mouse::MouseCmd::RightLockToggle, rep, fx);
            }
            CoreCmd::MouseRouteToReview => {
                self.mouse_command(super::mouse::MouseCmd::RouteToReview, rep, fx);
            }
            CoreCmd::MouseReviewToPointer => {
                self.mouse_command(super::mouse::MouseCmd::ReviewToPointer, rep, fx);
            }
            CoreCmd::MouseSayPosition => {
                self.mouse_command(super::mouse::MouseCmd::SayPosition, rep, fx);
            }
            planned_cmds!() => {
                // Declared but not implemented: the keymap never routes these (inert), so this
                // is only reachable through a stale table. Say so instead of staying silent.
                self.say(t(M::CommandNotAvailable), fx);
            }
        }
    }

    fn toggled(&mut self, on_msg: M, off_msg: M, on: bool, note: &str, fx: &mut ReaderEffects) {
        self.cmd.republish();
        let (full, word) = if on {
            (on_msg, M::StateOn)
        } else {
            (off_msg, M::StateOff)
        };
        self.confirm_noted(t(full), t(word), note, fx);
    }

    fn say_ring(&mut self, fx: &mut ReaderEffects) {
        let ring = self.cmd.ring;
        let v = self.level(ring);
        self.say(&msg!(RingSay, name = ring.name(), v = v), fx);
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
        let change = match ring {
            Ring::Rate => Change::Rate(v),
            Ring::Pitch => Change::Pitch(v),
            Ring::Volume => Change::Volume(v),
        };
        let Some(note) = self.apply_change(&change, fx) else {
            return;
        };
        self.cmd.ring = ring;
        self.cmd.republish();
        self.send_levels(fx);
        self.confirm_noted(&format!("{} {v}", ring.name()), &v.to_string(), &note, fx);
    }

    /// Send the effective voice levels if they differ from what the speech lane has.
    pub(crate) fn send_levels(&mut self, fx: &mut ReaderEffects) {
        let l = self.cmd.levels();
        if self.cmd.sent_levels != Some(l) {
            self.cmd.sent_levels = Some(l);
            fx.push(ReaderEffect::SpeechCtl(SpeechCtl::SetParams(l)));
        }
        let c = self.cmd.capital_levels();
        if self.cmd.sent_capitals != Some(c) {
            self.cmd.sent_capitals = Some(c);
            fx.push(ReaderEffect::SpeechCtl(SpeechCtl::SetCapitals(c)));
        }
    }

    /// The node the user is on: the focused one, or the one about to be.
    pub(crate) fn current_node(&self) -> Option<NodeKey> {
        match self.focus {
            FocusState::Focused { node, .. } => Some(node),
            FocusState::Settling { candidate, .. } => Some(candidate),
            FocusState::Orphaned { last, .. } => Some(last),
            FocusState::NoFocus => None,
        }
    }

    /// Must the explain log withhold the text of what was said about `node`? A password field's
    /// words are never logged, whatever the opt-in. Fail closed: when the node has left the mirror
    /// (a dialog that closed) or the utterance was not about a node, secrecy cannot be determined
    /// and the text is withheld.
    pub(crate) fn explain_withholds_text(&self, node: Option<&Arc<NodeSnapshot>>) -> bool {
        match node {
            Some(n) => {
                crate::present::node::is_secret(&self.fused_entry(n), n)
                    || self.last_spoken_is_sensitive()
            }
            None => true,
        }
    }

    /// `debug.explain_last` (dev builds): log where the words of the last utterance came from and
    /// say a short summary. Reads the mirror only; the fused entry of the node the utterance was
    /// about is read as it is now. Never touches the key path.
    fn explain_last(&mut self, fx: &mut ReaderEffects) {
        let Some(u) = self.cmd.last_spoken.clone() else {
            self.say(t(M::ExplainNone), fx);
            return;
        };
        let node = self
            .cmd
            .last_spoken_node
            .and_then(|k| self.dir.lookup(k).hit());
        let entry = node.as_ref().and_then(|n| n.sem.entry.clone());
        let secret = self.explain_withholds_text(node.as_ref());
        let r = super::diag::log_report(&self.sources, &u, entry.as_deref(), secret);
        let text = msg!(
            ExplainSummary,
            segments = r.segments.len(),
            catalogue = r.catalogue(),
            literal = r.literal(),
            groups = r.group_count(),
            hedged = r.hedged(),
            tainted = r.tainted(),
        );
        self.say(&text, fx);
    }

    fn ask_node(&mut self, kind: PendingKind, repeat: u8, fx: &mut ReaderEffects) {
        let Some(node) = self.current_node() else {
            self.say(t(M::NoFocus), fx);
            return;
        };
        if kind == PendingKind::WhereAmI {
            // Policies see the ancestry demand, with what the mirror could serve for it.
            let outcome = match self.dir.view(node) {
                Lookup::Hit(v) if v.stale.is_empty() => DemandOutcome::Hit,
                Lookup::Hit(v) => DemandOutcome::Stale { mask: v.stale },
                Lookup::Miss => DemandOutcome::Miss,
                Lookup::Gone => DemandOutcome::Gone,
            };
            fx.push(ReaderEffect::Demand {
                node,
                why: DemandWhy::WhereAmI,
                outcome,
            });
        }
        match self.dir.lookup(node) {
            Lookup::Hit(snap) => self.answer_or_refetch(kind, repeat, &snap, fx),
            Lookup::Miss => {
                let epoch = self.epoch.current();
                self.cmd.pending = Some(PendingInfo {
                    kind,
                    node,
                    repeat,
                    epoch,
                    window: false,
                });
                fx.push(ReaderEffect::Fetch {
                    node,
                    mask: PropMask::PRESENT,
                    class: BudgetClass::Interactive,
                    epoch,
                    kind: crate::reader::FetchKind::Info,
                });
            }
            Lookup::Gone => self.say(t(M::FocusGone), fx),
        }
    }

    /// `info.window_title` re-reads the window itself (one Interactive fetch, bounded by the
    /// connector's deadline; the cached name answers when it times out). The title of a page
    /// changes without the frame's name being re-read, so the mirror copy may be arbitrarily
    /// old. Every other command answers from the snapshot at hand.
    fn answer_or_refetch(
        &mut self,
        kind: PendingKind,
        repeat: u8,
        snap: &Arc<NodeSnapshot>,
        fx: &mut ReaderEffects,
    ) {
        if kind == PendingKind::WindowTitle {
            let chain = info::ancestry(&self.dir, snap);
            if let Some(w) = chain
                .iter()
                .find(|n| matches!(n.role, Role::Window | Role::Dialog))
            {
                let epoch = self.epoch.current();
                let node = w.key;
                self.cmd.pending = Some(PendingInfo {
                    kind,
                    node,
                    repeat,
                    epoch,
                    window: true,
                });
                fx.push(ReaderEffect::Fetch {
                    node,
                    mask: PropMask::PRESENT,
                    class: BudgetClass::Interactive,
                    epoch,
                    kind: crate::reader::FetchKind::Info,
                });
                return;
            }
        }
        self.answer_node(kind, repeat, snap, fx);
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
        let words = self.chain_words(&chain);
        let level = self.cmd.effective.verbosity.level;
        let text = match kind {
            PendingKind::WhereAmI => info::where_am_i(&chain, &words, repeat, level),
            PendingKind::WindowTitle => {
                let app = snap.key.app;
                let title = info::window_title(&chain, &words).inspect(|t| {
                    self.cmd.last_title = Some((app, t.clone()));
                });
                let title = title.or_else(|| {
                    self.cmd
                        .last_title
                        .as_ref()
                        .filter(|(a, _)| *a == app)
                        .map(|(_, t)| t.clone())
                });
                match title {
                    Some(t) if repeat >= 2 => info::spell(&t),
                    Some(t) => t,
                    None => t(M::WindowTitleUnavailable).to_owned(),
                }
            }
            PendingKind::NodeInfo => info::node_info(&chain, &words),
        };
        self.say_app(&text, fx);
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
        // A fetch that failed answers from the cache: a possibly old title beats silence.
        match result.or_else(|| self.dir.lookup(node).hit()) {
            Some(snap) if p.kind == PendingKind::WindowTitle && !p.window => {
                self.answer_or_refetch(p.kind, p.repeat, &snap, fx);
            }
            Some(snap) => self.answer_node(p.kind, p.repeat, &snap, fx),
            None => {
                // No snapshot at all: the last title read for this application still beats
                // "not responding" for a window title.
                let last = self
                    .cmd
                    .last_title
                    .as_ref()
                    .filter(|(a, _)| *a == p.node.app && p.kind == PendingKind::WindowTitle)
                    .map(|(_, t)| t.clone());
                match last {
                    Some(t) => self.say_app(&t, fx),
                    None => self.say(t(M::AppNotResponding), fx),
                }
            }
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
        let key = key_description(intent);
        let ext_help = cmd
            .and_then(|c| crate::ExtCommands::installed().and_then(|e| e.get(c)))
            .map(|s| {
                gorca_i18n::ext::resolve_for(&s.help, gorca_i18n::language(), &s.ext).into_owned()
            });
        let help = match cmd.and_then(CoreCmd::from_id) {
            None if ext_help.is_some() => ext_help.unwrap_or_default(),
            Some(c) => {
                let h = c.spec().help_text();
                if let super::spec::Availability::Planned(_) = c.spec().available {
                    msg!(InputHelpPlanned, help = h)
                } else if c.spec().gate.is_some() && !self.cmd.pointer.is_open() {
                    // RG-7: a gated command is described normally, with a note while its gate is closed.
                    format!("{h}, {}", t(M::MouseNotAvailableNow))
                } else {
                    h.to_owned()
                }
            }
            None => t(M::InputHelpUnbound).to_owned(),
        };
        self.say(
            &msg!(InputHelpLine, key = key.as_str(), help = help.as_str()),
            fx,
        );
    }

    pub(crate) fn on_control(&mut self, m: ControlMsg, now: Instant, fx: &mut ReaderEffects) {
        match m {
            ControlMsg::Mode(m) => self.on_mode(m, fx),
            ControlMsg::Settings(SettingsMsg::Loaded(l)) => self.on_loaded(&l, fx),
            ControlMsg::Settings(SettingsMsg::ProfilesSaved(r)) => self.profiles_saved(&r, fx),
            ControlMsg::Settings(SettingsMsg::Prewarm(p)) => self.profiles_prewarmed(&p),
            ControlMsg::Settings(SettingsMsg::SaveResult(r)) => {
                let text = match r {
                    SaveResult::Saved => t(M::SettingsSaved),
                    SaveResult::FileChanged => t(M::SettingsNotSavedChanged),
                    SaveResult::NewerSchema => t(M::SettingsNotSavedNewer),
                    SaveResult::Failed => t(M::SettingsSaveFailed),
                };
                self.say(text, fx);
            }
            ControlMsg::Input(n) => self.on_input_notice(n, fx),
            ControlMsg::Pointer(m) => self.on_pointer(m, now, fx),
            ControlMsg::PointerReply(r) => self.on_pointer_reply(r, now, fx),
            ControlMsg::PointerHint(h) => self.cmd.pointer_hint = Some(h),
        }
    }

    fn on_input_notice(&mut self, n: InputNotice, fx: &mut ReaderEffects) {
        match n {
            InputNotice::HeldByOtherScreenReader => {
                if !self.cmd.told_held {
                    self.cmd.told_held = true;
                    self.announce(t(M::InputHeldByOther), fx);
                }
            }
            InputNotice::Unavailable => {
                if !self.cmd.told_unavailable {
                    self.cmd.told_unavailable = true;
                    self.announce(t(M::InputUnavailable), fx);
                }
            }
            InputNotice::Lost => {
                if !self.cmd.told_lost {
                    self.cmd.told_lost = true;
                    self.announce(t(M::InputUnavailable), fx);
                }
            }
            InputNotice::ModifierStuck(keysym) => {
                let name = match keysym {
                    0xff63 => t(M::InputKeyInsert),
                    0xff9e => t(M::InputKeyKpInsert),
                    0xffe5 => t(M::InputKeyCapsLock),
                    _ => t(M::InputKeyGorca),
                };
                self.announce(&msg!(InputStuck, name = name), fx);
            }
            InputNotice::Restored => {
                self.cmd.told_unavailable = false;
                if self.cmd.told_lost {
                    self.cmd.told_lost = false;
                    self.announce(t(M::InputRestored), fx);
                }
            }
        }
    }

    /// A new configuration (startup, file change, explicit reload, own save).
    fn on_loaded(&mut self, l: &Arc<LoadedSettings>, fx: &mut ReaderEffects) {
        let old = std::mem::replace(&mut self.cmd.file, l.settings.clone());
        if let Some(layers) = &l.profiles {
            self.profiles_loaded(layers, l.cause, fx);
        }
        match l.cause {
            LoadCause::Startup | LoadCause::Layouts => {}
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
            || self.keys.exact != l.mode_keys.exact
            || self.keys.quick_nav != l.mode_keys.quick_nav
            || self.keys.web_escape != l.mode_keys.web_escape
            || self.keys.letters != l.mode_keys.letters
            || self.keys.gated != l.mode_keys.gated;
        self.cmd.republish();
        // Messages and role words follow the speech language of the new settings.
        gorca_i18n::set_language(gorca_i18n::Lang::from_code(
            &self.cmd.effective.speech.language,
        ));
        // The grab set follows the keymap (HR-1): re-sent only when the keys changed, and
        // always once at startup, which is when the source learns what to grab.
        self.keys = l.mode_keys.clone();
        self.keys.open_gates = self.cmd.pointer.open_bits();
        // `mouse.enabled` (a Base key) drives the pointer gate; only a change moves it.
        if self.cmd.pointer.enabled() != l.settings.mouse.enabled {
            self.on_pointer(
                super::pointer_gate::PointerMsg::Enabled(l.settings.mouse.enabled),
                self.now,
                fx,
            );
        }
        if keys_changed || l.cause == LoadCause::Startup {
            fx.push(ReaderEffect::SetGrabSet(self.current_grabs()));
        }
        self.send_levels(fx);
        let new_problems = l.problems_hash != self.cmd.problems_hash;
        self.cmd.problems.clone_from(&l.problems);
        self.cmd.problems_hash = l.problems_hash;
        let n = l.problems.len();
        let reloaded = if n > 0 && new_problems {
            msg!(SettingsReloadedProblems, n = n)
        } else {
            t(M::SettingsReloaded).to_owned()
        };
        match l.cause {
            LoadCause::FileChanged | LoadCause::Explicit => {
                self.announce(&reloaded, fx);
            }
            LoadCause::Startup => {
                if n > 0 {
                    self.announce(&msg!(SettingsStartupProblems, n = n), fx);
                }
            }
            LoadCause::Saved => self.announce(t(M::SettingsSaved), fx),
            LoadCause::Layouts => {}
        }
        if l.caps_lock_dropped && !self.cmd.told_caps {
            self.cmd.told_caps = true;
            self.announce(t(M::InputCapsDropped), fx);
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
                    self.say(t(M::PassCancelled), fx);
                }
            }
            Some(d) => fx.push(ReaderEffect::ArmTimer(d)),
            None => {}
        }
    }
}

/// "gorca control T": the modifiers, then the key.
pub(crate) fn key_description(intent: &KeyIntent) -> String {
    use gorca_input_api::Modifiers;
    let mut parts: Vec<String> = Vec::new();
    let m = intent.chord.map_or(intent.event.modifiers, |c| c.mods);
    if m.contains(Modifiers::GORCA) {
        parts.push(t(M::ModGorca).to_owned());
    }
    if m.contains(Modifiers::CONTROL) {
        parts.push(t(M::ModControl).to_owned());
    }
    if m.contains(Modifiers::MOD1) {
        parts.push(t(M::ModAlt).to_owned());
    }
    if m.contains(Modifiers::MOD4) {
        parts.push(t(M::ModSuper).to_owned());
    }
    if m.contains(Modifiers::SHIFT) {
        parts.push(t(M::ModShift).to_owned());
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

    #[test]
    fn key_descriptions_are_said_in_the_ui_language() {
        let _ru = gorca_i18n::scoped(gorca_i18n::Lang::Ru);
        assert_eq!(
            key_description(&intent(
                0x74,
                Modifiers::GORCA | Modifiers::SHIFT | Modifiers::CONTROL
            )),
            "горка контрол шифт T"
        );
        assert_eq!(
            key_description(&intent(0xffc9, Modifiers::empty())),
            "эф 12"
        );
        assert_eq!(key_description(&intent(0x20, Modifiers::empty())), "пробел");
    }

    #[test]
    fn counted_messages_use_the_russian_plural_forms() {
        let _ru = gorca_i18n::scoped(gorca_i18n::Lang::Ru);
        let say = |n: usize| gorca_i18n::msg!(SettingsProblems, n = n);
        assert_eq!(say(1), "в настройках 1 проблема");
        assert_eq!(say(3), "в настройках 3 проблемы");
        assert_eq!(say(5), "в настройках 5 проблем");
        assert_eq!(say(21), "в настройках 21 проблема");
        let _en = gorca_i18n::scoped(gorca_i18n::Lang::En);
        assert_eq!(
            gorca_i18n::msg!(SettingsProblems, n = 1usize),
            "1 settings problem"
        );
        assert_eq!(
            gorca_i18n::msg!(SettingsProblems, n = 2usize),
            "2 settings problems"
        );
    }
}
