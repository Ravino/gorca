//! gorca core: Reader, app actors, doc actors, Focus/Mode/Health state machines, presenter.
//!
//! Responsibility (ADR-0001 sections 3, 6, 7 and 11): "Reader, app actors, doc actors,
//! Focus/Mode SMs, presenter, `PresentJob`". The core is **connector-neutral and
//! backend-neutral**: it knows only the `-api` crates.
//!
//! > "The core never depends on connector crates. ... `gorca-core` must not depend on any
//! > `gorca-connector-*`, `gorca-speech-*`, `gorca-input-*` (except `-api`) or `gorca-ext-host`.
//! > Only `gorca-daemon` wires concrete implementations."  (section 11, rule 3)
//! > "Reader: ... never awaits. It reads mirrors lock-free and sends queries as messages. Its
//! > per-message work is bounded and O(1) or O(log n)."  (section 2, item 5)
//!
//! State machines are `enum`s with an exhaustive `match` and no wildcard arm, each a pure
//! `step(self, msg, ..) -> (Self, effects)` (section 7). They are unit-tested table by table.
//!
//! Phase 0 contents: [`FocusState`], [`Mode`], [`AppHealth`] with their tables; the hot-path
//! presenter; the [`Reader`] message handler with a biased [`ReaderInbox`]; [`AppActor`] and
//! [`DocActor`] pump functions; the lock-free [`MirrorDirectory`]. TBD (later phases): the lane
//! threads and their wake-up mechanism (bake-off B-5, MB-2), extension hook dispatch, the full
//! mode table (`docs/design/modes.md`, open item), `PresentJob` execution on `cpu`, the watchdog
//! wiring, and the supervisor restart loop.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-ingress`, `gorca-mirror`,
//! `gorca-connector-api`, `gorca-doc-source-api`, `gorca-doc`, `gorca-output-api`,
//! `gorca-input-api`, `gorca-speech`. Forbidden, even transitively: `atspi*`, `zbus*`,
//! `accesskit*`, `wayland-*`, `wasmtime*`, libei bindings.
#![deny(clippy::wildcard_enum_match_arm)]

mod app_actor;
mod aria;
mod browse;
mod builtin_ext;
mod commands;
mod directory;
mod doc_actor;
mod doc_registry;
mod ext;
mod focus;
mod health;
mod mode;
mod present;
mod quick;
mod reader;
mod review;
mod runtime;
#[cfg(test)]
mod statemodel;
mod terminal;
mod text;

pub use app_actor::{
    AppActor, AppRoute, FILL_QUEUE, FillMsg, ReaderLinks, TextLink, WebLink, health_view,
};
pub use browse::commands::WebEffect;
pub use browse::{
    BrowseCounters, BrowseSettings, FocusCause, ModeWord, Scope, WebEv, WebFx, WebMode,
};
pub use builtin_ext::AppProfile;
pub use builtin_ext::{AppPolicy, PolicyFacts, PolicyMoment, PolicyVerdict};
pub use commands::{
    Availability, CmdSpec, CommandTable, Compiled, ConfigRequest, ContextMask, ControlMsg, CoreCmd,
    InputNotice, LoadCause, LoadedSettings, LocalTime, Milestone, PASS_TIMEOUT, PlatformFacts,
    SaveResult, SettingsMsg, SpeechCtl, SystemWallClock, VoiceLevels, WallClock, browse_grabs,
    chord_grabs, command_id, compile, compile_with, escape_grabs, gorca_modifiers, grab_collisions,
    lane_settings, letter_grabs, tab_grabs,
};
pub use directory::{MirrorDirectory, Owner};
pub use doc_actor::{
    DocActorSpec, DocCounters, DocCtl, DocDirectory, DocHandle, DocNotice, WebJoin,
    prepare as prepare_doc_actor,
};
pub use doc_registry::{BUILD_HOLD, HubCfg, HubMsg, HubTx, WarmState, hub_channel, run_hub};
pub use ext::{
    AppExt, EXT_CMD_BASE, ExtCmdError, ExtCmdSpec, ExtCommands, ExtSink, ExtSource, ExtVerdict,
    TermProvider, TermSession, segments_text,
};
pub use focus::{FOCUS_FLAP_DELAY, FocusEffect, FocusMsg, FocusState, settle_delay};
pub use health::{
    AppHealth, HUNG_PROBE_MAX, HUNG_PROBE_MIN, HealthEffect, HealthMsg, HealthStepCtx,
    SLOW_RECOVERY, SLOW_WINDOW,
};
pub use mode::{BaseMode, Mode, ModeEffect, ModeKeys, ModeMsg, PassScope, WebCursor};
pub use present::log_states as snapshot_state_names;
pub use present::value_text;
pub use present::{
    Content, DesktopEffect, WindowEffect, WindowInfo, WindowMsg, WindowState,
    announcement_utterance, clean_announcement, dialog_content, fetch_level, fetch_window,
    is_notice_role, lead_segments, notice_segments, reads_content,
};
pub use present::{
    FOCUS_FIELDS, NoLabels, NodeLabel, PresentContext, PresentJob, PresentKind, PresentLog,
    PresentRecord, PresentSource, Presented, Presenter, PresenterRegistry, RAPID_FOCUS,
    REFINE_WINDOW, STORM_HOLD, SpeechIntent, focus_need, key_echo_text, present_focus,
    present_selected_item,
};
pub use quick::{
    BATTERY_DEADLINE, BatteryState, HISTORY_CAP, PowerInfo, PowerSource, install_power_source,
};
pub use reader::{
    BackgroundMsg, DUPLICATE_FOCUS_WINDOW, FetchKind, FocusLaneMsg, QueryReply, Reader,
    ReaderCounters, ReaderEffect, ReaderInbox, ReaderMsg, ReaderSenders, reader_channels,
};
pub use review::{
    BuildError, BuildRequest, Completeness, FlatPos, LineModel, ModelId, NavFetchError, NavOp,
    ProbeItem, ReviewCounters, ReviewCursor, ReviewFrom, ReviewIo, ReviewKind, ReviewLimits,
    ReviewMsg, ReviewOrigin, ReviewPos, ReviewState, RrVerdict,
};
pub use runtime::{
    Core, CoreConfig, CoreError, CoreStats, CoreStatsSnapshot, NullSpeech, SpeechOut,
};
pub use terminal::{TermReads, raw_key};
pub use text::{TextCounters, TextMsg, TextOp, TextReqId};
