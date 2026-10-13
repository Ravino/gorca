//! Command dispatcher: key intents and mode changes (ADR-0001 sections 7.2, 7.3).
//!
//! Owns the Reader's handling of [`gorca_input_api::KeyIntent`] and [`crate::ModeMsg`]: the
//! `Key` and `Mode` arms of [`crate::ReaderMsg`]. Milestone M1 grows the command registry
//! (`CmdId` to action, keymap lookup) here without touching `reader.rs`; commands run as
//! Reader messages whose results are speech intents (R3).

mod keys;
mod mode;
