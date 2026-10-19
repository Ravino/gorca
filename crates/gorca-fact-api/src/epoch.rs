// SPDX-License-Identifier: GPL-3.0-or-later
//! `SourceEpoch`: the per-(source, scope) counter a mask or revocation bumps (ADR-0024 section
//! 2.6, M-10). The epoch reaches `FUSE` as an input snapshot taken by the writer; no atomic is
//! read inside `FUSE`.

use std::collections::HashMap;

use gorca_model::fact::SourceId;

use crate::admit::Scope;

#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct SourceEpoch(pub u64);

impl SourceEpoch {
    #[must_use]
    pub const fn next(self) -> SourceEpoch {
        SourceEpoch(self.0.saturating_add(1))
    }

    /// True when a fact admitted under `self` is still valid at `current`.
    pub const fn is_current(self, current: SourceEpoch) -> bool {
        self.0 == current.0
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MaskKind {
    Mask,
    Unmask,
    Revoke,
    Purge,
}

/// The timeline message that delivers a mask or revocation to the node's writer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MaskMessage {
    pub source: SourceId,
    pub scope: Scope,
    pub epoch: SourceEpoch,
    pub kind: MaskKind,
}

/// Epochs of every (source, scope). Bumping is O(1), so masking is O(1) to issue (FS-7).
#[derive(Debug, Default)]
pub struct EpochBook {
    epochs: HashMap<(SourceId, Scope), SourceEpoch>,
}

impl EpochBook {
    pub fn new() -> EpochBook {
        EpochBook::default()
    }

    pub fn current(&self, source: SourceId, scope: Scope) -> SourceEpoch {
        self.epochs
            .get(&(source, scope))
            .copied()
            .unwrap_or_default()
    }

    /// Bump and build the message for the writer.
    pub fn bump(&mut self, source: SourceId, scope: Scope, kind: MaskKind) -> MaskMessage {
        let epoch = self.current(source, scope).next();
        self.epochs.insert((source, scope), epoch);
        MaskMessage {
            source,
            scope,
            epoch,
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::ids::{AppId, ConnectorId};

    #[test]
    fn bump_is_monotone_and_per_scope() {
        let src = SourceId {
            slot: 1,
            generation: 0,
        };
        let sc = |n| Scope {
            app: AppId {
                connector: ConnectorId(1),
                serial: n,
            },
            doc: None,
            site: None,
            tab: None,
        };
        let mut b = EpochBook::new();
        let m1 = b.bump(src, sc(1), MaskKind::Mask);
        let m2 = b.bump(src, sc(1), MaskKind::Unmask);
        assert!(m2.epoch.0 > m1.epoch.0);
        assert!(!m1.epoch.is_current(b.current(src, sc(1))));
        assert_eq!(b.current(src, sc(2)), SourceEpoch(0));
    }
}
