use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gorca_model::{DocDelta, DocId};
use gorca_rt::{BoxFuture, CancellationToken, Clock, mpsc};

use crate::caps::{SourceCaps, SourceKind};
use crate::join::JoinPort;
use crate::request::{DocRequest, SourceError};

pub trait DocSource: Send + 'static {
    fn kind(&self) -> SourceKind;
    fn caps(&self) -> SourceCaps;
    /// Runs until detached or shutdown. Must honour deadlines on every `DocRequest`.
    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>>;
}

pub struct DocSourceCtx {
    pub doc: DocId,
    /// Non-blocking push (SL-1).
    pub sink: DeltaSink,
    /// Bounded; every request has a deadline.
    pub requests: mpsc::Receiver<DocRequest>,
    /// Identity reconciliation (section 9.3).
    pub join: JoinPort,
    pub shutdown: CancellationToken,
    pub clock: Clock,
}

impl std::fmt::Debug for DocSourceCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DocSourceCtx")
            .field("doc", &self.doc)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
struct Overflow(AtomicBool);

/// Producer side of the delta channel. `push` never blocks (SL-1).
/// TBD: coalescing per `SrcNodeId` before the channel.
#[derive(Clone, Debug)]
pub struct DeltaSink {
    tx: mpsc::Sender<DocDelta>,
    overflow: Arc<Overflow>,
}

/// Consumer side, held by the doc actor.
#[derive(Debug)]
pub struct DeltaReceiver {
    pub deltas: mpsc::Receiver<DocDelta>,
    overflow: Arc<Overflow>,
}

pub fn delta_channel(capacity: usize) -> (DeltaSink, DeltaReceiver) {
    let (tx, rx) = mpsc::channel(capacity);
    let overflow = Arc::new(Overflow(AtomicBool::new(false)));
    (
        DeltaSink {
            tx,
            overflow: Arc::clone(&overflow),
        },
        DeltaReceiver {
            deltas: rx,
            overflow,
        },
    )
}

impl DeltaSink {
    /// On a full or closed channel the delta is dropped and the document is marked dirty.
    pub fn push(&self, delta: DocDelta) {
        if self.tx.try_send(delta).is_err() {
            self.overflow.0.store(true, Ordering::Release);
        }
    }
}

impl DeltaReceiver {
    /// True once if any delta was dropped since the last call: the doc actor must resync.
    pub fn take_overflow(&self) -> bool {
        self.overflow.0.swap(false, Ordering::AcqRel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::SrcNodeId;

    #[test]
    fn overflow_marks_dirty_instead_of_blocking() {
        let (sink, mut rx) = delta_channel(2);
        for i in 0..5 {
            sink.push(DocDelta::Settled { batch: i });
        }
        assert!(rx.take_overflow());
        assert!(!rx.take_overflow(), "flag is cleared by take");
        assert!(rx.deltas.try_recv().is_ok());
        assert!(rx.deltas.try_recv().is_ok());
        assert!(rx.deltas.try_recv().is_err());
        sink.push(DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        });
        assert!(!rx.take_overflow());
    }
}
