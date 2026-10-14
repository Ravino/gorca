use std::ops::Range;

use gorca_model::{DocAction, DocDelta, SrcNodeId};
use gorca_rt::{Deadline, oneshot};
use smallvec::SmallVec;

pub type Reply<T> = oneshot::Sender<Result<T, SourceError>>;

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
}
