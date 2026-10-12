//! Document actor (ADR-0001 section 6.1): one per web document. Owns the document mirror and
//! applies `DocDelta`s from its `SourceLayer`, publishing one snapshot per settled batch.
//!
//! TBD: dirty-subtree refresh and `Subtree` resync requests, the parallel build on `cpu`, the
//! 50 ms / 2,000 node publish cadence while building, the identity join table (`JoinReceiver`)
//! and live-region classification.

use std::sync::Arc;

use arc_swap::ArcSwap;
use gorca_doc::{ApplyOutcome, DocSnapshot, DocumentMirror};
use gorca_doc_source_api::DeltaReceiver;
use gorca_model::DocId;

/// Deltas applied per pump, so one document cannot monopolise a worker.
const MAX_DELTAS_PER_PUMP: usize = 1024;

#[derive(Debug, Default, Eq, PartialEq)]
pub struct DocPump {
    pub applied: usize,
    pub published: bool,
    /// The sink overflowed or a delta referenced an unknown node: a `Subtree` resync is due.
    pub resync_needed: bool,
}

#[derive(Debug)]
pub struct DocActor {
    mirror: DocumentMirror,
    deltas: DeltaReceiver,
    published: Arc<ArcSwap<DocSnapshot>>,
}

impl DocActor {
    pub fn new(doc: DocId, deltas: DeltaReceiver) -> Self {
        let mirror = DocumentMirror::new(doc);
        let published = Arc::new(ArcSwap::new(mirror.snapshot()));
        Self {
            mirror,
            deltas,
            published,
        }
    }

    /// Lock-free read handle for the Reader (`ArcSwap<DocSnapshot>`, PC-1).
    pub fn snapshots(&self) -> Arc<ArcSwap<DocSnapshot>> {
        Arc::clone(&self.published)
    }

    pub fn pump(&mut self) -> DocPump {
        let mut out = DocPump::default();
        for _ in 0..MAX_DELTAS_PER_PUMP {
            let Ok(delta) = self.deltas.deltas.try_recv() else {
                break;
            };
            out.applied += 1;
            match self.mirror.apply(delta) {
                ApplyOutcome::Applied | ApplyOutcome::Hint(_) => {}
                ApplyOutcome::Settled(_) => {
                    self.published.store(self.mirror.snapshot());
                    out.published = true;
                }
                ApplyOutcome::UnknownNode(_)
                | ApplyOutcome::TextRangeInvalid(_)
                | ApplyOutcome::IdSpaceExhausted => out.resync_needed = true,
            }
        }
        if self.deltas.take_overflow() {
            out.resync_needed = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_doc_source_api::delta_channel;
    use gorca_model::{AttrMap, DocDelta, Role, SrcNode, SrcNodeId, StateSet};

    fn node(id: u64, role: Role) -> SrcNode {
        SrcNode {
            id: SrcNodeId(id),
            role,
            states: StateSet::empty(),
            name: None,
            text: None,
            attrs: AttrMap::new(),
        }
    }

    #[test]
    fn snapshots_are_published_only_when_a_batch_settles() {
        let (sink, rx) = delta_channel(16);
        let mut actor = DocActor::new(DocId(1), rx);
        let reader_view = actor.snapshots();
        sink.push(DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        });
        sink.push(DocDelta::Upsert(vec![node(1, Role::Document)]));
        let p = actor.pump();
        assert_eq!(
            p,
            DocPump {
                applied: 2,
                published: false,
                resync_needed: false
            }
        );
        assert!(
            reader_view.load().is_empty(),
            "readers still see the old snapshot"
        );
        sink.push(DocDelta::Settled { batch: 1 });
        assert!(actor.pump().published);
        assert_eq!(reader_view.load().len(), 1);
    }

    #[test]
    fn unknown_nodes_and_sink_overflow_request_a_resync() {
        let (sink, rx) = delta_channel(2);
        let mut actor = DocActor::new(DocId(1), rx);
        sink.push(DocDelta::Children {
            parent: SrcNodeId(9),
            children: smallvec::SmallVec::new(),
        });
        assert!(actor.pump().resync_needed);
        for i in 0..5 {
            sink.push(DocDelta::Settled { batch: i });
        }
        assert!(actor.pump().resync_needed, "overflow flag");
    }
}
