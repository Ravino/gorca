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
