// SPDX-License-Identifier: GPL-3.0-or-later
//! Output boundary: speech and braille backends (ADR-0001 section 8).
//!
//! Responsibility (section 11): `SpeechClient`, `SpeechCmd`, `SpeechEvent`, `SpeechCaps`,
//! `Verbalized`, `BrailleBackend`.
//!
//! > "gorca owns the utterance queue, epochs and cancellation. It talks to the server through
//! > `SpeechClient`, with backends voicego-native (primary) and SSIP (compatibility). No
//! > in-process synthesis."  (section 2, item 7)
//!
//! The trait must not change when the TBD-V1..V6 hooks of the voicego backend are filled in
//! (section 8.2): backend differences are expressed through [`SpeechCaps`].
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `thiserror`. Never a transport crate.

mod braille;
pub mod peer;
pub mod queue;
mod speech;
pub mod track;

pub use braille::{BrailleBackend, BrailleCaps, BrailleError, BrailleRegion};
pub use speech::{
    CAPITAL_PITCH_DELTA, ClientHealth, SpeechCaps, SpeechClient, SpeechCmd, SpeechErr, SpeechEvent,
    SubmitError, Verbalized, VoiceParams, VoiceRef, VoiceSetup,
};
