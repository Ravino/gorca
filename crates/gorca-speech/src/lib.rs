//! SpeechQueue state machine, speech lane logic and verbalizer.
//!
//! Responsibility (ADR-0001 section 11): "`SpeechQueue` SM, speech lane, verbalizer
//! (punctuation, symbols, dicts)". Sections 7.4 and 8.1.
//!
//! > "S-1 gorca is the queue authority. The server holds at most `caps.max_in_flight`
//! > utterances ... All other queued speech lives in gorca."
//! > "S-2 The validity check (E-2) runs at send time and on every transition."
//! > "S-3 No speech command ever blocks the speech lane."
//!
//! The state machine is a pure `SpeechState::step(self, msg, ctx) -> (Self, effects)` with an
//! exhaustive `match` and no wildcard arm. [`SpeechQueue`] wraps it with the bounded FIFO.
//! TBD: the lane thread itself (select over intents, backend events, timers and the
//! `EpochCell` notify) lands with bake-off B-5 and MB-2; this crate defines the logic it runs.
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `gorca-output-api`. Never a concrete
//! backend (`gorca-speech-*`).
#![deny(clippy::wildcard_enum_match_arm)]

mod backend;
mod failover;
#[cfg(test)]
mod fake;
mod lane;
mod queue;
mod state;
mod verbalize;

pub use backend::{BackendConnectError, FnBackend, SpeechBackend};
pub use failover::{FailoverClient, FailoverConfig, FailoverMode, FailoverNotice};
pub use lane::{LaneConfig, SpeechHandle, SpeechLane, SpeechStats};
pub use queue::{FIFO_CAPACITY, SpeechQueue};
pub use state::{
    REPLACE_DEADLINE, SpeechEffect, SpeechMsg, SpeechState, StepCtx, UttMeta, backoff,
};
pub use verbalize::Verbalizer;
