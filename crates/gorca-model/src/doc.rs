//! Pure-data types of the web document mirror and its source boundary.
//!
//! ADR-0001 section 11 lists `DocDelta`, `DocNodeId` and friends under `gorca-doc-source-api`,
//! but also requires `gorca-doc` to be free of tokio, and `gorca-doc-source-api` needs tokio
//! channel types through `gorca-rt`. To satisfy both, the pure-data types live here and
//! `gorca-doc-source-api` re-exports them.

use std::ops::Range;
use std::sync::Arc;

use smallvec::SmallVec;

use crate::ids::LocalId;
use crate::node::{AttrMap, StateSet};
use crate::role::Role;

/// Identifies one web document (one per document actor).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DocId(pub u32);

/// Document-local node identity: an arena slot plus a generation that bumps when the slot is
/// freed (ADR-0003 DM-I3, AM-2). Never reused within one doc actor's life.
///
/// Only `gorca-doc` mints ids (`from_parts`); every other crate holds and compares them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct DocNodeId {
    slot: u32,
    gen_: u32,
}

impl DocNodeId {
    /// Minted only by the `gorca-doc` arena.
    #[doc(hidden)]
    pub const fn from_parts(slot: u32, gen_: u32) -> Self {
        Self { slot, gen_ }
    }

    pub const fn slot(self) -> u32 {
        self.slot
    }

    pub const fn generation(self) -> u32 {
        self.gen_
    }

    pub const fn to_u64(self) -> u64 {
        ((self.gen_ as u64) << 32) | self.slot as u64
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "low half of the packed u64"
    )]
    pub const fn from_u64(v: u64) -> Self {
        Self {
            slot: v as u32,
            gen_: (v >> 32) as u32,
        }
    }
}

/// Source-native node identity (a DOM or backend node id, or an AT-SPI path handle).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct SrcNodeId(pub u64);

#[derive(Clone, Debug)]
pub struct SrcNode {
    pub id: SrcNodeId,
    pub role: Role,
    pub states: StateSet,
    pub name: Option<Arc<str>>,
    pub text: Option<Arc<str>>,
    pub attrs: AttrMap,
    /// Transport key of the same object, as the connector's drain computes it (`None` for
    /// synthetic text runs). It lets the doc actor build the focus join without a second
    /// channel (ADR-0003 AM-3, EF-3).
    pub local: Option<LocalId>,
}

/// Offsets are in the source's declared `TextUnit`; the mirror converts at apply time (IR-6).
#[derive(Clone, Debug)]
pub enum TextEdit {
    Splice { range: Range<u32>, text: Arc<str> },
    Replace(Arc<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocHint {
    RouteChanged { url: Arc<str> },
    Scrolled,
    Visited(SrcNodeId),
}

#[derive(Clone, Debug)]
pub enum DocDelta {
    /// New document or navigation.
    Reset {
        root: SrcNodeId,
        nav_id: u64,
    },
    Upsert(Vec<SrcNode>),
    /// Complete ordered child list (SL-4), so the mirror can do a keyed ordered-forest diff.
    Children {
        parent: SrcNodeId,
        children: SmallVec<[SrcNodeId; 8]>,
    },
    Text {
        node: SrcNodeId,
        edit: TextEdit,
    },
    Removed(Vec<SrcNodeId>),
    Settled {
        batch: u64,
    },
    Hint(DocHint),
}

/// Actions a source may execute (`SourceCaps::actions`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocAction {
    Focus,
    Click,
    ScrollIntoView,
    SetCaret(u32),
    SetSelection(Range<u32>),
}

/// Fallback caret bookmark when a node no longer joins (IR-7): `(ordinal, context hash)`,
/// relocated by the NVDA-style bookmark rule.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TextAnchor {
    pub ordinal: u32,
    pub context_hash: u64,
}
