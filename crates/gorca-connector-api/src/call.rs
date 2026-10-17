// SPDX-License-Identifier: GPL-3.0-or-later
//! Core-side request helper: the "never block" rule for `BackendRequest`.

use std::time::Duration;

use gorca_model::ReqId;
use gorca_rt::{BudgetClass, Deadline, mpsc, oneshot, with_deadline};

use crate::backend::{BackendError, BackendOp, BackendReply, BackendRequest};

/// Grace after the request deadline before the core gives up on the reply (C-2: "by
/// `deadline + 5 ms`").
pub const REPLY_GRACE: Duration = Duration::from_millis(5);

/// Send one request and wait for its reply, never past `deadline + REPLY_GRACE`.
///
/// - A full request queue is `Busy` (section 6.2: "synthesize `Busy`; the Reader falls back to
///   the snapshot"), without waiting.
/// - A dropped reply sender counts as `Timeout` (C-2).
pub async fn request(
    tx: &mpsc::Sender<BackendRequest>,
    id: ReqId,
    class: BudgetClass,
    op: BackendOp,
    deadline: Deadline,
) -> Result<BackendReply, BackendError> {
    let (reply, rx) = oneshot::channel();
    let req = BackendRequest {
        id,
        deadline,
        class,
        op,
        reply,
    };
    tx.try_send(req).map_err(|e| match e {
        mpsc::error::TrySendError::Full(_) => BackendError::Busy,
        mpsc::error::TrySendError::Closed(_) => BackendError::Gone,
    })?;
    let wait = Deadline(deadline.0 + REPLY_GRACE);
    match with_deadline(wait, rx).await {
        Ok(Ok(result)) => result,
        Ok(Err(_)) | Err(_) => Err(BackendError::Timeout),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey, PropMask};
    use std::time::Instant;

    fn op() -> BackendOp {
        BackendOp::Fetch {
            node: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            want: PropMask::NAME,
        }
    }

    fn dl(ms: u64) -> Deadline {
        Deadline(Instant::now() + Duration::from_millis(ms))
    }

    #[tokio::test(start_paused = true)]
    async fn answered_request_returns_the_reply() {
        let (tx, mut rx) = mpsc::channel::<BackendRequest>(4);
        tokio::spawn(async move {
            let req = rx.recv().await.unwrap();
            let _ = req.reply.send(Ok(BackendReply::ActionDone));
        });
        let r = request(&tx, ReqId(1), BudgetClass::Interactive, op(), dl(120)).await;
        assert!(matches!(r, Ok(BackendReply::ActionDone)));
    }

    #[tokio::test(start_paused = true)]
    async fn silent_connector_times_out_at_the_deadline() {
        let (tx, mut rx) = mpsc::channel::<BackendRequest>(4);
        let hold = tokio::spawn(async move {
            let req = rx.recv().await;
            std::future::pending::<()>().await;
            drop(req);
        });
        let start = tokio::time::Instant::now();
        let r = request(&tx, ReqId(1), BudgetClass::Interactive, op(), dl(120)).await;
        assert!(matches!(r, Err(BackendError::Timeout)));
        assert!(
            start.elapsed() <= Duration::from_millis(126),
            "{:?}",
            start.elapsed()
        );
        hold.abort();
    }

    #[tokio::test(start_paused = true)]
    async fn dropped_sender_counts_as_timeout() {
        let (tx, mut rx) = mpsc::channel::<BackendRequest>(4);
        tokio::spawn(async move {
            drop(rx.recv().await);
        });
        let r = request(&tx, ReqId(1), BudgetClass::Interactive, op(), dl(120)).await;
        assert!(matches!(r, Err(BackendError::Timeout)));
    }

    #[tokio::test(start_paused = true)]
    async fn full_queue_is_busy_and_closed_queue_is_gone() {
        let (tx, rx) = mpsc::channel::<BackendRequest>(1);
        let first = request(&tx, ReqId(1), BudgetClass::Prefetch, op(), dl(10));
        // The first request occupies the only slot and is never answered.
        let busy = async {
            tokio::task::yield_now().await;
            request(&tx, ReqId(2), BudgetClass::Prefetch, op(), dl(10)).await
        };
        let (a, b) = tokio::join!(first, busy);
        assert!(matches!(a, Err(BackendError::Timeout)));
        assert!(matches!(b, Err(BackendError::Busy)));
        drop(rx);
        let r = request(&tx, ReqId(3), BudgetClass::Prefetch, op(), dl(10)).await;
        assert!(matches!(r, Err(BackendError::Gone)));
    }
}
