//! Review cursor types (ADR-0006 3). All `Copy`, so `Mode` stays `Copy`.

use gorca_doc::DocCursor;
use gorca_model::{DocId, NodeKey};

/// Unique per model build within a process; never reused.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ModelId(pub u64);

/// Identifies one model build request.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct BuildId(pub u64);

/// Identifies one navigation fetch or action request.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct NavReqId(pub u64);

/// Where `ExitReview` returns to (modes.md section 3, never Review).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReviewFrom {
    Focus,
    Browse {
        doc: DocId,
        cursor: DocCursor,
        sticky: bool,
    },
}

/// The node the user was on when review started, and where review returns to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReviewOrigin {
    pub node: NodeKey,
    pub from: ReviewFrom,
}

/// Review kinds (ADR-0006 2.1). `Document` joins in M7.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReviewKind {
    Object,
    Screen,
}

/// Re-anchoring key used when a newer model replaces the one a position belongs to (RA-1).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ZoneAnchor {
    pub node: NodeKey,
    pub src_start: u32,
    pub y_mid: i32,
}

/// A position inside a line model.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FlatPos {
    pub model: ModelId,
    pub line: u32,
    pub zone: u16,
    /// Unicode scalar offset inside the zone text (TU-1).
    pub offset: u32,
    pub anchor: ZoneAnchor,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReviewPos {
    /// Object review. M4a: the navigator's presentation text only.
    Object { offset: u32, text_line: Option<u32> },
    /// Screen (flat) review over a line model. `None` = not anchored yet.
    Screen(Option<FlatPos>),
}

/// NVDA's navigator object plus the reading position.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReviewCursor {
    /// Always set; may read `Gone` (ADR-0006 13).
    pub nav: NodeKey,
    pub pos: ReviewPos,
}

impl ReviewCursor {
    pub fn kind(&self) -> ReviewKind {
        match self.pos {
            ReviewPos::Object { .. } => ReviewKind::Object,
            ReviewPos::Screen(_) => ReviewKind::Screen,
        }
    }

    /// A fresh cursor of `kind` at `nav`.
    pub fn at(nav: NodeKey, kind: ReviewKind) -> Self {
        Self {
            nav,
            pos: match kind {
                ReviewKind::Object => ReviewPos::Object {
                    offset: 0,
                    text_line: None,
                },
                ReviewKind::Screen => ReviewPos::Screen(None),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId};

    fn node() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            local: LocalId(1),
        }
    }

    #[test]
    fn a_fresh_cursor_has_the_requested_kind() {
        assert_eq!(
            ReviewCursor::at(node(), ReviewKind::Screen).kind(),
            ReviewKind::Screen
        );
        assert_eq!(
            ReviewCursor::at(node(), ReviewKind::Object).kind(),
            ReviewKind::Object
        );
    }
}
