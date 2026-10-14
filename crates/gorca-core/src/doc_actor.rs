//! Document actor (ADR-0001 section 6.1): one per web document. Drains `DeltaReceiver`s from its
//! `SourceLayer`.
//!
//! The scaffold mirror that used to live here is deleted (ADR-0003 AM-2, browse-mode design
//! ID-3). TBD (M7a, browse-mode design section 4.2): the translator from `DocDelta` to
//! `gorca_doc::web::DocOp`, `DocWriter` publication, dirty-subtree refresh and `Subtree` resync
//! requests, the identity join table and live-region classification. Until then the actor only
//! drains the sink and reports settled batches and overflow.

use gorca_doc_source_api::DeltaReceiver;
use gorca_model::{DocDelta, DocId};

/// Deltas drained per pump, so one document cannot monopolise a worker.
const MAX_DELTAS_PER_PUMP: usize = 1024;

#[derive(Debug, Default, Eq, PartialEq)]
pub struct DocPump {
    pub applied: usize,
    /// A `Settled` batch was seen: a snapshot is due once the translator exists.
    pub published: bool,
    /// The sink overflowed: a `Subtree` resync is due.
    pub resync_needed: bool,
}

#[derive(Debug)]
pub struct DocActor {
    doc: DocId,
    deltas: DeltaReceiver,
}

impl DocActor {
    pub fn new(doc: DocId, deltas: DeltaReceiver) -> Self {
        Self { doc, deltas }
    }

    pub fn doc(&self) -> DocId {
        self.doc
    }

    pub fn pump(&mut self) -> DocPump {
        let mut out = DocPump::default();
        for _ in 0..MAX_DELTAS_PER_PUMP {
            let Ok(delta) = self.deltas.deltas.try_recv() else {
                break;
            };
            out.applied += 1;
            match delta {
                DocDelta::Settled { .. } => out.published = true,
                DocDelta::Reset { .. }
                | DocDelta::Upsert(_)
                | DocDelta::Children { .. }
                | DocDelta::Text { .. }
                | DocDelta::Removed(_)
                | DocDelta::Hint(_) => {}
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
    use gorca_model::SrcNodeId;

    #[test]
    fn settled_batches_are_reported() {
        let (sink, rx) = delta_channel(16);
        let mut actor = DocActor::new(DocId(1), rx);
        sink.push(DocDelta::Reset {
            root: SrcNodeId(1),
            nav_id: 1,
        });
        assert_eq!(
            actor.pump(),
            DocPump {
                applied: 1,
                published: false,
                resync_needed: false
            }
        );
        sink.push(DocDelta::Settled { batch: 1 });
        assert!(actor.pump().published);
    }

    #[test]
    fn sink_overflow_requests_a_resync() {
        let (sink, rx) = delta_channel(2);
        let mut actor = DocActor::new(DocId(1), rx);
        for i in 0..5 {
            sink.push(DocDelta::Settled { batch: i });
        }
        assert!(actor.pump().resync_needed, "overflow flag");
    }
}
