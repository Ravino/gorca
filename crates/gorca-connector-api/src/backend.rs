//! Core to connector requests. Every request has a deadline (never-block rule).

use std::ops::Range;
use std::sync::Arc;

use gorca_model::{
    AttrMap, NodeKey, NodeSnapshot, Point, PropMask, ReqId, Role, SnapshotBatch, StateSet,
    TextQuery, Value,
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
    Text(TextQuery),
    /// `DoDefault`, `Focus`, `SetCaret`, `Select`, `ScrollTo`, `SetValue`.
    Action(ActionReq),
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
    Hit(Option<NodeKey>),
    Matches(Vec<NodeKey>),
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
}
