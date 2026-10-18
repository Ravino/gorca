// SPDX-License-Identifier: GPL-3.0-or-later
//! Audio cues: short non-speech signals (ADR-0017 plan, docs/plan/phase-3-features.md 7.3).
//!
//! A [`CueSink`] sits beside the speech client. Nothing renders sound yet: [`NullCueSink`] is the
//! placeholder that counts what was asked and plays nothing, so the Reader can already say "this
//! is a boundary" through the final seam and the sound theme (milestone M13) only has to replace
//! the sink.

use std::sync::atomic::{AtomicU64, Ordering};

/// A cue the Reader can ask for. Grows with the sound theme (mode switches, capitals, lock keys).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum CueId {
    /// A movement key could not move the caret: the start or end of a line or field.
    Boundary,
}

impl CueId {
    /// Every cue, for tests and theme checks.
    pub const ALL: [CueId; 1] = [CueId::Boundary];

    /// The name of the cue in a theme directory.
    pub fn name(self) -> &'static str {
        match self {
            CueId::Boundary => "boundary",
        }
    }
}

/// What a sink can do.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CueCaps {
    /// Cues play while speech is going on (a sidecar), rather than in the speech stream.
    pub overlaps_speech: bool,
    /// The sink renders sound at all. False for the placeholder.
    pub audible: bool,
}

/// Why a cue was not played.
#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CueError {
    #[error("the cue queue is full")]
    Full,
    #[error("the cue sink is gone")]
    Disconnected,
}

/// Where cues go. `play` never blocks.
pub trait CueSink: Send + Sync + 'static {
    fn caps(&self) -> CueCaps;

    /// Ask for `cue` to be played soon. Never blocks; a full queue or a dead sink is an error the
    /// caller may ignore (a missed cue is not worth a stall).
    fn play(&self, cue: CueId) -> Result<(), CueError>;
}

/// The placeholder sink: plays nothing, counts requests.
#[derive(Debug, Default)]
pub struct NullCueSink {
    requested: AtomicU64,
}

impl NullCueSink {
    /// Cues asked for so far.
    pub fn requested(&self) -> u64 {
        self.requested.load(Ordering::Relaxed)
    }
}

impl CueSink for NullCueSink {
    fn caps(&self) -> CueCaps {
        CueCaps::default()
    }

    fn play(&self, _cue: CueId) -> Result<(), CueError> {
        self.requested.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_null_sink_counts_and_is_silent() {
        let s = NullCueSink::default();
        assert!(!s.caps().audible);
        for c in CueId::ALL {
            assert_eq!(s.play(c), Ok(()));
        }
        assert_eq!(s.requested(), CueId::ALL.len() as u64);
    }

    #[test]
    fn cue_names_are_distinct() {
        let mut names: Vec<_> = CueId::ALL.iter().map(|c| c.name()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CueId::ALL.len());
    }
}
