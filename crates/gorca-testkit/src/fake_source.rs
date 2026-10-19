// SPDX-License-Identifier: GPL-3.0-or-later
use gorca_doc_source_api::{
    ActionCaps, DocDelta, DocRequest, DocSource, DocSourceCtx, JoinKeyCaps, SourceCaps,
    SourceError, SourceKind, TextUnit,
};
use gorca_rt::BoxFuture;

/// Pushes a script of deltas on attach, then answers requests: `Subtree` with an empty delta
/// list, `Text` with an empty string, `Action` with `Ok`, `Validate` with all `true`.
#[derive(Debug, Default)]
pub struct FakeDocSource {
    pub script: Vec<DocDelta>,
}

impl DocSource for FakeDocSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Atspi
    }

    fn caps(&self) -> SourceCaps {
        SourceCaps {
            bulk_snapshot: true,
            push_deltas: true,
            settled_markers: true,
            text_unit: TextUnit::Scalar,
            actions: ActionCaps::all(),
            join_keys: JoinKeyCaps::EXACT,
            silent_reorders: false,
        }
    }

    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            let DocSourceCtx {
                sink,
                mut requests,
                shutdown,
                ..
            } = ctx;
            for d in self.script {
                sink.push(d);
            }
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Ok(()),
                    req = requests.recv() => {
                        let Some(req) = req else { return Err(SourceError::Detached) };
                        match req {
                            DocRequest::Subtree { reply, .. } => { let _ = reply.send(Ok(Vec::new())); }
                            DocRequest::Text { reply, .. } => { let _ = reply.send(Ok(String::new())); }
                            DocRequest::Action { reply, .. } => { let _ = reply.send(Ok(())); }
                            DocRequest::Validate { nodes, reply, .. } => {
                                let _ = reply.send(Ok(vec![true; nodes.len()]));
                            }
                            DocRequest::Revalidate { reply, .. } => {
                                let _ = reply.send(Ok(smallvec::SmallVec::default()));
                            }
                            DocRequest::Repair { reply } => {
                                let _ = reply.send(Ok(gorca_doc_source_api::RepairReport::default()));
                            }
                            DocRequest::SetActivity(_)
                            | DocRequest::Rebuild
                            | DocRequest::Prioritize { .. }
                            | DocRequest::DriftCheck => {}
                        }
                    }
                }
            }
        })
    }
}

// ---------------------------------------------------------------------------------------------
// FakeFactSource: a scripted, pure fact source of any class (M22a-B3). No tokio, no IO: it
// produces the list of things a real source's transport would deliver, in order, and the
// conformance kit (`fact_conformance`) and the writer-side tests replay them.
// ---------------------------------------------------------------------------------------------

use std::sync::Arc;

use gorca_fact_api::{AnchorKinds, InputNeeds, PeerProof, SourceDescriptor, StableIdentity};
use gorca_model::AppId;
use gorca_model::fact::{
    AnchorKey, Basis, Confidence, EvidenceValue, Fact, FactValue, PropValue, SourceId, SrcSeq,
    Subject,
};
use gorca_model::sem::{Group, GroupCode, SourceClass};

/// One thing a fake source's transport delivers.
#[derive(Clone, Debug)]
pub enum SourceOutput {
    /// The transport connected and the peer was verified.
    Hello,
    Fact(Fact),
    /// The connection dropped.
    TransportClosed,
    /// The user revoked the source.
    Revoke,
    /// A privacy pause started or ended.
    Pause,
    Resume,
}

/// A scripted fact source. Builder methods append to the script; `seq` advances by one per fact
/// unless a method says otherwise.
#[derive(Clone, Debug)]
pub struct FakeFactSource {
    descriptor: SourceDescriptor,
    identity: StableIdentity,
    id: SourceId,
    seq: u64,
    script: Vec<SourceOutput>,
}

impl FakeFactSource {
    /// A source of `class` declaring every anchor kind; slot 1, generation 0.
    pub fn new(name: &str, class: SourceClass) -> FakeFactSource {
        let name: Arc<str> = Arc::from(name);
        FakeFactSource {
            descriptor: SourceDescriptor {
                name: name.clone(),
                class,
                version: (1, 0),
                anchors: AnchorKinds::ALL,
                wants: InputNeeds::NONE,
            },
            identity: StableIdentity {
                peer: PeerProof::Extension(name.clone()),
                name,
            },
            id: SourceId {
                slot: 1,
                generation: 0,
            },
            seq: 0,
            script: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_anchors(mut self, kinds: AnchorKinds) -> FakeFactSource {
        self.descriptor.anchors = kinds;
        self
    }

    /// Host this source on a given verified peer (several named sources can share one peer).
    #[must_use]
    pub fn on_peer(mut self, peer: &str) -> FakeFactSource {
        self.identity.peer = PeerProof::Extension(Arc::from(peer));
        self
    }

    pub fn descriptor(&self) -> &SourceDescriptor {
        &self.descriptor
    }

    pub fn identity(&self) -> &StableIdentity {
        &self.identity
    }

    pub fn id(&self) -> SourceId {
        self.id
    }

    pub fn class(&self) -> SourceClass {
        self.descriptor.class
    }

    pub fn script(&self) -> &[SourceOutput] {
        &self.script
    }

    /// The facts of the script, in order.
    pub fn facts(&self) -> impl Iterator<Item = &Fact> {
        self.script.iter().filter_map(|o| match o {
            SourceOutput::Fact(f) => Some(f),
            SourceOutput::Hello
            | SourceOutput::TransportClosed
            | SourceOutput::Revoke
            | SourceOutput::Pause
            | SourceOutput::Resume => None,
        })
    }

    /// The slot is recycled for a new source: same slot, next generation (M-8).
    pub fn reuse_slot(&mut self) -> &mut FakeFactSource {
        self.id.generation = self.id.generation.saturating_add(1);
        self
    }

    fn next_seq(&mut self) -> SrcSeq {
        self.seq += 1;
        SrcSeq(self.seq)
    }

    /// Default TTL: Inferred facts require one, others may omit it.
    fn default_ttl(&self) -> Option<u32> {
        match self.descriptor.class {
            SourceClass::Inferred | SourceClass::Annotation => Some(300_000),
            SourceClass::Primary | SourceClass::Secondary => None,
        }
    }

    fn push(&mut self, f: Fact) -> &mut FakeFactSource {
        self.script.push(SourceOutput::Fact(f));
        self
    }

    pub fn hello(&mut self) -> &mut FakeFactSource {
        self.script.push(SourceOutput::Hello);
        self
    }

    pub fn detach(&mut self) -> &mut FakeFactSource {
        self.script.push(SourceOutput::TransportClosed);
        self
    }

    pub fn revoke(&mut self) -> &mut FakeFactSource {
        self.script.push(SourceOutput::Revoke);
        self
    }

    pub fn pause(&mut self) -> &mut FakeFactSource {
        self.script.push(SourceOutput::Pause);
        self
    }

    pub fn resume(&mut self) -> &mut FakeFactSource {
        self.script.push(SourceOutput::Resume);
        self
    }

    /// A well-formed value fact with the given confidence (`p_milli`) and basis.
    pub fn value(
        &mut self,
        subject: Subject,
        group: GroupCode,
        value: PropValue,
        p_milli: u16,
        basis: Basis,
    ) -> &mut FakeFactSource {
        let seq = self.next_seq();
        let ttl_ms = self.default_ttl();
        self.push(Fact {
            subject,
            group,
            value: FactValue::Value(value),
            seq,
            conf: Confidence::from_p_milli(p_milli),
            basis,
            ttl_ms,
        })
    }

    /// A well-formed name fact at `p_milli` with no basis.
    pub fn name(&mut self, subject: Subject, text: &str, p_milli: u16) -> &mut FakeFactSource {
        self.value(
            subject,
            Group::Name.group_code(),
            PropValue::Name(Arc::from(text)),
            p_milli,
            Basis::None,
        )
    }

    pub fn absent(&mut self, subject: Subject, group: GroupCode) -> &mut FakeFactSource {
        let seq = self.next_seq();
        let ttl_ms = self.default_ttl();
        self.push(Fact {
            subject,
            group,
            value: FactValue::Absent,
            seq,
            conf: Confidence::from_p_milli(1000),
            basis: Basis::None,
            ttl_ms,
        })
    }

    pub fn retract(&mut self, subject: Subject, group: GroupCode) -> &mut FakeFactSource {
        let seq = self.next_seq();
        self.push(Fact {
            subject,
            group,
            value: FactValue::Retract,
            seq,
            conf: Confidence::from_p_milli(1000),
            basis: Basis::None,
            ttl_ms: None,
        })
    }

    /// Malformed: a `Level` payload delivered for the `name` group.
    pub fn malformed_value(&mut self, subject: Subject) -> &mut FakeFactSource {
        let seq = self.next_seq();
        let ttl_ms = self.default_ttl();
        self.push(Fact {
            subject,
            group: Group::Name.group_code(),
            value: FactValue::Value(PropValue::Level(3)),
            seq,
            conf: Confidence::from_p_milli(950),
            basis: Basis::None,
            ttl_ms,
        })
    }

    /// Malformed: a group code no registry knows.
    pub fn unregistered_group(&mut self, subject: Subject, code: u16) -> &mut FakeFactSource {
        let seq = self.next_seq();
        let ttl_ms = self.default_ttl();
        self.push(Fact {
            subject,
            group: GroupCode(code),
            value: FactValue::Absent,
            seq,
            conf: Confidence::from_p_milli(950),
            basis: Basis::None,
            ttl_ms,
        })
    }

    /// Malformed: a confidence above 1000 milli.
    pub fn bad_confidence(&mut self, subject: Subject) -> &mut FakeFactSource {
        let seq = self.next_seq();
        let ttl_ms = self.default_ttl();
        self.push(Fact {
            subject,
            group: Group::Name.group_code(),
            value: FactValue::Value(PropValue::Name(Arc::from("x"))),
            seq,
            conf: Confidence {
                class: gorca_model::sem::ConfClass::High,
                p_milli: 5000,
            },
            basis: Basis::None,
            ttl_ms,
        })
    }

    /// A name fact about a node of another app: outside the pinned scope (F19).
    pub fn out_of_scope(
        &mut self,
        other: AppId,
        node: gorca_model::LocalId,
    ) -> &mut FakeFactSource {
        let subject = Subject::Node(gorca_model::NodeKey {
            app: other,
            local: node,
        });
        self.name(subject, "elsewhere", 990)
    }

    /// A name fact whose text is `len` bytes of ASCII (page-derived oversize string).
    pub fn oversize_name(&mut self, subject: Subject, len: usize) -> &mut FakeFactSource {
        let text = "a".repeat(len);
        self.name(subject, &text, 990)
    }

    /// An evidence text fact (for example `hint.title_attr`).
    pub fn evidence_text(
        &mut self,
        subject: Subject,
        kind: gorca_model::sem::EvidenceKind,
        text: &str,
        p_milli: u16,
    ) -> &mut FakeFactSource {
        self.value(
            subject,
            kind.group_code(),
            PropValue::Evidence(EvidenceValue::Text(Arc::from(text))),
            p_milli,
            Basis::None,
        )
    }

    /// Re-send the previous fact's sequence number (a conformance violation).
    pub fn repeat_last_seq(&mut self) -> &mut FakeFactSource {
        let last = self.facts().last().cloned();
        if let Some(mut f) = last {
            f.value = FactValue::Absent;
            self.push(f)
        } else {
            self
        }
    }

    /// A subject for a Dom id anchor with the given `dom_count`.
    pub fn dom_anchor(id: &str, dom_count: u16) -> Subject {
        Subject::Anchor(AnchorKey::DomId {
            frame: gorca_model::fact::FrameKey(1),
            shadow: 0,
            id: Arc::from(id),
            dom_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_doc_source_api::{delta_channel, join_channel};
    use gorca_model::{DocId, SrcNodeId};
    use gorca_rt::{CancellationToken, Clock, Deadline, mpsc, oneshot};
    use smallvec::smallvec;
    use std::time::{Duration, Instant};

    #[tokio::test]
    async fn pushes_its_script_answers_requests_and_detaches_on_shutdown() {
        let (sink, mut deltas) = delta_channel(8);
        let (req_tx, requests) = mpsc::channel(4);
        let (join, _jrx) = join_channel(4);
        let shutdown = CancellationToken::new();
        let (_ev_tx, events) = mpsc::channel(4);
        let ctx = DocSourceCtx {
            doc: DocId(1),
            app: gorca_model::AppId {
                connector: gorca_model::ConnectorId(0),
                serial: 0,
            },
            sink,
            requests,
            events,
            join,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let src = FakeDocSource {
            script: vec![DocDelta::Settled { batch: 1 }],
        };
        let h = tokio::spawn(Box::new(src).attach(ctx));

        assert!(matches!(
            deltas.deltas.recv().await,
            Some(DocDelta::Settled { batch: 1 })
        ));

        let (reply, rx) = oneshot::channel();
        req_tx
            .send(DocRequest::Validate {
                nodes: smallvec![SrcNodeId(1), SrcNodeId(2)],
                deadline: Deadline(Instant::now() + Duration::from_secs(1)),
                reply,
            })
            .await
            .unwrap();
        assert_eq!(rx.await.unwrap().unwrap(), vec![true, true]);

        shutdown.cancel();
        assert!(h.await.unwrap().is_ok());
    }
}
