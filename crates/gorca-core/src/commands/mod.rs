// SPDX-License-Identifier: GPL-3.0-or-later
//! The command layer (ADR-0005): the closed command set, the keymap compiler, and the Reader's
//! `Key`, `Mode` and `Control` arms.
//!
//! Commands run as Reader messages whose results are speech intents and effects (R3). The input
//! lane (`gorca-input-api`) classifies keys against the [`compile`]d keymap and hands the Reader
//! a [`gorca_input_api::KeyIntent`]; [`handlers`] executes it.

mod compile;
mod diag;
mod handlers;
mod holds;
mod info;
mod keys;
mod mode;
mod mouse;
mod pointer_gate;
mod pointer_req;
pub(crate) mod profiles;
mod spec;
mod state;
mod table;

pub use compile::{
    Compiled, PlatformFacts, browse_grabs, chord_grabs, command_id, compile, compile_with,
    escape_grabs, gated_chord_grabs, gorca_modifiers, grab_collisions, lane_settings, letter_grabs,
    tab_grabs,
};
pub use diag::{
    ATSPI_SOURCE, ExplainReport, SegLine, SourceDirectory, SourceEntry, explain_utterance,
    set_explain_text,
};
pub use handlers::PASS_TIMEOUT;
pub use pointer_gate::{GateOut, OPEN_AFTER, PointerGate, PointerMsg};
pub use pointer_req::{PointerDone, PointerOp, PointerReply, PointerReq, PtrReqId};
pub use profiles::prewarm;
pub use spec::{Availability, CmdSpec, CommandTable, ContextMask, Milestone, RuntimeGate};
pub(crate) use state::CmdState;
pub use state::{
    CapitalLevels, ConfigRequest, ControlMsg, InputNotice, LoadCause, LoadedSettings, LocalTime,
    PrewarmEntries, PrewarmRequest, ProfileFocus, ProfileSaveOutcome, ProfilesSaved, SaveKind,
    SaveProfiles, SaveResult, SettingsMsg, SpeechCtl, SystemWallClock, VoiceLevels, WallClock,
};
pub use table::CoreCmd;

#[cfg(test)]
mod tests;
