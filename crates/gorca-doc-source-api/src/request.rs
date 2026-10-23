// SPDX-License-Identifier: GPL-3.0-or-later
use std::ops::Range;

use gorca_model::{DocAction, DocDelta, SrcNodeId};
use gorca_rt::{Deadline, oneshot};
use smallvec::SmallVec;

pub type Reply<T> = oneshot::Sender<Result<T, SourceError>>;

/// Whether the document is the one the user is reading (ADR-0003 DM-I5, AM-4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Activity {
    /// Process events and refetch as usual.
    Awake,
    /// Mark events only (no IPC) until woken; a wake always resyncs in M7a (AM-13).
    Dormant,
}

/// Doc actor to source. Every request has a deadline (SL-2).
#[derive(Debug)]
pub enum DocRequest {
    Subtree {
        root: SrcNodeId,
        max_nodes: u32,
        deadline: Deadline,
        reply: Reply<Vec<DocDelta>>,
    },
    Text {
        node: SrcNodeId,
        range: Range<u32>,
        deadline: Deadline,
        reply: Reply<String>,
    },
    Action {
        node: SrcNodeId,
        act: DocAction,
        deadline: Deadline,
        reply: Reply<()>,
    },
    Validate {
        nodes: SmallVec<[SrcNodeId; 4]>,
        deadline: Deadline,
        reply: Reply<Vec<bool>>,
    },
    /// ADR-0003 IL-R1 (revalidation on read): re-list the children of `parent` at the transport
    /// (one `GetChildren`), push whatever deltas the difference needs through the sink, and reply
    /// with the parent's current ordered child ids. The hot path never waits for it; dedupe
    /// (once per parent per 250 ms) is the doc actor's job.
    Revalidate {
        parent: SrcNodeId,
        deadline: Deadline,
        reply: Reply<SmallVec<[SrcNodeId; 16]>>,
    },
    /// The on-screen rectangles of nodes (layout, for label inference: M22a C3b). One entry per
    /// node, in order; `None` for a node that is gone or has no box. Cheap (one call per node,
    /// no tree walk) and only ever asked for a handful of nodes at a focus, never in bulk.
    Extents {
        nodes: SmallVec<[SrcNodeId; 6]>,
        deadline: Deadline,
        reply: Reply<SmallVec<[Option<gorca_model::Rect>; 6]>>,
    },
    /// Awake or dormant (AM-4). Fire and forget: the effect shows in the deltas.
    SetActivity(Activity),
    /// Rebuild as resync: refetch the whole document, re-match it against the ids already issued
    /// (ids kept) and push the difference followed by `Settled` (AM-4, IL-R5).
    Rebuild,
    /// A repair of a stale mirror (CHR-0): the same refetch and re-match as [`DocRequest::Rebuild`],
    /// but answered with a report once the difference is in the sink, so the document actor can
    /// meter repairs by their cost. The source runs it off its request loop: requests that arrive
    /// meanwhile are served against the old mirror.
    ///
    /// `cheap_check`: the request is a guess (events on unknown nodes, a state flip), so the source
    /// first looks at the child counts of the top of its tree and answers without a refetch when
    /// they match the mirror.
    Repair {
        cheap_check: bool,
        reply: Reply<RepairReport>,
    },
    /// Urgent marks for nodes at the cursor (UP-2, M7b): the source bypasses its settle for
    /// events on them.
    Prioritize { nodes: SmallVec<[SrcNodeId; 8]> },
    /// W3 drift check (M7b).
    DriftCheck,
}

/// What a repair found and what it cost (CHR-0).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RepairReport {
    /// Nodes the refetch returned.
    pub nodes: usize,
    /// Deltas the difference needed (0: the mirror was already right).
    pub deltas: usize,
    /// Wall time of the whole repair, fetch included.
    pub took: std::time::Duration,
    /// The refetch failed; nothing was pushed.
    pub failed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SourceError {
    #[error("request timed out")]
    Timeout,
    #[error("source detached")]
    Detached,
    #[error("operation not supported by this source")]
    NotSupported,
    #[error("protocol error: {0}")]
    Protocol(String),
    /// The object answered but declined (a `GrabFocus` that returned false).
    #[error("request refused")]
    Refused,
    /// The object the request names no longer exists (the page replaced it) and could not be
    /// matched to a live one by identity: nothing was done to any other object.
    #[error("target object is gone")]
    Gone,
    /// A click was not made: the object at the node is not the one the user heard (the page
    /// re-used or re-ordered its rows since the mirror last said so). The mirror is refreshed;
    /// the user hears what is there now and decides (CHR-0, act only on what was spoken).
    #[error("target object changed since it was spoken")]
    Displaced(Option<std::sync::Arc<str>>),
    /// A page selection was applied, but not exactly as asked (the engine rounded it).
    #[error("applied approximately")]
    Approximate,
}
