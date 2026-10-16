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
    /// Awake or dormant (AM-4). Fire and forget: the effect shows in the deltas.
    SetActivity(Activity),
    /// Rebuild as resync: refetch the whole document, re-match it against the ids already issued
    /// (ids kept) and push the difference followed by `Settled` (AM-4, IL-R5).
    Rebuild,
    /// Urgent marks for nodes at the cursor (UP-2, M7b): the source bypasses its settle for
    /// events on them.
    Prioritize { nodes: SmallVec<[SrcNodeId; 8]> },
    /// W3 drift check (M7b).
    DriftCheck,
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
}
