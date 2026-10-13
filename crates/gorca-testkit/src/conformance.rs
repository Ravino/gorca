//! Conformance checks for connectors and sources (ADR-0001 C-1..C-6, SL-1..SL-4). Each check
//! returns a measurement or a verdict; the caller decides the threshold, so the same code serves
//! unit tests (generous bounds, debug builds) and the perf harness (tight bounds, release).

use std::time::{Duration, Instant};

use gorca_connector_api::{
    AppPort, BackendError, BackendOp, BackendReply, BackendRequest, REPLY_GRACE,
};
use gorca_doc_source_api::DeltaSink;
use gorca_model::{DocDelta, NodeEvent, ReqId};
use gorca_rt::{BudgetClass, Deadline, mpsc, with_deadline};

/// Number of equal chunks the push measurements are split into.
const CHUNKS: usize = 16;

/// Wall-clock mean per item, as the minimum over `CHUNKS` chunks of `items`.
///
/// These checks must see a *blocking* push, which burns no CPU, so thread CPU time (see
/// [`crate::cpu`]) would miss exactly the defect they exist to find. Wall time is kept, and
/// robustness to preemption comes from the minimum-of-k rule instead: a push that blocks or
/// does too much work is slow in every chunk, while a preemption on a loaded host hits one
/// chunk at most. The minimum of the chunk means therefore still fails for a real defect and
/// ignores scheduler noise.
fn min_chunk_mean<T>(items: Vec<T>, mut each: impl FnMut(T)) -> Duration {
    let total = items.len();
    let chunk = total.div_ceil(CHUNKS).max(1);
    let mut best = Duration::MAX;
    let mut it = items.into_iter().peekable();
    while it.peek().is_some() {
        let mut n = 0u32;
        let start = Instant::now();
        for item in it.by_ref().take(chunk) {
            each(item);
            n += 1;
        }
        best = best.min(start.elapsed() / n.max(1));
    }
    if best == Duration::MAX {
        Duration::ZERO
    } else {
        best
    }
}

/// C-1: cost per `AppPort::push` over `events`: the minimum over 16 chunk means (see
/// [`min_chunk_mean`]). A blocking `push` shows up as a cost orders of magnitude above the
/// sub-microsecond target in every chunk.
pub fn c1_push_cost(port: &AppPort, events: Vec<NodeEvent>) -> Duration {
    min_chunk_mean(events, |ev| port.push(ev))
}

/// SL-1: cost per `DeltaSink::push`, including the overflow path (minimum over 16 chunk means).
pub fn sl1_sink_never_blocks(sink: &DeltaSink, count: u64) -> Duration {
    min_chunk_mean((0..count).collect(), |batch| {
        sink.push(DocDelta::Settled { batch });
    })
}

/// Outcome of one request in [`c2_requests_are_answered`].
#[derive(Debug)]
pub struct C2Outcome {
    pub result: Result<BackendReply, BackendError>,
    pub elapsed: Duration,
}

/// C-2: send `count` requests with a `deadline_ms` deadline and check that every one finishes
/// (answer or `Timeout`) within `deadline + REPLY_GRACE`. Requests go in sequence through
/// `requests`, the core half of the connector's request queue.
pub async fn c2_requests_are_answered(
    requests: &mpsc::Sender<BackendRequest>,
    op: impl Fn() -> BackendOp,
    count: u64,
    deadline_ms: u64,
) -> Vec<C2Outcome> {
    let mut out = Vec::new();
    for i in 0..count {
        let start = Instant::now();
        let deadline = Deadline(start + Duration::from_millis(deadline_ms));
        let r = gorca_connector_api::request(
            requests,
            ReqId(i),
            BudgetClass::Interactive,
            op(),
            deadline,
        )
        .await;
        out.push(C2Outcome {
            result: r,
            elapsed: start.elapsed(),
        });
    }
    out
}

/// True when every outcome finished within `deadline + REPLY_GRACE + slack`.
pub fn c2_all_within_deadline(outcomes: &[C2Outcome], deadline_ms: u64, slack: Duration) -> bool {
    let limit = Duration::from_millis(deadline_ms) + REPLY_GRACE + slack;
    outcomes.iter().all(|o| o.elapsed <= limit)
}

/// Convenience: run a future under a deadline in a test.
pub async fn within<T>(ms: u64, fut: impl std::future::Future<Output = T>) -> Option<T> {
    with_deadline(Deadline(Instant::now() + Duration::from_millis(ms)), fut)
        .await
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Behavior, FakeConnector, FakeScript};
    use gorca_connector_api::{
        AppChannel, Connector, ConnectorCtx, RegistrarRequest, ScopeRegistry, registrar_channel,
    };
    use gorca_doc_source_api::delta_channel;
    use gorca_ingress::IngressConfig;
    use gorca_model::{ConnectorId, LocalId, NodeKey, PropMask};
    use gorca_rt::{CancellationToken, Clock};
    use std::sync::Arc;

    /// Runs a `FakeConnector` against a minimal core-side registrar and returns the request
    /// sender of its first app.
    async fn start(behavior: Behavior) -> (CancellationToken, mpsc::Sender<BackendRequest>) {
        let (registry, mut rx) = registrar_channel(ConnectorId(9), 8);
        let shutdown = CancellationToken::new();
        let ctx = ConnectorCtx {
            id: ConnectorId(9),
            registry,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let script = FakeScript::single_app("a", behavior);
        tokio::spawn(Box::new(FakeConnector::new(script)).run(ctx));
        let mut table = ScopeRegistry::unbounded();
        let Some(RegistrarRequest::Announce { scope, reply, .. }) = rx.recv().await else {
            panic!("connector did not announce");
        };
        let ch = table
            .register(ConnectorId(9), scope, IngressConfig::default())
            .unwrap();
        let AppChannel { port, requests, .. } = ch;
        let _ = reply.send(Ok(port));
        (shutdown, requests)
    }

    fn fetch() -> BackendOp {
        BackendOp::Fetch {
            node: NodeKey {
                app: gorca_model::AppId {
                    connector: ConnectorId(9),
                    serial: 1,
                },
                local: LocalId(1),
            },
            want: PropMask::NAME,
        }
    }

    #[tokio::test]
    async fn answering_connector_conforms_to_c2() {
        let (shutdown, requests) = start(Behavior::Answer).await;
        let out = c2_requests_are_answered(&requests, fetch, 20, 100).await;
        assert!(out.iter().all(|o| o.result.is_ok()));
        assert!(c2_all_within_deadline(&out, 100, Duration::from_millis(50)));
        shutdown.cancel();
    }

    #[tokio::test]
    async fn hung_and_dropping_connectors_still_end_every_request_by_the_deadline() {
        for behavior in [Behavior::Hang, Behavior::DropSender] {
            let (shutdown, requests) = start(behavior).await;
            let out = c2_requests_are_answered(&requests, fetch, 3, 30).await;
            assert!(
                out.iter()
                    .all(|o| matches!(o.result, Err(BackendError::Timeout))),
                "{behavior:?}"
            );
            assert!(
                c2_all_within_deadline(&out, 30, Duration::from_millis(50)),
                "{behavior:?}"
            );
            shutdown.cancel();
        }
    }

    #[tokio::test]
    async fn busy_connector_reports_busy() {
        let (shutdown, requests) = start(Behavior::Busy).await;
        let out = c2_requests_are_answered(&requests, fetch, 2, 50).await;
        assert!(
            out.iter()
                .all(|o| matches!(o.result, Err(BackendError::Busy)))
        );
        shutdown.cancel();
    }

    #[tokio::test]
    async fn scripted_events_arrive_through_the_port() {
        let (registry, mut rx) = registrar_channel(ConnectorId(9), 8);
        let shutdown = CancellationToken::new();
        let ctx = ConnectorCtx {
            id: ConnectorId(9),
            registry,
            shutdown: shutdown.clone(),
            clock: Clock::System,
        };
        let script = FakeScript::single_app("a", Behavior::Answer).with_events(Arc::new(|app| {
            vec![NodeEvent::FocusChanged {
                node: NodeKey {
                    app,
                    local: LocalId(5),
                },
            }]
        }));
        tokio::spawn(Box::new(FakeConnector::new(script)).run(ctx));
        let mut table = ScopeRegistry::unbounded();
        let Some(RegistrarRequest::Announce { scope, reply, .. }) = rx.recv().await else {
            panic!("no announce");
        };
        let ch = table
            .register(ConnectorId(9), scope, IngressConfig::default())
            .unwrap();
        let AppChannel { port, ingress, .. } = ch;
        let _ = reply.send(Ok(port));
        let drained = within(500, async {
            loop {
                let d = ingress.drain();
                if !d.events.is_empty() {
                    return d;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("event within 500 ms");
        assert!(matches!(drained.events[0], NodeEvent::FocusChanged { .. }));
        shutdown.cancel();
    }

    #[test]
    fn push_and_sink_are_cheap_even_under_overflow() {
        let ch = gorca_connector_api::app_channel(
            gorca_model::AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            IngressConfig::default(),
        );
        let app = ch.port.app;
        let events = crate::storm(3, app, 1000, 20_000);
        let per_push = c1_push_cost(&ch.port, events);
        // Debug build, shared CI box: only a blocking push would exceed this.
        assert!(
            per_push < Duration::from_micros(100),
            "{per_push:?} per push"
        );

        let (sink, _rx) = delta_channel(8);
        let per = sl1_sink_never_blocks(&sink, 10_000);
        assert!(per < Duration::from_micros(100), "{per:?} per push");
    }
}
