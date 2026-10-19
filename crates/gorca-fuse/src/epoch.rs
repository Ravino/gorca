// SPDX-License-Identifier: GPL-3.0-or-later
//! Epochs as `FUSE` inputs (ADR-0024 section 2.6, M-10): a **global** epoch (the patch set, a
//! host-wide mask) and a per-source epoch that carries the source's lifecycle state. Both are
//! joined by `max`, so a late or duplicated epoch message cannot move a source backwards, and both
//! reach `FUSE` only as an immutable [`EpochSnapshot`]: no atomic is read inside `FUSE`.
//!
//! One writer serves one scope, so a source's epoch here is its epoch for that scope. The
//! `(source, scope)` bookkeeping that spans writers belongs to `gorca-fact-api` (`EpochBook`).

use std::collections::BTreeMap;

use gorca_model::fact::SourceId;

use crate::side::{EpochSnapshot, SourceState};

const fn state_code(s: SourceState) -> u8 {
    match s {
        SourceState::Active => 0,
        SourceState::Throttled => 1,
        SourceState::Detached => 2,
        SourceState::Paused => 3,
        SourceState::Quarantined => 4,
        SourceState::Revoked => 5,
    }
}

/// True for the states that silence a source: masking and revocation must reach the focus chain
/// and the visible window in the same writer turn (FS-7).
pub const fn silences(s: SourceState) -> bool {
    match s {
        SourceState::Paused
        | SourceState::Quarantined
        | SourceState::Revoked
        | SourceState::Detached => true,
        SourceState::Active | SourceState::Throttled => false,
    }
}

/// The epochs of one writer.
#[derive(Clone, Debug, Default)]
pub struct Epochs {
    global: u64,
    sources: BTreeMap<SourceId, (u64, SourceState)>,
}

impl Epochs {
    /// The global epoch (bumped by a patch-set change or a host-wide mask).
    pub const fn global(&self) -> u64 {
        self.global
    }

    /// Bump the global epoch; every node must be re-fused.
    pub fn bump_global(&mut self) -> u64 {
        self.global = self.global.saturating_add(1);
        self.global
    }

    /// Join a per-source epoch message `(n, state)` by `max`. Returns true iff the epoch of
    /// `src` changed. The order is total: `n` first, then the state code.
    pub fn join_source(&mut self, src: SourceId, n: u64, state: SourceState) -> bool {
        let new = (n, state_code(state));
        match self.sources.get(&src) {
            Some(&(cn, cs)) if new <= (cn, state_code(cs)) => false,
            _ => {
                self.sources.insert(src, (n, state));
                true
            }
        }
    }

    /// The epoch entry of `src`: `None` until an epoch message arrived for it.
    pub fn entry(&self, src: SourceId) -> Option<(u64, SourceState)> {
        self.sources.get(&src).copied()
    }

    /// The state of `src`, or `None` when no epoch message ever arrived for it. A newly
    /// registered source has no state and stays **masked** until an explicit `Active` epoch.
    pub fn state(&self, src: SourceId) -> Option<SourceState> {
        self.sources.get(&src).map(|&(_, s)| s)
    }

    /// Remove and return a released source's epoch entry. The caller decides whether to retain a
    /// silencing state by stable name (a revoked source must not escape by reconnecting).
    pub fn take(&mut self, src: SourceId) -> Option<(u64, SourceState)> {
        self.sources.remove(&src)
    }

    /// The snapshot `FUSE` reads: only sources with an epoch entry are listed. A registered source
    /// without one, and an unregistered one, are missing and therefore masked (fail closed).
    pub fn snapshot(&self, registered: impl IntoIterator<Item = SourceId>) -> EpochSnapshot {
        let mut e = EpochSnapshot::default();
        for id in registered {
            if let Some(s) = self.state(id) {
                e.set(id, s);
            }
        }
        e
    }
}

/// States that survive a reconnect or a release: the user's or the host's decision to silence a
/// source is about the source, not about one connection.
pub const fn sticky(s: SourceState) -> bool {
    match s {
        SourceState::Paused | SourceState::Quarantined | SourceState::Revoked => true,
        SourceState::Active | SourceState::Throttled | SourceState::Detached => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(n: u32) -> SourceId {
        SourceId {
            slot: n,
            generation: 0,
        }
    }

    #[test]
    fn epochs_are_joins_not_overwrites() {
        let mut e = Epochs::default();
        assert!(e.join_source(id(1), 2, SourceState::Revoked));
        assert!(!e.join_source(id(1), 1, SourceState::Active), "older epoch");
        assert!(!e.join_source(id(1), 2, SourceState::Revoked), "duplicate");
        assert_eq!(e.state(id(1)), Some(SourceState::Revoked));
        assert!(e.join_source(id(1), 3, SourceState::Active));
        assert_eq!(e.state(id(1)), Some(SourceState::Active));
        assert_eq!(
            e.state(id(9)),
            None,
            "no epoch yet: masked until Active arrives"
        );
    }

    #[test]
    fn snapshot_lists_only_registered_sources() {
        let mut e = Epochs::default();
        e.join_source(id(1), 1, SourceState::Paused);
        e.join_source(id(2), 1, SourceState::Active);
        let s = e.snapshot([id(1), id(2), id(4)]);
        assert_eq!(s.state(id(1)), Some(SourceState::Paused));
        assert_eq!(s.state(id(2)), Some(SourceState::Active));
        assert_eq!(
            s.state(id(4)),
            None,
            "registered without an epoch stays masked"
        );
        assert_eq!(s.state(id(3)), None, "unregistered stays fail-closed");
        assert!(sticky(SourceState::Revoked) && !sticky(SourceState::Detached));
    }

    #[test]
    fn global_epoch_is_monotone() {
        let mut e = Epochs::default();
        assert_eq!(e.bump_global(), 1);
        assert_eq!(e.bump_global(), 2);
        assert!(silences(SourceState::Revoked) && !silences(SourceState::Throttled));
    }
}
