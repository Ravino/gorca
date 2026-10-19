// SPDX-License-Identifier: GPL-3.0-or-later
//! Profiles in the Reader (ADR-0016 slices P4 and P5): the state machine, the focus inputs and
//! the profile key, the published snapshot, edits that go to the focused application's profile,
//! and the profile commands.
//!
//! The Reader is the single writer of the snapshot (INV-P4). A key press never folds layers
//! unless the cache misses: [`prewarm`] (run by the config task) folds the keys one step from
//! the focus, and the cache is tied to the snapshot generation (F-6).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_config::profiles::{
    AppCandidates, AppFacts, ContextSet, Edit, EditPlan, EditRefusal, EditTargetKind,
    EffectiveCache, FocusContext, KeyRef, Pin, ProfileCell, ProfileKey, ProfileMode,
    ProfileSnapshot, ProfileSource, SaveRefusal, TargetWhy, initial_snapshot,
};
use gorca_config::{KeyId, ModeDefault, Overlay, Punct, Settings, Value, Verbosity};
use gorca_i18n::{MsgId as M, msg, t};

use super::state::{
    ConfigRequest, PrewarmEntries, PrewarmRequest, ProfileFocus, ProfileSaveOutcome, ProfilesSaved,
    SaveKind, SaveProfiles,
};
use crate::browse::WebMode;
use crate::mode::Mode;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// The cache holds this many folded keys (ADR-0016 SW-1).
const CACHE_ENTRIES: usize = 64;
/// A confirmation names its target once per burst of changes: the same target again within this
/// time stays short (PE-3).
const BURST: Duration = Duration::from_secs(4);
/// A save the config task never answered stops blocking edits after this.
const SAVE_TIMEOUT: Duration = Duration::from_secs(10);
/// Pressed twice within this time, `profiles.save_global` also applies everywhere (PE-4).
const EVERYWHERE_WINDOW: Duration = Duration::from_secs(10);

// ---- the state machine (ADR-0016 section 2.4) -----------------------------------------------

/// Why profile switching is held.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Suspend {
    /// The Reader is in a mode of its own (learn mode): triggers are not re-evaluated (TR-4).
    OwnMode,
    /// `profiles.suspend`: only the layers that survive suspension fold.
    User,
    Both,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProfileState {
    Active,
    /// `pending`: the inputs changed while held and are evaluated once on leaving.
    Suspended {
        why: Suspend,
        pending: bool,
    },
    /// Safe mode: only L0, L1, product rows and L5 reload.
    Disabled,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum ProfileEvent {
    /// The focus, the document host or the browse/focus mode changed.
    Inputs,
    OwnModeEntered,
    OwnModeLeft,
    /// `profiles.suspend`.
    UserSuspend,
    /// The snapshot's compiled set was replaced.
    Reload,
    SafeMode,
}

/// What a transition asks the runtime to evaluate.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Eval {
    No,
    /// Take the latest inputs, then resolve.
    Adopt,
    /// Resolve again with the inputs already in force.
    Refold,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Say {
    Nothing,
    Off,
    On,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Step {
    pub state: ProfileState,
    pub eval: Eval,
    pub say: Say,
}

const fn step_of(state: ProfileState, eval: Eval, say: Say) -> Step {
    Step { state, eval, say }
}

/// The transition table of section 2.4, one arm per (state, event).
#[allow(clippy::match_same_arms)] // one row per table entry, kept apart on purpose
pub(crate) fn step(state: ProfileState, ev: ProfileEvent) -> Step {
    use Eval::{Adopt, No, Refold};
    use ProfileEvent as E;
    use ProfileState as S;
    use Say::{Nothing, Off, On};
    use Suspend as W;
    let held = |why: W, pending: bool| S::Suspended { why, pending };
    match state {
        S::Active => match ev {
            E::Inputs => step_of(S::Active, Adopt, Nothing),
            E::OwnModeEntered => step_of(held(W::OwnMode, false), No, Nothing),
            E::OwnModeLeft => step_of(S::Active, No, Nothing),
            E::UserSuspend => step_of(held(W::User, false), Refold, Off),
            E::Reload => step_of(S::Active, Refold, Nothing),
            E::SafeMode => step_of(S::Disabled, Refold, Nothing),
        },
        S::Suspended { why, pending } => match (why, ev) {
            (W::OwnMode, E::Inputs) => step_of(held(W::OwnMode, true), No, Nothing),
            (W::OwnMode, E::OwnModeEntered) => step_of(state, No, Nothing),
            (W::OwnMode, E::OwnModeLeft) => step_of(S::Active, Adopt, Nothing),
            (W::OwnMode, E::UserSuspend) => step_of(held(W::Both, pending), Refold, Off),
            (W::User, E::Inputs) => step_of(state, Adopt, Nothing),
            (W::User, E::OwnModeEntered) => step_of(held(W::Both, false), No, Nothing),
            (W::User, E::OwnModeLeft) => step_of(state, No, Nothing),
            (W::User, E::UserSuspend) => step_of(S::Active, Adopt, On),
            (W::Both, E::Inputs) => step_of(held(W::Both, true), No, Nothing),
            (W::Both, E::OwnModeEntered) => step_of(state, No, Nothing),
            (W::Both, E::OwnModeLeft) => step_of(held(W::User, false), Adopt, Nothing),
            // Takes effect when the own mode is left.
            (W::Both, E::UserSuspend) => step_of(held(W::OwnMode, pending), No, On),
            (_, E::Reload) => step_of(state, Refold, Nothing),
            (_, E::SafeMode) => step_of(S::Disabled, Refold, Nothing),
        },
        S::Disabled => match ev {
            E::Inputs | E::OwnModeEntered | E::OwnModeLeft | E::UserSuspend => {
                step_of(S::Disabled, No, Nothing)
            }
            E::Reload | E::SafeMode => step_of(S::Disabled, Refold, Nothing),
        },
    }
}

/// The snapshot's layer set for a state: learn mode alone does not switch profiles off.
pub(crate) fn mode_of(state: ProfileState) -> ProfileMode {
    match state {
        ProfileState::Active
        | ProfileState::Suspended {
            why: Suspend::OwnMode,
            ..
        } => ProfileMode::Active,
        ProfileState::Suspended {
            why: Suspend::User | Suspend::Both,
            ..
        } => ProfileMode::Suspended,
        ProfileState::Disabled => ProfileMode::Disabled,
    }
}

// ---- the runtime -----------------------------------------------------------------------------

/// What the triggers look at, owned and comparable: a change is detected by `==`, so the key
/// path costs one hash lookup and one comparison per message.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct Inputs {
    app: Option<Arc<str>>,
    contexts: ContextSet,
    host: Option<Arc<str>>,
}

impl Inputs {
    fn focus(&self) -> ProfileFocus {
        ProfileFocus {
            app: self.app.clone(),
            contexts: self.contexts,
            host: self.host.clone(),
        }
    }

    fn in_document(&self) -> bool {
        self.contexts.contains(FocusContext::Document)
    }
}

/// Counters for tests and the bench: how often a key was served from the cache.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileCounters {
    pub hits: u64,
    pub folds: u64,
    pub prewarmed: u64,
    pub prewarm_dropped: u64,
    pub switches: u64,
}

/// What a pending save will say when it is done.
#[derive(Clone, Debug, Default)]
struct SaveNote {
    /// Names of the profiles that had unsaved changes.
    profiles: Vec<String>,
    /// The global settings had unsaved changes.
    global: bool,
}

#[derive(Debug)]
struct Cands {
    set: Arc<gorca_config::profiles::ProfileSet>,
    app: Arc<str>,
    list: gorca_config::profiles::AppCandidates,
}

pub(crate) struct ProfileRuntime {
    cell: Arc<ProfileCell>,
    snap: ProfileSnapshot,
    cache: EffectiveCache,
    state: ProfileState,
    /// The latest inputs of the focus.
    live: Inputs,
    /// The inputs the profile key was made from (held back while profiles are held).
    eval: Inputs,
    key: Option<ProfileKey>,
    memo: Option<(u64, ProfileKey, Arc<Settings>)>,
    cands: Option<Cands>,
    own_mode: bool,
    host: Option<Arc<str>>,
    counters: ProfileCounters,
    /// A save is in flight; edits are refused until it lands (the snapshot is completed with
    /// what was written).
    saving: Option<(SaveNote, Instant)>,
    last_target_note: Option<(String, Instant)>,
    /// Keys the last save-to-global moved, for the second press (apply everywhere).
    last_global: Option<(String, Vec<KeyRef>, Instant)>,
    prewarmed_for: Option<(u64, Inputs)>,
}

impl std::fmt::Debug for ProfileRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProfileRuntime")
            .field("state", &self.state)
            .field("generation", &self.snap.generation)
            .finish_non_exhaustive()
    }
}

impl ProfileRuntime {
    pub(crate) fn new(cell: Arc<ProfileCell>, snap: ProfileSnapshot) -> Self {
        let mut rt = ProfileRuntime {
            cell,
            snap,
            cache: EffectiveCache::new(CACHE_ENTRIES),
            state: ProfileState::Active,
            live: Inputs::default(),
            eval: Inputs::default(),
            key: None,
            memo: None,
            cands: None,
            own_mode: false,
            host: None,
            counters: ProfileCounters::default(),
            saving: None,
            last_target_note: None,
            last_global: None,
            prewarmed_for: None,
        };
        rt.publish();
        rt
    }

    /// Publish the writer's snapshot through the cell.
    fn publish(&mut self) {
        self.cell.store(self.snap.clone());
    }

    fn set_snapshot(&mut self, snap: ProfileSnapshot) {
        self.snap = snap;
        self.publish();
    }

    /// The facts of `inputs` for the triggers.
    fn make_key(&mut self, inputs: &Inputs) -> ProfileKey {
        let focus = inputs.focus();
        let facts = focus.facts();
        let Some(app) = facts.app else {
            return self.snap.focus_key(&AppCandidates::default(), &facts);
        };
        let fresh = self.cands.as_ref().is_some_and(|c| {
            Arc::ptr_eq(&c.set, &self.snap.set) && inputs.app.as_deref() == Some(&*c.app)
        });
        if !fresh && let Some(name) = &inputs.app {
            self.cands = Some(Cands {
                set: Arc::clone(&self.snap.set),
                app: Arc::clone(name),
                list: self.snap.set.for_app(&app),
            });
        }
        match &self.cands {
            Some(c) => self.snap.focus_key(&c.list, &facts),
            None => self.snap.focus_key_for(&facts),
        }
    }

    /// The effective settings of the inputs in force, from the cache when possible.
    pub(crate) fn settings(&mut self) -> Arc<Settings> {
        let inputs = self.eval.clone();
        let key = self.make_key(&inputs);
        self.key = Some(key);
        if let Some((g, k, s)) = &self.memo
            && *g == self.snap.generation
            && *k == key
        {
            return Arc::clone(s);
        }
        if self.cache.contains(&self.snap, key) {
            self.counters.hits += 1;
        } else {
            self.counters.folds += 1;
        }
        let eff = self.cache.get_or_fold(&self.snap, key);
        let s = Arc::new(eff.settings.clone());
        self.memo = Some((self.snap.generation, key, Arc::clone(&s)));
        s
    }

    fn app_key(&self) -> Option<gorca_config::profiles::AppKey> {
        self.eval
            .app
            .as_deref()
            .map(gorca_config::profiles::AppKey::of)
    }

    pub(crate) fn counters(&self) -> ProfileCounters {
        self.counters
    }

    /// Is a save in flight? A save nobody answered within [`SAVE_TIMEOUT`] is forgotten.
    fn busy(&mut self, now: Instant) -> bool {
        match &self.saving {
            Some((_, at)) if now.saturating_duration_since(*at) < SAVE_TIMEOUT => true,
            Some(_) => {
                self.saving = None;
                false
            }
            None => false,
        }
    }
}

// ---- prewarm (SW-2) -------------------------------------------------------------------------

/// Fold the keys one step from `req.focus`: the focus as it is, the document modes, no context,
/// and every context some profile names. Pure; the config task runs it and answers with
/// `SettingsMsg::Prewarm`.
pub fn prewarm(req: &PrewarmRequest) -> PrewarmEntries {
    let snap = &req.snapshot;
    let base = &req.focus;
    let mut sets: Vec<ContextSet> = vec![base.contexts, ContextSet::EMPTY];
    let doc = ContextSet::EMPTY.with(FocusContext::Document);
    sets.push(doc.with(FocusContext::BrowseMode));
    sets.push(doc.with(FocusContext::FocusMode));
    for c in snap.set.named_contexts().iter() {
        sets.push(base.contexts.with(c));
    }
    sets.dedup();
    let mut entries = Vec::new();
    let mut seen = Vec::new();
    for contexts in sets {
        let focus = ProfileFocus {
            app: base.app.clone(),
            contexts,
            host: base.host.clone(),
        };
        let key = snap.focus_key_for(&focus.facts());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);
        entries.push((key, Arc::new(snap.resolve_focus(key))));
    }
    PrewarmEntries {
        generation: snap.generation,
        entries,
    }
}

// ---- edits ------------------------------------------------------------------------------------

/// A change a command makes to a setting.
#[derive(Clone, Debug)]
pub(crate) enum Change {
    Rate(u8),
    Pitch(u8),
    Volume(u8),
    TypedChars(bool),
    TypedWords(bool),
    CommandKeys(bool),
    Verbosity(Verbosity),
    Punctuation(Punct),
    ModeDefault(ModeDefault),
}

impl Change {
    fn key(&self) -> (&'static str, &'static str) {
        match self {
            Change::Rate(_) => ("speech", "rate"),
            Change::Pitch(_) => ("speech", "pitch"),
            Change::Volume(_) => ("speech", "volume"),
            Change::TypedChars(_) => ("echo", "typed_characters"),
            Change::TypedWords(_) => ("echo", "typed_words"),
            Change::CommandKeys(_) => ("echo", "command_keys"),
            Change::Verbosity(_) => ("verbosity", "level"),
            Change::Punctuation(_) => ("speech", "punctuation"),
            Change::ModeDefault(_) => ("browse", "mode_default"),
        }
    }

    fn value(&self) -> Value {
        match self {
            Change::Rate(v) | Change::Pitch(v) | Change::Volume(v) => Value::Int(i64::from(*v)),
            Change::TypedChars(b) | Change::TypedWords(b) | Change::CommandKeys(b) => {
                Value::Bool(*b)
            }
            Change::Verbosity(v) => Value::Str(v.name().to_owned()),
            Change::Punctuation(p) => Value::Str(p.name().to_owned()),
            Change::ModeDefault(m) => Value::Str(m.name().to_owned()),
        }
    }

    /// The overlay route, for a Reader without profiles. `false`: no overlay entry exists.
    fn overlay(&self, o: &mut Overlay) -> bool {
        match self {
            Change::Rate(v) => o.rate = Some(*v),
            Change::Pitch(v) => o.pitch = Some(*v),
            Change::Volume(v) => o.volume = Some(*v),
            Change::TypedChars(b) => o.typed_characters = Some(*b),
            Change::TypedWords(b) => o.typed_words = Some(*b),
            Change::CommandKeys(b) => o.command_keys = Some(*b),
            Change::Verbosity(v) => o.verbosity = Some(*v),
            Change::Punctuation(p) => o.punctuation = Some(*p),
            Change::ModeDefault(_) => return false,
        }
        true
    }
}

impl Reader {
    /// The profile runtime's counters (tests, bench).
    pub fn profile_counters(&self) -> Option<ProfileCounters> {
        self.cmd.profiles.as_ref().map(ProfileRuntime::counters)
    }

    /// The writer's snapshot (tests, `settings.explain_current` later).
    pub fn profile_snapshot(&self) -> Option<Arc<ProfileSnapshot>> {
        self.cmd.profile_cell.as_ref().map(|c| c.load())
    }

    /// Publish the profile snapshot through `cell` (the doc registry and the other resolvers
    /// read it, SS-1).
    #[must_use]
    pub fn with_profile_cell(mut self, cell: Arc<ProfileCell>) -> Self {
        self.cmd.profile_cell = Some(cell);
        self
    }

    /// The host of the focused document, from the document's address (ADR-0016 TR-1, slice P7
    /// feeds it). Evaluated with the next message.
    pub fn set_document_host(&mut self, host: Option<Arc<str>>) {
        if let Some(rt) = self.cmd.profiles.as_mut() {
            rt.host = host;
        }
    }

    /// Safe mode (M11): only L0, L1, product rows and L5 apply.
    pub fn set_profiles_safe_mode(&mut self, fx: &mut ReaderEffects) {
        self.profile_event(ProfileEvent::SafeMode, fx);
    }

    /// The snapshot a Reader that has no profile directory still resolves through.
    pub(crate) fn profile_cell(&mut self) -> Arc<ProfileCell> {
        Arc::clone(
            self.cmd
                .profile_cell
                .get_or_insert_with(|| Arc::new(ProfileCell::new(initial_snapshot()))),
        )
    }

    /// A load delivered profile data: create the runtime, or replace the compiled set (HR-4 per
    /// layer, unsaved edits of changed keys dropped).
    pub(crate) fn profiles_loaded(
        &mut self,
        layers: &gorca_config::profiles::BuiltLayers,
        cause: super::state::LoadCause,
        fx: &mut ReaderEffects,
    ) {
        use super::state::LoadCause as C;
        let set = Arc::clone(&layers.set);
        let base = Arc::clone(&layers.base);
        if let Some(rt) = self.cmd.profiles.as_mut() {
            match cause {
                // Same files, new grab keysyms: nothing to recompile.
                C::Layouts => return,
                C::Explicit => {
                    let reverted = rt.snap.revert();
                    rt.snap = reverted;
                }
                C::Startup | C::FileChanged | C::Saved => {}
            }
            let next = rt.snap.reload(set, base);
            rt.set_snapshot(next);
            rt.saving = None;
            rt.cache = EffectiveCache::new(CACHE_ENTRIES);
            rt.memo = None;
            rt.cands = None;
            self.profile_event(ProfileEvent::Reload, fx);
            return;
        }
        let cell = self.profile_cell();
        let snap = ProfileSnapshot::new(set, base);
        let mut rt = ProfileRuntime::new(cell, snap);
        rt.live = self.profile_live_inputs(rt.host.clone());
        rt.eval = rt.live.clone();
        rt.own_mode = matches!(self.mode, Mode::InputHelp { .. });
        self.cmd.profiles = Some(rt);
    }

    /// The inputs of the focus right now.
    fn profile_live_inputs(&self, host: Option<Arc<str>>) -> Inputs {
        let app = self.current_node().and_then(|n| self.dir.app_name(n.app));
        let contexts = match self.web.as_ref().map(|b| b.mode) {
            Some(WebMode::Browse { .. }) => ContextSet::EMPTY
                .with(FocusContext::Document)
                .with(FocusContext::BrowseMode),
            Some(WebMode::Focus { .. }) => ContextSet::EMPTY
                .with(FocusContext::Document)
                .with(FocusContext::FocusMode),
            Some(WebMode::Inactive | WebMode::Loading { .. }) | None => ContextSet::EMPTY,
        };
        Inputs {
            app,
            contexts,
            host,
        }
    }

    /// After every message: did the focus, the document or the mode change what the triggers
    /// see? One hash lookup and one comparison when nothing changed.
    pub(crate) fn profiles_after(&mut self, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_ref() else {
            return;
        };
        let own = matches!(self.mode, Mode::InputHelp { .. });
        let live = self.profile_live_inputs(rt.host.clone());
        let own_changed = own != rt.own_mode;
        let inputs_changed = live != rt.live;
        if !own_changed && !inputs_changed {
            return;
        }
        let mut events: [Option<ProfileEvent>; 2] = [None, None];
        if let Some(rt) = self.cmd.profiles.as_mut() {
            if own_changed {
                rt.own_mode = own;
                events[0] = Some(if own {
                    ProfileEvent::OwnModeEntered
                } else {
                    ProfileEvent::OwnModeLeft
                });
            }
            if inputs_changed {
                rt.live = live;
                events[1] = Some(ProfileEvent::Inputs);
            }
        }
        for ev in events.into_iter().flatten() {
            self.profile_event(ev, fx);
        }
    }

    /// Run one event through the table and apply the result. Voice parameters go to the speech
    /// lane **before** anything the message already spoke (SW-3).
    fn profile_event(&mut self, ev: ProfileEvent, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            return;
        };
        let st = step(rt.state, ev);
        rt.state = st.state;
        let want = mode_of(st.state);
        if rt.snap.mode != want {
            let next = rt.snap.with_mode(want);
            rt.set_snapshot(next);
        }
        match st.eval {
            Eval::Adopt => rt.eval = rt.live.clone(),
            Eval::No | Eval::Refold => {}
        }
        if st.eval != Eval::No {
            self.profile_switched(fx);
        }
        match st.say {
            Say::Nothing => {}
            Say::Off => self.say(t(M::ProfilesOff), fx),
            Say::On => self.say(t(M::ProfilesOn), fx),
        }
    }

    /// The inputs or the layers changed: resolve, publish the settings and order the voice.
    fn profile_switched(&mut self, fx: &mut ReaderEffects) {
        let before = Arc::clone(&self.cmd.effective);
        self.cmd.republish();
        if let Some(rt) = self.cmd.profiles.as_mut() {
            rt.counters.switches += 1;
        }
        let mut voice = ReaderEffects::new();
        self.send_levels(&mut voice);
        if !voice.is_empty() {
            for (i, e) in voice.into_iter().enumerate() {
                fx.insert(i, e);
            }
        }
        if before.speech.language != self.cmd.effective.speech.language {
            gorca_i18n::set_language(gorca_i18n::Lang::from_code(
                &self.cmd.effective.speech.language,
            ));
        }
        self.profile_request_prewarm(fx);
    }

    /// Ask the config task to fold the keys one step away (once per generation and focus).
    fn profile_request_prewarm(&mut self, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            return;
        };
        let tag = (rt.snap.generation, rt.eval.clone());
        if rt.prewarmed_for.as_ref() == Some(&tag) {
            return;
        }
        let focus = rt.eval.focus();
        rt.prewarmed_for = Some(tag);
        fx.push(ReaderEffect::Config(ConfigRequest::Prewarm(Box::new(
            PrewarmRequest {
                snapshot: rt.cell.load(),
                focus,
            },
        ))));
    }

    /// The config task's folded keys (SW-2): dropped when they were folded from an older
    /// generation.
    pub(crate) fn profiles_prewarmed(&mut self, p: &PrewarmEntries) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            return;
        };
        for (key, eff) in &p.entries {
            if p.generation == rt.snap.generation
                && key.epoch == rt.snap.interner().epoch()
                && rt.cache.insert_prewarmed(&rt.snap, *key, Arc::clone(eff))
            {
                rt.counters.prewarmed += 1;
            } else {
                rt.counters.prewarm_dropped += 1;
            }
        }
    }

    // ---- edits ---------------------------------------------------------------------------------

    /// Apply a command's change where it belongs (PE-1) and return what to say about where it
    /// went ("for Firefox", "globally"; empty when it was said recently). `None`: refused, and
    /// the refusal was spoken.
    pub(crate) fn apply_change(&mut self, c: &Change, fx: &mut ReaderEffects) -> Option<String> {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            if c.overlay(&mut self.cmd.overlay) {
                return Some(String::new());
            }
            self.say(t(M::CommandNotAvailable), fx);
            return None;
        };
        if rt.busy(self.now) {
            self.say(t(M::ProfileSaveBusy), fx);
            return None;
        }
        let Some(path) = KeyId::of(c.key().0, c.key().1) else {
            return Some(String::new());
        };
        let edit = Edit {
            key: KeyRef::Core(path),
            value: c.value(),
        };
        let focus = rt.eval.focus();
        let facts = focus.facts();
        let plan = match rt.snap.plan_edit(&facts, edit) {
            Ok(p) => p,
            Err(e) => {
                let text = match e {
                    EditRefusal::VolumeFloor => msg!(ProfileVolumeFloor, min = 20u32),
                    EditRefusal::UnknownKey
                    | EditRefusal::WrongValue
                    | EditRefusal::NotAllowedHere(_)
                    | EditRefusal::UnusableName => t(M::ProfileEditRefused).to_owned(),
                };
                self.say(&text, fx);
                return None;
            }
        };
        let note = target_note(rt, &plan);
        let applied = rt.snap.apply_edit(&facts, &plan);
        let (changed, held) = (applied.changed, applied.held_by.is_some());
        rt.set_snapshot(applied.snapshot);
        let mut said = String::new();
        if let Some(name) = &plan.announce_new {
            said = msg!(ProfileNew, name = name.as_str());
        }
        let now = Instant::now();
        let repeat = rt
            .last_target_note
            .as_ref()
            .is_some_and(|(n, at)| *n == note && now.saturating_duration_since(*at) < BURST);
        rt.last_target_note = Some((note.clone(), now));
        if !changed && held {
            // PE-3: the value was set, but another layer keeps its own.
            let app = rt.eval.app.as_deref().unwrap_or_default().to_owned();
            if !said.is_empty() {
                said.push_str(". ");
            }
            said.push_str(&note);
            said.push_str("; ");
            said.push_str(&msg!(ProfileKeeps, name = app.as_str()));
            return Some(said);
        }
        if !repeat || !said.is_empty() {
            if !said.is_empty() {
                said.push_str(". ");
            }
            said.push_str(&note);
        }
        Some(said)
    }

    // ---- save ----------------------------------------------------------------------------------

    /// `settings.save` with profiles: every dirty delta to its own file.
    pub(crate) fn profiles_save(&mut self, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            return;
        };
        if rt.busy(self.now) {
            self.say(t(M::ProfileSaveBusy), fx);
            return;
        }
        let d = &rt.snap.deltas;
        let mut note = SaveNote {
            global: !d.base.is_empty(),
            profiles: Vec::new(),
        };
        for (id, delta) in &d.profiles {
            if !delta.is_empty() || d.created.contains_key(id) {
                note.profiles.push(profile_name(&rt.snap, id));
            }
        }
        for id in d.created.keys() {
            let n = profile_name(&rt.snap, id);
            if !note.profiles.contains(&n) {
                note.profiles.push(n);
            }
        }
        if !note.global && note.profiles.is_empty() {
            self.say(t(M::ProfileNothingToSave), fx);
            return;
        }
        rt.saving = Some((note, self.now));
        let req = SaveProfiles {
            snapshot: rt.cell.load(),
            kind: SaveKind::All,
        };
        fx.push(ReaderEffect::Config(ConfigRequest::SaveProfiles(Box::new(
            req,
        ))));
    }

    /// `profiles.save_global`: the touched keys of this application's profile move to the global
    /// file. Pressed twice, they also leave every other profile.
    pub(crate) fn profiles_save_global(&mut self, rep: u8, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        };
        if rt.busy(self.now) {
            self.say(t(M::ProfileSaveBusy), fx);
            return;
        }
        let now = Instant::now();
        if rep >= 2
            && let Some((except, keys, at)) = rt.last_global.take()
            && now.saturating_duration_since(at) < EVERYWHERE_WINDOW
        {
            rt.saving = Some((SaveNote::default(), self.now));
            let req = SaveProfiles {
                snapshot: rt.cell.load(),
                kind: SaveKind::Everywhere { keys, except },
            };
            fx.push(ReaderEffect::Config(ConfigRequest::SaveProfiles(Box::new(
                req,
            ))));
            return;
        }
        let Some(profile_id) = rt.edit_profile_id() else {
            // No application, or an application with no profile: nothing it could move.
            let text = match rt.eval.app.as_deref() {
                Some(app) => msg!(ProfileNothingChanged, name = app),
                None => t(M::ProfileNoApplication).to_owned(),
            };
            self.say(&text, fx);
            return;
        };
        let focus = rt.eval.focus();
        rt.saving = Some((SaveNote::default(), self.now));
        let req = SaveProfiles {
            snapshot: rt.cell.load(),
            kind: SaveKind::Global { profile_id, focus },
        };
        fx.push(ReaderEffect::Config(ConfigRequest::SaveProfiles(Box::new(
            req,
        ))));
    }

    /// The config task's answer to a profile save.
    pub(crate) fn profiles_saved(&mut self, r: &ProfilesSaved, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            return;
        };
        let note = rt.saving.take().map(|(n, _)| n).unwrap_or_default();
        let done_count = match &r.outcome {
            ProfileSaveOutcome::Written { done, .. } | ProfileSaveOutcome::Failed { done } => *done,
            ProfileSaveOutcome::Refused(_) | ProfileSaveOutcome::NewerSchema => 0,
        };
        let mut moved: Vec<KeyRef> = Vec::new();
        if let Some(plan) = &r.plan {
            if done_count > 0 {
                let next = rt.snap.complete(plan, done_count);
                rt.set_snapshot(next);
            }
            moved.extend(plan.report.copied.iter().cloned());
            moved.extend(plan.report.already_global.iter().cloned());
        }
        let text = match &r.outcome {
            ProfileSaveOutcome::Written { done, total } if done >= total => {
                saved_text(rt, &r.kind, &note, r.plan.as_deref(), &moved)
            }
            ProfileSaveOutcome::Written { .. } | ProfileSaveOutcome::Failed { .. } => {
                match &r.kind {
                    SaveKind::Global { profile_id, .. } if done_count > 0 => {
                        let name = profile_name(&rt.snap, profile_id);
                        msg!(ProfileSavedPartial, name = name.as_str())
                    }
                    SaveKind::All | SaveKind::Global { .. } | SaveKind::Everywhere { .. } => {
                        t(M::SettingsSaveFailed).to_owned()
                    }
                }
            }
            ProfileSaveOutcome::NewerSchema => t(M::SettingsNotSavedNewer).to_owned(),
            ProfileSaveOutcome::Refused(why) => match why {
                SaveRefusal::NothingChanged(name) => {
                    msg!(ProfileNothingChanged, name = name.as_str())
                }
                SaveRefusal::ChangedOnDisk(gorca_config::profiles::FileId::Global) => {
                    t(M::SettingsNotSavedChanged).to_owned()
                }
                SaveRefusal::ChangedOnDisk(gorca_config::profiles::FileId::Profile(id)) => {
                    let name = profile_name(&rt.snap, id);
                    msg!(ProfileChangedOnDisk, name = name.as_str())
                }
                SaveRefusal::NothingToSave => t(M::ProfileNothingToSave).to_owned(),
                SaveRefusal::UnknownProfile(_)
                | SaveRefusal::MissingOnDisk(_)
                | SaveRefusal::Text(_) => t(M::SettingsSaveFailed).to_owned(),
            },
        };
        if matches!(&r.outcome, ProfileSaveOutcome::Written { done, total } if done >= total)
            && let SaveKind::Global { profile_id, .. } = &r.kind
            && !moved.is_empty()
        {
            rt.last_global = Some((profile_id.clone(), moved, Instant::now()));
        }
        self.cmd.republish();
        let mut voice = ReaderEffects::new();
        self.send_levels(&mut voice);
        fx.extend(voice);
        self.say(&text, fx);
    }

    // ---- commands ------------------------------------------------------------------------------

    fn profile_app_name(&self) -> Option<Arc<str>> {
        self.cmd
            .profiles
            .as_ref()
            .and_then(|rt| rt.eval.app.clone())
    }

    /// `profiles.cycle`: Automatic, each profile of this application, "global only".
    pub(crate) fn profiles_cycle(&mut self, fx: &mut ReaderEffects) {
        let Some(app) = self.profile_app_name() else {
            self.say(t(M::ProfileNoApplication), fx);
            return;
        };
        let Some(rt) = self.cmd.profiles.as_mut() else {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        };
        let key = gorca_config::profiles::AppKey::of(&app);
        let facts = AppFacts {
            name: &app,
            exe: None,
            desktop_id: None,
        };
        let mut options: Vec<Option<Pin>> = vec![None];
        let mut ids: Vec<String> = rt
            .snap
            .set
            .app_profiles(&facts)
            .into_iter()
            .filter_map(|i| rt.snap.set.get(i).map(|p| p.id.clone()))
            .collect();
        for (_, p) in rt.snap.set.iter() {
            if p.source == ProfileSource::User && p.manual && !ids.contains(&p.id) {
                ids.push(p.id.clone());
            }
        }
        ids.sort();
        options.extend(ids.into_iter().map(|id| Some(Pin::Profile(id))));
        options.push(Some(Pin::GlobalOnly));
        let current = rt.snap.pin_of(&key).cloned();
        let at = options
            .iter()
            .position(|o| *o == current)
            .map_or(0, |i| (i + 1) % options.len());
        let next = options[at].clone();
        let text = match &next {
            None => msg!(ProfilePinAuto, name = &*app),
            Some(Pin::GlobalOnly) => msg!(ProfilePinGlobal, name = &*app),
            Some(Pin::Profile(id)) => {
                let p = profile_name(&rt.snap, id);
                msg!(ProfilePinProfile, profile = p.as_str(), name = &*app)
            }
        };
        let snap = rt.snap.with_pin(&key, next);
        rt.set_snapshot(snap);
        self.profile_switched(fx);
        self.say(&text, fx);
    }

    /// `profiles.toggle_enabled`: the profile in effect for this application, on or off.
    pub(crate) fn profiles_toggle_enabled(&mut self, fx: &mut ReaderEffects) {
        let Some(app) = self.profile_app_name() else {
            self.say(t(M::ProfileNoApplication), fx);
            return;
        };
        let Some(rt) = self.cmd.profiles.as_mut() else {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        };
        let Some(id) = rt.effective_profile_id() else {
            self.say(&msg!(ProfileNoneToToggle, name = &*app), fx);
            return;
        };
        let Some(next) = rt.snap.toggle_enabled(&id) else {
            self.say(t(M::SettingsSaveFailed), fx);
            return;
        };
        let name = profile_name(&next, &id);
        let on = next
            .set
            .id_of(&id)
            .and_then(|i| next.set.get(i))
            .is_some_and(|p| next.is_enabled(p));
        rt.set_snapshot(next);
        self.profile_switched(fx);
        let text = if on {
            msg!(ProfileEnabled, profile = name.as_str())
        } else {
            msg!(ProfileDisabled, profile = name.as_str())
        };
        self.say(&text, fx);
    }

    /// `profiles.edit_target_cycle`: this application, this site (in a document), global.
    pub(crate) fn profiles_edit_target_cycle(&mut self, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        };
        let Some(key) = rt.app_key() else {
            self.say(t(M::ProfileNoApplication), fx);
            return;
        };
        let in_doc = rt.eval.in_document() && rt.eval.host.is_some();
        let next = rt.snap.target_of(&key).next(in_doc);
        let snap = rt.snap.with_target(&key, next);
        rt.set_snapshot(snap);
        let text = target_phrase(rt, next);
        self.say(&text, fx);
    }

    /// `profiles.announce`: the profile in effect and where edits go; twice: the whole stack.
    pub(crate) fn profiles_announce(&mut self, rep: u8, fx: &mut ReaderEffects) {
        let Some(rt) = self.cmd.profiles.as_mut() else {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        };
        let Some(app) = rt.eval.app.clone() else {
            self.say(t(M::ProfileNoApplication), fx);
            return;
        };
        let Some(key) = rt.app_key() else {
            return;
        };
        let target = target_phrase(rt, rt.snap.target_of(&key));
        let profile = rt
            .effective_profile_id()
            .map(|id| profile_name(&rt.snap, &id));
        let text = if rep >= 2 {
            let stack = rt.stack_names();
            if stack.is_empty() {
                msg!(ProfileStackNone, name = &*app)
            } else {
                msg!(ProfileStack, list = stack.join(", ").as_str())
            }
        } else {
            match profile {
                Some(p) => msg!(
                    ProfileAnnounce,
                    profile = p.as_str(),
                    target = target.as_str()
                ),
                None => msg!(ProfileAnnounceNone, name = &*app, target = target.as_str()),
            }
        };
        self.say(&text, fx);
    }

    /// `profiles.suspend`.
    pub(crate) fn profiles_suspend(&mut self, fx: &mut ReaderEffects) {
        if self.cmd.profiles.is_none() {
            self.say(t(M::CommandNotAvailable), fx);
            return;
        }
        self.profile_event(ProfileEvent::UserSuspend, fx);
    }

    /// `browse.save_mode_default`: the mode this document is in becomes the default for the
    /// application (or the site, with that edit target).
    pub(crate) fn browse_save_mode_default(&mut self, fx: &mut ReaderEffects) {
        let mode = match self.web.as_ref().map(|b| b.mode) {
            Some(WebMode::Browse { .. }) => ModeDefault::Browse,
            Some(WebMode::Focus { .. }) => ModeDefault::Focus,
            Some(WebMode::Inactive | WebMode::Loading { .. }) | None => {
                self.say(t(M::ProfileNoDocument), fx);
                return;
            }
        };
        let Some(note) = self.apply_change(&Change::ModeDefault(mode), fx) else {
            return;
        };
        self.cmd.republish();
        let name = match mode {
            ModeDefault::Browse => t(M::WebModeBrowse),
            ModeDefault::Focus => t(M::WebModeFocus),
        };
        let mut text = msg!(ProfileModeSaved, mode = name);
        if !note.is_empty() {
            text.push_str(", ");
            text.push_str(&note);
        }
        self.say(&text, fx);
    }
}

// ---- helpers ----------------------------------------------------------------------------------

fn profile_name(snap: &ProfileSnapshot, id: &str) -> String {
    snap.set
        .id_of(id)
        .and_then(|i| snap.set.get(i))
        .map_or_else(|| id.to_owned(), |p| p.name.clone())
}

/// "for Firefox" or "globally": where an edit went (PE-3).
fn target_note(rt: &ProfileRuntime, plan: &EditPlan) -> String {
    let name = match &plan.target {
        gorca_config::profiles::Target::Global => None,
        gorca_config::profiles::Target::Profile { id, create } => Some(
            create
                .as_ref()
                .map_or_else(|| profile_name(&rt.snap, id), |p| p.name.clone()),
        ),
    };
    match (name, plan.why) {
        (Some(n), _) => msg!(ProfileFor, name = n.as_str()),
        (
            None,
            TargetWhy::BaseKey
            | TargetWhy::PrivacyKey
            | TargetWhy::ExplicitGlobal
            | TargetWhy::NoApplication
            | TargetWhy::Pinned
            | TargetWhy::AppProfile
            | TargetWhy::NewAppProfile
            | TargetWhy::MoreSpecificSupplier
            | TargetWhy::SiteProfile
            | TargetWhy::NewSiteProfile,
        ) => t(M::ProfileGlobally).to_owned(),
    }
}

/// Where edits go, as a phrase: "to the Firefox profile".
fn target_phrase(rt: &ProfileRuntime, kind: EditTargetKind) -> String {
    let app = rt.eval.app.as_deref().unwrap_or_default();
    match kind {
        EditTargetKind::App => msg!(ProfileToApp, name = app),
        EditTargetKind::Site => {
            msg!(
                ProfileToSite,
                name = rt.eval.host.as_deref().unwrap_or_default()
            )
        }
        EditTargetKind::Global => t(M::ProfileToGlobal).to_owned(),
    }
}

fn saved_text(
    rt: &ProfileRuntime,
    kind: &SaveKind,
    note: &SaveNote,
    plan: Option<&gorca_config::profiles::SavePlan>,
    moved: &[KeyRef],
) -> String {
    match kind {
        SaveKind::All => {
            let mut parts: Vec<String> = note.profiles.clone();
            if note.global {
                parts.push(t(M::ProfileGlobalSettings).to_owned());
            }
            if parts.len() == 1 && note.global {
                t(M::ProfileSavedGlobal).to_owned()
            } else {
                msg!(ProfileSavedFor, names = parts.join(", ").as_str())
            }
        }
        SaveKind::Global { profile_id, .. } => {
            let name = profile_name(&rt.snap, profile_id);
            let n = plan.map_or(moved.len(), |p| p.report.copied.len());
            let already = plan.map_or(0, |p| p.report.already_global.len());
            if n == 0 && already > 0 {
                msg!(ProfileAlreadyGlobal, n = already, name = name.as_str())
            } else {
                msg!(ProfileSavedGlobalN, n = n, name = name.as_str())
            }
        }
        SaveKind::Everywhere { .. } => {
            let n = plan.map_or(0, |p| p.report.other_profiles_changed.len());
            msg!(ProfileEverywhere, n = n)
        }
    }
}

impl ProfileRuntime {
    /// The user profile an edit of this application lands in, for save-to-global: the pinned
    /// profile, else the most specific matching profile that holds touched keys, else the most
    /// specific matching one (PE-1).
    fn edit_profile_id(&mut self) -> Option<String> {
        let key = self.app_key()?;
        if let Some(Pin::Profile(id)) = self.snap.pin_of(&key) {
            return Some(id.clone());
        }
        let inputs = self.eval.clone();
        let pk = self.make_key(&inputs);
        let user = self.snap.interner().set(pk.user);
        let touched = &self.snap.deltas.touched;
        user.iter()
            .rev()
            .filter_map(|i| self.snap.set.get(*i))
            .find(|p| touched.get(&p.id).is_some_and(|t| !t.is_empty()))
            .or_else(|| user.last().and_then(|i| self.snap.set.get(*i)))
            .map(|p| p.id.clone())
    }

    /// The profile currently in effect for the application: pinned, else the top matching user
    /// profile, else the top row.
    fn effective_profile_id(&mut self) -> Option<String> {
        let inputs = self.eval.clone();
        let key = self.make_key(&inputs);
        let user = self.snap.interner().set(key.user);
        let rows = self.snap.interner().set(key.rows);
        let id = user.last().or_else(|| rows.last())?;
        self.snap.set.get(*id).map(|p| p.id.clone())
    }

    /// Names of the rows and profiles in effect, lowest layer first.
    fn stack_names(&mut self) -> Vec<String> {
        let inputs = self.eval.clone();
        let key = self.make_key(&inputs);
        let interner = Arc::clone(self.snap.interner());
        let mut out = Vec::new();
        for id in interner
            .set(key.rows)
            .iter()
            .chain(interner.set(key.user).iter())
        {
            if let Some(p) = self.snap.set.get(*id) {
                out.push(p.name.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ProfileEvent as E;
    use ProfileState as S;
    use Suspend as W;

    fn all_states() -> Vec<ProfileState> {
        let mut v = vec![S::Active, S::Disabled];
        for why in [W::OwnMode, W::User, W::Both] {
            for pending in [false, true] {
                v.push(S::Suspended { why, pending });
            }
        }
        v
    }

    const EVENTS: [ProfileEvent; 6] = [
        E::Inputs,
        E::OwnModeEntered,
        E::OwnModeLeft,
        E::UserSuspend,
        E::Reload,
        E::SafeMode,
    ];

    #[test]
    fn every_state_and_event_has_a_transition_and_safe_mode_is_absorbing() {
        for s in all_states() {
            for e in EVENTS {
                let t = step(s, e);
                if e == E::SafeMode {
                    assert_eq!(t.state, S::Disabled, "{s:?}");
                }
                if s == S::Disabled {
                    assert_eq!(t.state, S::Disabled, "{e:?}");
                    assert_eq!(t.say, Say::Nothing);
                }
                // A reload always re-resolves.
                if e == E::Reload && s != S::Disabled {
                    assert_ne!(t.eval, Eval::No, "{s:?}");
                }
                // The held inputs are adopted only by states that are not held by an own mode.
                if t.eval == Eval::Adopt && e == E::Inputs {
                    assert!(
                        !matches!(
                            s,
                            S::Suspended {
                                why: W::OwnMode | W::Both,
                                ..
                            }
                        ),
                        "{s:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_table_matches_section_2_4() {
        let own = S::Suspended {
            why: W::OwnMode,
            pending: false,
        };
        let user = S::Suspended {
            why: W::User,
            pending: false,
        };
        let both = S::Suspended {
            why: W::Both,
            pending: false,
        };
        assert_eq!(step(S::Active, E::Inputs).eval, Eval::Adopt);
        assert_eq!(step(S::Active, E::OwnModeEntered).state, own);
        assert_eq!(step(S::Active, E::UserSuspend).state, user);
        assert_eq!(step(S::Active, E::UserSuspend).say, Say::Off);
        // While in an own mode the latest inputs are remembered, then evaluated once on leaving.
        let held = step(own, E::Inputs);
        assert_eq!(
            held.state,
            S::Suspended {
                why: W::OwnMode,
                pending: true
            }
        );
        assert_eq!(held.eval, Eval::No);
        let left = step(held.state, E::OwnModeLeft);
        assert_eq!((left.state, left.eval), (S::Active, Eval::Adopt));
        // `profiles.suspend` keeps the key current for explain.
        assert_eq!(step(user, E::Inputs).eval, Eval::Adopt);
        assert_eq!(step(user, E::OwnModeEntered).state, both);
        let back = step(user, E::UserSuspend);
        assert_eq!((back.state, back.say), (S::Active, Say::On));
        assert_eq!(step(both, E::OwnModeLeft).state, user);
        let resumed = step(both, E::UserSuspend);
        assert_eq!((resumed.state, resumed.say), (own, Say::On));
        assert_eq!(resumed.eval, Eval::No, "takes effect when the mode is left");
        assert_eq!(step(S::Active, E::SafeMode).state, S::Disabled);
    }

    #[test]
    fn layer_sets_per_state() {
        assert_eq!(mode_of(S::Active), ProfileMode::Active);
        for pending in [false, true] {
            assert_eq!(
                mode_of(S::Suspended {
                    why: W::OwnMode,
                    pending
                }),
                ProfileMode::Active,
                "an own mode alone does not switch profiles off"
            );
            for why in [W::User, W::Both] {
                assert_eq!(
                    mode_of(S::Suspended { why, pending }),
                    ProfileMode::Suspended
                );
            }
        }
        assert_eq!(mode_of(S::Disabled), ProfileMode::Disabled);
    }
}
