// SPDX-License-Identifier: GPL-3.0-or-later
//! Source descriptor and stable identity (ADR-0024 section 2.6).

use std::sync::Arc;

use gorca_model::fact::AnchorKey;
use gorca_model::sem::SourceClass;

/// A set of anchor kinds a source declares it will use.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct AnchorKinds(pub u8);

impl AnchorKinds {
    pub const NONE: AnchorKinds = AnchorKinds(0);
    pub const EXACT: AnchorKinds = AnchorKinds(1);
    pub const DOM_ID: AnchorKinds = AnchorKinds(2);
    pub const STRUCTURAL: AnchorKinds = AnchorKinds(4);
    pub const REGION: AnchorKinds = AnchorKinds(8);
    pub const CONTENT: AnchorKinds = AnchorKinds(16);
    pub const ALL: AnchorKinds = AnchorKinds(31);

    /// The kind of one anchor key.
    pub const fn of(key: &AnchorKey) -> AnchorKinds {
        match key {
            AnchorKey::Exact { .. } => AnchorKinds::EXACT,
            AnchorKey::DomId { .. } => AnchorKinds::DOM_ID,
            AnchorKey::Structural { .. } => AnchorKinds::STRUCTURAL,
            AnchorKey::Region { .. } => AnchorKinds::REGION,
            AnchorKey::Content(_) => AnchorKinds::CONTENT,
        }
    }

    #[must_use]
    pub const fn union(self, other: AnchorKinds) -> AnchorKinds {
        AnchorKinds(self.0 | other.0)
    }

    /// True when every kind in `other` is in `self`.
    pub const fn contains(self, other: AnchorKinds) -> bool {
        self.0 & other.0 == other.0
    }
}

/// What a source wants the core to send it (the core still re-checks privacy, TP-3).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct InputNeeds(pub u8);

impl InputNeeds {
    pub const NONE: InputNeeds = InputNeeds(0);
    pub const TREE: InputNeeds = InputNeeds(1);
    pub const TEXT: InputNeeds = InputNeeds(2);
    pub const CAPTURE: InputNeeds = InputNeeds(4);
}

/// What a source declares about itself. There are **no scopes**: the transport pins them (FS-11).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceDescriptor {
    pub name: Arc<str>,
    pub class: SourceClass,
    pub version: (u16, u16),
    pub anchors: AnchorKinds,
    pub wants: InputNeeds,
}

/// What the core verified about the peer (exe path hash, extension id, ...).
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub enum PeerProof {
    /// Verified executable (path or digest) of the connecting peer.
    Exe(Arc<str>),
    /// Verified extension id.
    Extension(Arc<str>),
}

/// The identity of a source. Session memory (quarantine, pause, revocation) is keyed on the
/// verified `peer`, never on the self-declared `name`, for the whole session (M-7), so
/// reconnecting never escapes them.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct StableIdentity {
    pub name: Arc<str>,
    pub peer: PeerProof,
}
