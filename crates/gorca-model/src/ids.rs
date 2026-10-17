// SPDX-License-Identifier: GPL-3.0-or-later
//! Identities (ADR-0001 section 4.1) and the process-wide epoch value.

use std::fmt;

/// Identifies one connector instance. Never reused within a gorca process lifetime.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ConnectorId(pub u16);

/// Unit of isolation. AT-SPI: one bus unique name. Newton (later): one surface.
/// Never reused within a gorca process lifetime.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct AppId {
    pub connector: ConnectorId,
    pub serial: u32,
}

/// Connector-local handle, opaque to the core. AT-SPI: interned
/// `(bus_name, object_path, generation)`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct LocalId(pub u64);

/// Stable identity of one node.
///
/// Invariants (ADR-0001):
/// - **I-1** A `NodeKey` never denotes two different objects. A connector that cannot guarantee
///   freshness bumps a generation inside [`LocalId`] on reuse.
/// - **I-2** The core holds no connector type.
/// - **I-3** Resolving a gone key yields `Lookup::Gone`; it never panics and never blocks.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct NodeKey {
    pub app: AppId,
    pub local: LocalId,
}

/// Cancellation epoch (ADR-0001 section 3.4). Monotonic; the atomic cell lives in `gorca-rt`.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Epoch(pub u64);

impl Epoch {
    pub const ZERO: Epoch = Epoch(0);
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "e{}", self.0)
    }
}

/// Request identifier, unique per process.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ReqId(pub u64);

/// Utterance identifier chosen by gorca (`SpeechCaps::utterance_ids`).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct UttId(pub u64);
