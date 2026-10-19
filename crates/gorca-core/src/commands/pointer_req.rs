// SPDX-License-Identifier: GPL-3.0-or-later
//! Requests the Reader sends to the pointer sink and the replies that come back as messages
//! (ADR-0025 EX-1: the Reader never does pointer I/O; ADR-0001 R3: results are messages).

use gorca_output_api::{Button, ButtonAct, PointerError, PointerOutcome, PointerPos};

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

/// A successful pointer call.
#[derive(Clone, Debug, PartialEq)]
pub enum PointerDone {
    Outcome(PointerOutcome),
    Released(u32),
    Pos(PointerPos),
}

/// The answer to a [`PointerReq`], sent to the Reader as `ControlMsg::PointerReply`.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerReply {
    pub id: PtrReqId,
    pub result: Result<PointerDone, PointerError>,
}
