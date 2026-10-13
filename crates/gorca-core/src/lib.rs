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
mod builtin_ext;
mod commands;
mod directory;
mod doc_actor;
mod focus;
mod health;
mod mode;
mod present;
mod reader;
mod review;
mod runtime;
mod text;

pub use app_actor::{AppActor, AppRoute, FILL_QUEUE, FillMsg, ReaderLinks, health_view};
pub use builtin_ext::AppProfile;
pub use directory::MirrorDirectory;
pub use doc_actor::DocActor;
pub use focus::{FOCUS_FLAP_DELAY, FocusEffect, FocusMsg, FocusState, settle_delay};
pub use health::{
    AppHealth, HUNG_PROBE_MAX, HUNG_PROBE_MIN, HealthEffect, HealthMsg, HealthStepCtx,
    SLOW_RECOVERY, SLOW_WINDOW,
};
pub use mode::{Mode, ModeEffect, ModeKeys, ModeMsg, PassScope, ReviewCursor};
pub use present::{
    NoLabels, NodeLabel, PresentContext, PresentJob, PresentKind, PresentLog, PresentRecord,
    PresentSource, Presented, Presenter, PresenterRegistry, REFINE_WINDOW, SpeechIntent,
    focus_need, key_echo_text, present_focus, present_selected_item,
};
pub use reader::{
    BackgroundMsg, DUPLICATE_FOCUS_WINDOW, FetchKind, FocusLaneMsg, QueryReply, Reader,
    ReaderCounters, ReaderEffect, ReaderInbox, ReaderMsg, ReaderSenders, reader_channels,
};
pub use review::ReviewMsg;
pub use runtime::{
    Core, CoreConfig, CoreError, CoreStats, CoreStatsSnapshot, NullSpeech, SpeechOut,
};
pub use text::TextMsg;
