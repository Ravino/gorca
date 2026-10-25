// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::Arc;
use std::time::Duration;

use gorca_connector_api::{
    BackendError, BackendOp, BackendReply, BackendRequest, Connector, ConnectorCtx, ConnectorError,
    ScopeKey,
};
use gorca_model::{AppId, AppInfo, ConnectorKind, NodeEvent};
use gorca_rt::{BoxFuture, BudgetClass, Deadline, TaskSet};

/// How the fake app answers requests.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Behavior {
    /// Reply `ActionDone` (or `Children([])`, ...) immediately.
    Answer,
    /// Reply after this delay.
    Delay(Duration),
    /// Never reply and never drop the sender (a hung application).
    Hang,
    /// Drop the reply sender (counts as `Timeout`, C-2).
    DropSender,
    /// Reply `Err(Busy)`.
    Busy,
}

/// Events produced for an app once it is announced (needs the `AppId`, hence a function).
pub type EventScript = Arc<dyn Fn(AppId) -> Vec<NodeEvent> + Send + Sync>;

#[derive(Clone)]
pub struct FakeScript {
    pub apps: Vec<(ScopeKey, AppInfo)>,
    pub behavior: Behavior,
    pub events: EventScript,
}

impl std::fmt::Debug for FakeScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeScript")
            .field("apps", &self.apps.len())
            .field("behavior", &self.behavior)
            .finish_non_exhaustive()
    }
}

impl FakeScript {
    pub fn single_app(name: &str, behavior: Behavior) -> Self {
        Self {
            apps: vec![(
                ScopeKey(Arc::from(format!(":fake.{name}"))),
                AppInfo {
                    toolkit: Arc::from("fake"),
                    app_name: Arc::from(name),
                    pid: None,
                    connector_kind: ConnectorKind::Fake,
                    verified_shell: false,
                    dump_complete: false,
                    exe: None,
                    text_unit: gorca_model::TextUnit::Scalar,
                    text_insert_slack: 0,
                    edit_quirks: gorca_model::EditQuirks::default(),
                    page_events_settle_ms: 0,
                    web_engine: gorca_model::WebEngine::None,
                    root: None,
                    sem_tables: None,
                },
            )],
            behavior,
            events: Arc::new(|_| Vec::new()),
        }
    }

    #[must_use]
    pub fn with_events(mut self, events: EventScript) -> Self {
        self.events = events;
        self
    }
}

/// A scripted connector: announces its apps, pushes scripted events, answers requests per
/// [`Behavior`]. Every task it spawns is owned by a `TaskSet` and ends on `ctx.shutdown`.
#[derive(Debug)]
pub struct FakeConnector {
    script: FakeScript,
}

impl FakeConnector {
    pub fn new(script: FakeScript) -> Self {
        Self { script }
    }
}

async fn serve(mut requests: gorca_rt::mpsc::Receiver<BackendRequest>, behavior: Behavior) {
    while let Some(req) = requests.recv().await {
        match behavior {
            Behavior::Answer => {
                let _ = req.reply.send(Ok(reply_for(&req.op)));
            }
            Behavior::Delay(d) => {
                tokio::time::sleep(d).await;
                let _ = req.reply.send(Ok(reply_for(&req.op)));
            }
            Behavior::Hang => {
                // Keep the sender alive and never answer; the request is parked until shutdown.
                std::mem::forget(req);
            }
            Behavior::DropSender => drop(req),
            Behavior::Busy => {
                let _ = req.reply.send(Err(BackendError::Busy));
            }
        }
    }
}

fn reply_for(op: &BackendOp) -> BackendReply {
    match op {
        BackendOp::Children { .. } => BackendReply::Children(Vec::new()),
        BackendOp::Subtree { .. }
        | BackendOp::ChildSnapshots { .. }
        | BackendOp::Siblings { .. }
        | BackendOp::Ancestry { .. }
        | BackendOp::CacheDump { .. } => {
            BackendReply::Subtree(gorca_model::SnapshotBatch::default())
        }
        BackendOp::Text(_) => BackendReply::Text(Arc::from("")),
        BackendOp::HitTest { .. } => BackendReply::Hit(None),
        BackendOp::Collection(_) => BackendReply::Matches(Vec::new()),
        BackendOp::Fetch { .. }
        | BackendOp::Action(_)
        | BackendOp::Locate { .. }
        | BackendOp::Probe => BackendReply::ActionDone,
    }
}

impl Connector for FakeConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Fake
    }

    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move {
            let mut tasks = TaskSet::new(ctx.shutdown.child_token());
            for (scope, info) in &self.script.apps {
                let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
                let port = ctx
                    .registry
                    .announce(scope.clone(), info.clone(), deadline)
                    .await
                    .map_err(|e| ConnectorError::Startup(e.to_string()))?;
                for ev in (self.script.events)(port.app) {
                    port.push(ev);
                }
                let behavior = self.script.behavior;
                tasks.spawn("fake-app", async move {
                    let mut port = port;
                    let requests = std::mem::replace(&mut port.requests, closed_receiver());
                    serve(requests, behavior).await;
                });
            }
            ctx.shutdown.cancelled().await;
            tasks.shutdown(Duration::from_millis(200)).await;
            Ok(())
        })
    }
}

fn closed_receiver() -> gorca_rt::mpsc::Receiver<BackendRequest> {
    let (_tx, rx) = gorca_rt::mpsc::channel(1);
    rx
}
