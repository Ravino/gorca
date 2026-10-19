// SPDX-License-Identifier: GPL-3.0-or-later
//! Core to connector requests. Every request has a deadline (never-block rule).

use std::ops::Range;
use std::sync::Arc;

use gorca_model::{
    AttrMap, NodeKey, NodeSnapshot, Point, PropMask, ReqId, Role, SnapshotBatch, StateSet,
    TextFormat, TextLines, TextQuery, TextSelections, TextUnitAt, TextWindowFill, Value,
};
use gorca_rt::{BudgetClass, Deadline, oneshot};

#[derive(Debug)]
pub struct BackendRequest {
    pub id: ReqId,
    /// Mandatory. The connector must reply or drop `reply` by then (C-2).
    pub deadline: Deadline,
    pub class: BudgetClass,
    pub op: BackendOp,
    pub reply: oneshot::Sender<Result<BackendReply, BackendError>>,
}

#[derive(Clone, Debug)]
pub enum BackendOp {
    Fetch {
        node: NodeKey,
        want: PropMask,
    },
    Children {
        node: NodeKey,
        range: Range<u32>,
    },
    Subtree {
        root: NodeKey,
        max_nodes: u32,
    },
    /// Children of `node` in `range`, each as a snapshot with at least `want`. Answered with
    /// `BackendReply::Subtree`; partial nodes carry their `have`.
    ChildSnapshots {
        node: NodeKey,
        range: Range<u32>,
        want: PropMask,
    },
    /// The siblings of `node` under `parent`: `before` entries ahead of it, the node and `after`
    /// entries behind it, each as a snapshot with at least `want` and the parent link. The node's
    /// position comes out of the parent's child list, so the caller needs no index. Answered
    /// with `BackendReply::Subtree`; empty when `node` is not (or no longer) a child of `parent`.
    Siblings {
        parent: NodeKey,
        node: NodeKey,
        before: u32,
        after: u32,
        /// The parent's child list is long: do not list it, read the window by index
        /// (`GetIndexInParent` + `GetChildAtIndex`). Without it a reply too large to be a
        /// window's worth of list is refused (`Protocol`).
        indexed: bool,
        want: PropMask,
    },
    /// `node`, its parent, its parent's parent ... up to `max_depth` (at most 64) or the app
    /// root; stops at a cycle, a foreign reference or a `Document*` role. Answered with
    /// `BackendReply::Subtree`, `truncated` set when it stopped early.
    Ancestry {
        node: NodeKey,
        max_depth: u8,
        want: PropMask,
    },
    /// Whole-app bulk dump (AT-SPI: `Cache.GetItems`). `NotSupported` when the app has none.
    /// Answered with `BackendReply::Subtree`.
    CacheDump {
        max_nodes: u32,
    },
    Text(TextQuery),
    /// `DoDefault`, `Focus`, `SetCaret`, `Select`, `ScrollTo`, `SetValue`, `Click`, `ContextMenu`.
    Action(ActionReq),
    /// Where a pointer must go to hit `node` (or its character `offset`, a scalar offset in the node's
    /// text): the pointer resolver of ADR-0025. Answered with `BackendReply::Target`.
    Locate {
        node: NodeKey,
        offset: Option<u32>,
    },
    HitTest {
        point: Point,
    },
    Collection(MatchRule),
    /// The cheapest possible liveness check of the app (AT-SPI: `GetRole` on the app root). Used
    /// by the `Hung` probe (I-4); answered with `BackendReply::ActionDone`.
    Probe,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ActionReq {
    DoDefault { node: NodeKey },
    Focus { node: NodeKey },
    SetCaret { node: NodeKey, offset: u32 },
    Select { node: NodeKey },
    ScrollTo { node: NodeKey },
    SetValue { node: NodeKey, value: Value },
}

/// Collection query (AT-SPI `GetMatches` and equivalents).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchRule {
    pub root: NodeKey,
    pub roles: Vec<Role>,
    pub states_all: StateSet,
    pub states_none: StateSet,
    pub max_results: u32,
}

#[derive(Clone, Debug)]
pub struct TextAttrRun {
    pub range: Range<u32>,
    pub attrs: AttrMap,
}

#[derive(Clone, Debug)]
pub enum BackendReply {
    Node(Arc<NodeSnapshot>),
    Children(Vec<NodeKey>),
    Subtree(SnapshotBatch),
    Text(Arc<str>),
    CaretOffset(u32),
    TextAttributes(Vec<TextAttrRun>),
    ActionDone,
    /// Answer to `BackendOp::Locate`.
    Target(gorca_output_api::PointTarget),
    Hit(Option<NodeKey>),
    Matches(Vec<NodeKey>),
    /// Answer to `TextQuery::VisibleLines`.
    Lines(TextLines),
    /// Answer to `TextQuery::AtOffset`.
    Unit(TextUnitAt),
    /// Answer to `TextQuery::Selections`.
    Selections(TextSelections),
    /// Answer to `TextQuery::Window`.
    Window(TextWindowFill),
    /// Answer to `TextQuery::Format`.
    Format(TextFormat),
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BackendError {
    #[error("request timed out")]
    Timeout,
    #[error("target is gone")]
    Gone,
    #[error("operation not supported by this connector or toolkit")]
    NotSupported,
    #[error("connector is busy (in-flight limit reached)")]
    Busy,
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The connector has no path for this key (evicted from its path table). A miss, never
    /// `Gone`: the object may well exist.
    #[error("the key has no known path")]
    Unresolved,
}
