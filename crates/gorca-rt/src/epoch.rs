// SPDX-License-Identifier: GPL-3.0-or-later
//! The epoch as a lock-free cancellation primitive (ADR-0001 section 3.4).

use std::sync::atomic::{AtomicU64, Ordering};

use gorca_model::Epoch;

/// Process-wide, monotonic. Written by the Reader (focus/mode/nav change) and the input lane
/// (interrupting key press). Read by everyone.
///
/// - **E-1** Every utterance, query and `PresentJob` carries the epoch it was created in.
/// - **E-2** The speech lane checks validity at send time and at every state transition.
/// - **E-3** Results from a worker with an older epoch are dropped by whoever receives them first.
/// - **E-4** The Reader adopts the epoch carried by an input `KeyIntent` via [`EpochCell::observe`].
#[derive(Debug, Default)]
pub struct EpochCell(AtomicU64);

impl EpochCell {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    /// `fetch_add(1) + 1`, Release.
    pub fn advance(&self) -> Epoch {
        Epoch(self.0.fetch_add(1, Ordering::AcqRel).saturating_add(1))
    }

    /// `fetch_max(e)`, for adopting an epoch carried by a message. Returns the resulting value.
    pub fn observe(&self, e: Epoch) -> Epoch {
        let prev = self.0.fetch_max(e.0, Ordering::AcqRel);
        Epoch(prev.max(e.0))
    }

    /// Acquire load.
    pub fn current(&self) -> Epoch {
        Epoch(self.0.load(Ordering::Acquire))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn advance_is_strictly_increasing() {
        let c = EpochCell::new();
        assert_eq!(c.current(), Epoch(0));
        assert_eq!(c.advance(), Epoch(1));
        assert_eq!(c.advance(), Epoch(2));
        assert_eq!(c.current(), Epoch(2));
    }

    #[test]
    fn observe_never_goes_backwards() {
        let c = EpochCell::new();
        c.advance();
        c.advance();
        assert_eq!(c.observe(Epoch(1)), Epoch(2));
        assert_eq!(c.current(), Epoch(2));
        assert_eq!(c.observe(Epoch(9)), Epoch(9));
        assert_eq!(c.current(), Epoch(9));
    }

    #[test]
    fn concurrent_advances_are_unique() {
        let c = Arc::new(EpochCell::new());
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let c = Arc::clone(&c);
                std::thread::spawn(move || (0..1000).map(|_| c.advance().0).collect::<Vec<_>>())
            })
            .collect();
        let mut all: Vec<u64> = handles
            .into_iter()
            .flat_map(|h| h.join().unwrap())
            .collect();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 4000);
        assert_eq!(c.current(), Epoch(4000));
    }
}
