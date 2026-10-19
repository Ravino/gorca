// SPDX-License-Identifier: GPL-3.0-or-later
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
use crate::sem::SrcRecord;

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
    /// The node as the source's normaliser saw it (H-7: the normaliser runs in the source crate,
    /// never in the core). `None` for synthetic nodes (text runs, placeholders) and for sources
    /// that do not normalise. Shared, never mutated: the document actor feeds it to the fusion.
    pub rec: Option<Arc<SrcRecord>>,
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
    /// Select these hypertext ranges in the page (core side: ids are [`DocNodeId`]; the doc actor
    /// rewrites it to [`DocAction::SetSourceSelection`] before the source sees it).
    SetSelection(SelectionSpec<DocNodeId>),
    /// The same with the source's own node ids.
    SetSourceSelection(SelectionSpec<SrcNodeId>),
    /// Collapse the page selection set earlier (and drop extra ranges); `node` is a container
    /// that held it.
    ClearSelection,
}

/// One hypertext range of a page selection: `range` in UTF-16 units (Gecko's unit) and
/// `range_cp` in Unicode scalars (Blink's), `end` exclusive and exact. The flags say what the
/// end touches, because engines read an end offset that names an embedded child differently
/// (Firefox ends the range after a hypertext child and before an object, Blink excludes both):
/// the source adapts, the mirror stays engine-neutral.
#[allow(clippy::struct_excessive_bools)] // four independent facts about one range end
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelSpan<N> {
    pub node: N,
    pub range: Range<u32>,
    pub range_cp: Range<u32>,
    /// The last selected character is an embedded hypertext child (a link, a list item).
    pub end_on_child: bool,
    /// The last selected character is an embedded object that has no text (an image).
    pub end_on_object: bool,
    /// The character at `end` is an embedded hypertext child.
    pub child_follows: bool,
    /// The last selected character is white space (leaving it out names the same words).
    pub trim_ws: bool,
}

/// The exact atom interval as two points, for engines that take one range across objects
/// (`Document.SetTextSelections`): `(object, offset in scalars)` for the start and the end.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocRange<N> {
    pub start: (N, u32),
    pub end: (N, u32),
}

/// A page selection as hypertext ranges: `spans` are exact (one range each, in document order),
/// `snap` is the single-range alternative for engines that keep one range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SelectionSpec<N> {
    /// The document node: the browser keeps its ranges per document, and only the document's
    /// hypertext counts and removes all of them.
    pub root: N,
    pub spans: Vec<SelSpan<N>>,
    pub snap: SelSpan<N>,
    /// The exact interval as two points (see [`DocRange`]).
    pub doc_range: DocRange<N>,
    /// Gecko cannot select this exactly (a leg ends inside a hypertext child it continues
    /// into, or several ranges are joined with no separator): the copy may differ from what
    /// was heard. The source reports `Approximate` when it applied such a plan.
    pub gecko_lossy: bool,
    /// The editable node that holds the page focus, when the selection is not inside it. A plain
    /// text control (`input`, `textarea`) answers Ctrl+C from its own selection, so the page
    /// selection set here would never be copied: the source moves the focus to the document first.
    pub focus_field: Option<N>,
}

/// Fallback caret bookmark when a node no longer joins (IR-7): `(ordinal, context hash)`,
/// relocated by the NVDA-style bookmark rule.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct TextAnchor {
    pub ordinal: u32,
    pub context_hash: u64,
}
