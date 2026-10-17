// SPDX-License-Identifier: GPL-3.0-or-later
//! Bulk-presentation job descriptors and the intent handed to the speech lane.

use gorca_model::{Epoch, NodeKey, Utterance};

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PresentKind {
    Focus,
    Table,
    SayAllChunk,
    LiveRegionDiff,
}

/// Bulk work for the `cpu` pool. The result carries `epoch` (E-1, E-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PresentJob {
    pub node: NodeKey,
    pub epoch: Epoch,
    pub kind: PresentKind,
}

/// Reader or `cpu` to the speech lane. The utterance carries its epoch.
#[derive(Clone, Debug)]
pub struct SpeechIntent {
    pub utterance: Utterance,
}
