// SPDX-License-Identifier: GPL-3.0-or-later
//! Requests the Reader sends to the pointer sink and the replies that come back as messages
//! (ADR-0025 EX-1: the Reader never does pointer I/O; ADR-0001 R3: results are messages).

use gorca_model::NodeKey;
use gorca_output_api::{Button, ButtonAct, IfCovered, PointerError, PointerOutcome, PointerPos};

/// Correlates a request with its reply.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct PtrReqId(pub u64);

/// What to do at the pointer.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PointerOp {
    /// `Click` or `DoubleClick` of a button at the current position.
    Click(Button, ButtonAct),
    /// Hold a button down (drag lock).
    Press(Button),
    Release(Button),
    /// Release everything this caller holds.
    ReleaseAll,
    /// Read the pointer position.
    Position,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PointerReq {
    pub id: PtrReqId,
    pub op: PointerOp,
}

/// What to do at a located node.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum AtAct {
    /// Move the pointer there (route the mouse to the review position).
    Move,
    /// Move and click in one compositor turn.
    Click(Button),
}

/// Which accessibility action an item click tries before the pointer (ADR-0025 2.6, Orca's order).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ItemAction {
    /// `click press jump open activate`.
    Click,
    /// `show-context-menu menu`.
    ContextMenu,
}

/// A node (or one character of its text) to put the pointer on. The runtime asks the node's app for
/// the target (`BackendOp::Locate`) and then the pointer sink; both calls carry deadlines and the
/// answer is a [`PointerReply`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PointerAtReq {
    pub id: PtrReqId,
    pub node: NodeKey,
    /// A scalar offset in the node's text, for a character target.
    pub offset: Option<u32>,
    pub act: AtAct,
    pub if_covered: IfCovered,
    /// Item clicks: try this action in the node's own application first; only when it has none, use the
    /// pointer (`mouse.item_click = action_first`). `None`: the pointer only.
    pub item: Option<ItemAction>,
}

/// A successful pointer call.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerDone {
    Outcome(PointerOutcome),
    /// The click was done by the application's own action: no pointer was involved.
    Acted,
    Released(u32),
    Pos(PointerPos),
}

/// The answer to a [`PointerReq`], sent to the Reader as `ControlMsg::PointerReply`.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerReply {
    pub id: PtrReqId,
    pub result: Result<PointerDone, PointerError>,
}
