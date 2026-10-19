// SPDX-License-Identifier: GPL-3.0-or-later
//! Messages and state of the command layer: settings snapshots from the config task, notices
//! from the input source, and the requests the Reader sends back out (ADR-0005 6, 7.6).

use std::sync::Arc;
use std::time::Instant;

use arc_swap::ArcSwap;
use gorca_config::profiles::{BuiltLayers, Effective, ProfileKey, ProfileSnapshot, SavePlan};
use gorca_config::{Overlay, Settings};
use gorca_input_api::LaneLinks;
use gorca_model::{NodeKey, Utterance};

use crate::mode::{ModeKeys, ModeMsg};

/// Why a configuration snapshot was published.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum LoadCause {
    /// First load at startup.
    Startup,
    /// The file changed on disk (inotify).
    FileChanged,
    /// `settings.reload`: revert to the saved file, dropping runtime changes.
    Explicit,
    /// Our own `settings.save` was written; the overlay is now part of the file.
    Saved,
    /// The desktop's keyboard layouts changed (K-8b): the same settings, new grab keysyms. Silent
    /// and the runtime overlay is kept.
    Layouts,
}

/// A validated configuration, ready for the Reader. Built by the daemon's config task.
#[derive(Clone, Debug)]
pub struct LoadedSettings {
    pub cause: LoadCause,
    pub settings: Settings,
    pub hash: u64,
    /// Rendered diagnostics, one sentence each (`settings.report_problems`).
    pub problems: Vec<Arc<str>>,
    /// Hash of the diagnostic set: spoken once per distinct set (CV-2).
    pub problems_hash: u64,
    pub newer_schema: bool,
    /// Keys the grab sets are built from, with Caps Lock already dropped if it switches layouts.
    pub mode_keys: ModeKeys,
    /// Caps Lock was dropped as a gorca key: announce it once.
    pub caps_lock_dropped: bool,
    pub clock_24h: bool,
    /// The compiled profiles and the layers under them (ADR-0016). `None`: this load has no
    /// profile data (tests, a Reader without a profile directory): edits use the overlay.
    pub profiles: Option<Arc<BuiltLayers>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SaveResult {
    Saved,
    /// HR-5: the file changed on disk since it was loaded.
    FileChanged,
    /// SV-3: the file is from a newer gorca.
    NewerSchema,
    Failed,
}

/// What the focus looks like to the profile triggers, owned so it can cross to the config task.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileFocus {
    pub app: Option<Arc<str>>,
    pub contexts: gorca_config::profiles::ContextSet,
    /// Host of the focused document (never the whole address, PC-5).
    pub host: Option<Arc<str>>,
}

impl ProfileFocus {
    /// The trigger facts of this focus.
    pub fn facts(&self) -> gorca_config::profiles::FocusFacts<'_> {
        gorca_config::profiles::FocusFacts {
            app: self
                .app
                .as_deref()
                .map(|name| gorca_config::profiles::AppFacts {
                    name,
                    exe: None,
                    desktop_id: None,
                }),
            window: None,
            contexts: self.contexts,
            site: self
                .host
                .as_deref()
                .map(|host| gorca_config::profiles::SiteFacts { host, path: None }),
        }
    }
}

/// Which write a profile save asks for.
#[derive(Clone, Debug)]
pub enum SaveKind {
    /// `settings.save`: every dirty delta to its own file (PE-5).
    All,
    /// `profiles.save_global` (PE-4): the touched keys of `profile_id` move to the global file.
    Global {
        profile_id: String,
        focus: ProfileFocus,
    },
    /// Second press of `profiles.save_global`: these keys also leave every other profile.
    Everywhere {
        keys: Vec<gorca_config::profiles::KeyRef>,
        except: String,
    },
}

/// A request to write profile files, planned and performed by the config task.
#[derive(Clone, Debug)]
pub struct SaveProfiles {
    /// Echoed in the answer: the Reader drops answers it no longer waits for (R-1).
    pub id: u64,
    /// The Reader's snapshot at the moment of the request.
    pub snapshot: Arc<ProfileSnapshot>,
    pub kind: SaveKind,
}

/// Fold the keys one step away from the focus, off the Reader (ADR-0016 SW-2).
#[derive(Clone, Debug)]
pub struct PrewarmRequest {
    pub snapshot: Arc<ProfileSnapshot>,
    pub focus: ProfileFocus,
}

/// Folded settings for keys the Reader may need next, tagged with the generation they were
/// folded from; the Reader drops entries of an older generation.
#[derive(Clone, Debug)]
pub struct PrewarmEntries {
    pub generation: u64,
    pub entries: Vec<(ProfileKey, Arc<Effective>)>,
}

/// How far a profile save got.
#[derive(Clone, Debug)]
pub enum ProfileSaveOutcome {
    /// The first `done` of `total` writes landed.
    Written { done: usize, total: usize },
    /// The plan was refused; nothing was written.
    Refused(gorca_config::profiles::SaveRefusal),
    /// The settings file is from a newer gorca (SV-3).
    NewerSchema,
    /// A write failed after `done` writes.
    Failed { done: usize },
}

/// The config task's answer to a [`SaveProfiles`] request.
#[derive(Clone, Debug)]
pub struct ProfilesSaved {
    /// The id of the request this answers.
    pub id: u64,
    pub plan: Option<Arc<SavePlan>>,
    pub outcome: ProfileSaveOutcome,
    pub kind: SaveKind,
}

#[derive(Clone, Debug)]
pub enum SettingsMsg {
    Loaded(Arc<LoadedSettings>),
    /// The result of a profile save; the Reader completes its snapshot with what was written.
    ProfilesSaved(Arc<ProfilesSaved>),
    /// Folded settings for the keys one step from the focus (SW-2).
    Prewarm(Arc<PrewarmEntries>),
    /// The outcome of `settings.save`. On success the new file contents follow as `Loaded`
    /// with [`LoadCause::Saved`].
    SaveResult(SaveResult),
}

/// Events from the keyboard source, for the user (ADR-0005 8.2, ADR-0001 7.3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InputNotice {
    /// Orca (or another reader) owns the keyboard monitor: commands are unavailable.
    HeldByOtherScreenReader,
    /// The keyboard monitor was lost (shell restart, name loss). Retrying.
    Lost,
    /// The monitor is authorised again and the grabs are restored.
    Restored,
    /// `WatchKeyboard` was refused or timed out.
    Unavailable,
    /// The Mutter replica predicts that this grabbed modifier (keysym) is stuck in Mutter 48.7's
    /// `grabbed_keypresses` after a long hold, so every key is swallowed. Nothing heals it
    /// without input injection except one tap of the modifier. Sent once per episode.
    ModifierStuck(u32),
}

/// Everything the Reader receives that is neither a key, a focus event nor a query reply.
#[derive(Clone, Debug)]
pub enum ControlMsg {
    Mode(ModeMsg),
    Settings(SettingsMsg),
    Input(InputNotice),
    /// Pointer availability and `mouse.enabled` (ADR-0025): moves the keypad-chord gate.
    Pointer(super::pointer_gate::PointerMsg),
    /// The answer to a pointer request (ADR-0025 EX-1).
    PointerReply(super::pointer_req::PointerReply),
}

/// Voice parameters, 0 to 100 (ADR-0005 6.2). Backends map them to their own ranges.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct VoiceLevels {
    pub rate: u8,
    pub pitch: u8,
    pub volume: u8,
}

/// What a capital letter spoken alone gets besides its text (`speech.caps`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CapitalLevels {
    /// Pitch change in percent of the pitch range, 0 when the pitch is not asked for.
    pub pitch_percent: u8,
    /// A sound icon before the letter.
    pub cue: bool,
}

/// Speech control, ordered with utterances (ADR-0005 6.2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SpeechCtl {
    SetParams(VoiceLevels),
    SetCapitals(CapitalLevels),
    Pause,
    Resume,
    /// `speech.mode_cycle`: drop new utterances while muted.
    Mute(bool),
}

/// Requests to the daemon's config task.
#[derive(Clone, Debug)]
pub enum ConfigRequest {
    /// Reload the file now; runtime changes are discarded.
    Reload,
    /// Merge these runtime changes into the file.
    Save(Overlay),
    /// Write profile files (ADR-0016 PE-5, PE-4); the answer is [`SettingsMsg::ProfilesSaved`].
    SaveProfiles(Box<SaveProfiles>),
    /// Fold the keys one step from the focus (SW-2); the answer is [`SettingsMsg::Prewarm`].
    Prewarm(Box<PrewarmRequest>),
}

/// The voice parameter the ring currently selects.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Ring {
    Rate,
    Pitch,
    Volume,
}

impl Ring {
    pub(crate) fn next(self) -> Self {
        match self {
            Ring::Rate => Ring::Pitch,
            Ring::Pitch => Ring::Volume,
            Ring::Volume => Ring::Rate,
        }
    }

    pub(crate) fn prev(self) -> Self {
        match self {
            Ring::Rate => Ring::Volume,
            Ring::Pitch => Ring::Rate,
            Ring::Volume => Ring::Pitch,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Ring::Rate => gorca_i18n::t(gorca_i18n::MsgId::RingRate),
            Ring::Pitch => gorca_i18n::t(gorca_i18n::MsgId::RingPitch),
            Ring::Volume => gorca_i18n::t(gorca_i18n::MsgId::RingVolume),
        }
    }
}

/// An answer that needs one Interactive fetch of the focused node (CR-3).
#[derive(Copy, Clone, Debug)]
pub(crate) struct PendingInfo {
    pub kind: PendingKind,
    pub node: NodeKey,
    pub repeat: u8,
    pub epoch: gorca_model::Epoch,
    /// `node` is the window itself, fetched fresh for `info.window_title`: the mirror's copy of
    /// a window name can be as old as the last event that named it (a page changes its title
    /// without the frame announcing it).
    pub window: bool,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum PendingKind {
    WhereAmI,
    WindowTitle,
    NodeInfo,
}

/// Wall-clock access for `info.time_date`; a trait so tests are deterministic.
pub trait WallClock: Send + Sync + std::fmt::Debug + 'static {
    fn now(&self) -> LocalTime;
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LocalTime {
    pub year: i32,
    /// 1 to 12.
    pub month: u8,
    pub day: u8,
    /// 0 = Monday.
    pub weekday: u8,
    pub hour: u8,
    pub minute: u8,
}

/// The system clock in the local time zone.
#[derive(Debug, Default)]
pub struct SystemWallClock;

impl WallClock for SystemWallClock {
    fn now(&self) -> LocalTime {
        use chrono::{Datelike, Timelike};
        let n = chrono::Local::now();
        LocalTime {
            year: n.year(),
            month: u8::try_from(n.month()).unwrap_or(1),
            day: u8::try_from(n.day()).unwrap_or(1),
            weekday: u8::try_from(n.weekday().num_days_from_monday()).unwrap_or(0),
            hour: u8::try_from(n.hour()).unwrap_or(0),
            minute: u8::try_from(n.minute()).unwrap_or(0),
        }
    }
}

/// Reader-owned command state: settings (file plus overlay), speech toggles, notices already
/// told, the last spoken text.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)]
pub(crate) struct CmdState {
    pub file: Settings,
    pub file_hash: u64,
    pub overlay: Overlay,
    pub effective: Arc<Settings>,
    pub published: Option<Arc<ArcSwap<Settings>>>,
    /// The profile stack (ADR-0016), once the config task delivered one.
    pub profiles: Option<super::profiles::ProfileRuntime>,
    /// Where the runtime publishes its snapshot (tests leave it unset).
    pub profile_cell: Option<Arc<gorca_config::profiles::ProfileCell>>,
    pub problems: Vec<Arc<str>>,
    pub problems_hash: u64,
    pub newer_schema: bool,
    pub clock_24h: bool,
    pub ring: Ring,
    pub speech_on: bool,
    pub paused: bool,
    pub last_spoken: Option<Utterance>,
    /// The node in focus when `last_spoken` was remembered: what `debug.explain_last` reads the
    /// fused entry of.
    pub last_spoken_node: Option<gorca_model::NodeKey>,
    pub pending: Option<PendingInfo>,
    /// The last window title that was read, with the application it belongs to: the answer
    /// when a later read finds no window at all (a chain with a gap, a fetch that failed).
    pub last_title: Option<(gorca_model::AppId, String)>,
    pub pass_deadline: Option<Instant>,
    pub links: Option<Arc<LaneLinks>>,
    pub clock: Arc<dyn WallClock>,
    /// Voice levels last sent to the speech lane.
    pub sent_levels: Option<VoiceLevels>,
    /// Capital-letter effects last sent to the speech lane.
    pub sent_capitals: Option<CapitalLevels>,
    pub told_held: bool,
    pub told_lost: bool,
    pub told_caps: bool,
    pub told_unavailable: bool,
    /// Ambiguous command chords answered with "again" (never executed).
    pub ambiguous_cues: u64,
    /// F0 quick wins: lock keys, speech history, sleep mode.
    pub quick: crate::quick::QuickState,
    /// When the keypad mouse chords are grabbed (ADR-0025 RG-5).
    pub pointer: super::pointer_gate::PointerGate,
    /// Drag locks (ADR-0025 2.6).
    pub holds: super::mouse::Holds,
    pub ptr_next: u64,
    pub ptr_pending: std::collections::HashMap<super::pointer_req::PtrReqId, super::mouse::Pending>,
}

impl CmdState {
    pub(crate) fn new() -> Self {
        let file = Settings::default();
        Self {
            pointer: super::pointer_gate::PointerGate::new(),
            holds: super::mouse::Holds::default(),
            ptr_next: 0,
            ptr_pending: std::collections::HashMap::new(),
            effective: Arc::new(file.clone()),
            file,
            file_hash: 0,
            overlay: Overlay::default(),
            published: None,
            profiles: None,
            profile_cell: None,
            problems: Vec::new(),
            problems_hash: 0,
            newer_schema: false,
            clock_24h: true,
            ring: Ring::Rate,
            speech_on: true,
            paused: false,
            last_spoken: None,
            last_spoken_node: None,
            pending: None,
            last_title: None,
            pass_deadline: None,
            links: None,
            clock: Arc::new(SystemWallClock),
            // The speech service starts at its own defaults, which are the schema defaults: only
            // a different value needs to be sent.
            sent_levels: Some(VoiceLevels {
                rate: 50,
                pitch: 50,
                volume: 100,
            }),
            // The lane starts with +30 on the pitch and no sound (the `speech.caps` default).
            sent_capitals: Some(CapitalLevels {
                pitch_percent: 30,
                cue: false,
            }),
            told_held: false,
            told_lost: false,
            told_caps: false,
            told_unavailable: false,
            ambiguous_cues: 0,
            quick: crate::quick::QuickState::default(),
        }
    }

    pub(crate) fn levels(&self) -> VoiceLevels {
        let s = &self.effective.speech;
        VoiceLevels {
            rate: s.rate,
            pitch: s.pitch,
            volume: s.volume,
        }
    }

    pub(crate) fn capital_levels(&self) -> CapitalLevels {
        let s = &self.effective.speech;
        CapitalLevels {
            pitch_percent: if s.caps.pitch { s.caps_pitch_change } else { 0 },
            cue: s.caps.sound,
        }
    }

    /// Recompute the effective snapshot and publish it (one `Arc` allocation per change).
    pub(crate) fn republish(&mut self) {
        self.effective = match &mut self.profiles {
            Some(rt) => rt.settings(),
            None => Arc::new(self.overlay.apply(&self.file)),
        };
        if let Some(p) = &self.published {
            p.store(Arc::clone(&self.effective));
        }
    }
}
