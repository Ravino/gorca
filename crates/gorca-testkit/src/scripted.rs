// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted connector with real node data: the in-process stand-in for an AT-SPI application
//! when a test needs the whole core pipeline (ingress, app actor, Reader, fetch, speech) but no
//! bus. Unlike [`FakeConnector`](crate::FakeConnector) it answers `Fetch` with node snapshots,
//! its behaviour can change while the test runs (hang, delay), and the test pushes events
//! through a [`ScriptedHandle`] whenever it wants.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_connector_api::{
    AppPort, BackendError, BackendOp, BackendReply, BackendRequest, Connector, ConnectorCtx,
    ConnectorError, ScopeKey,
};
use gorca_model::{
    AppId, AppInfo, AppLifecycle, ChildList, ConnectorKind, Freshness, Ifaces, LocalId, NodeEvent,
    NodeKey, NodeSnapshot, Role, StateSet, Value,
};
use gorca_rt::{BoxFuture, BudgetClass, Deadline, TaskSet, mpsc};
use tokio::sync::Notify;

use crate::Behavior;

#[derive(Clone, Debug)]
pub struct ScriptedNode {
    pub role: Role,
    pub name: Option<String>,
    pub states: StateSet,
    pub value: Option<Value>,
}

impl ScriptedNode {
    pub fn new(role: Role, name: &str) -> Self {
        Self {
            role,
            name: Some(name.to_owned()),
            states: StateSet::empty(),
            value: None,
        }
    }

    #[must_use]
    pub fn with_states(mut self, states: StateSet) -> Self {
        self.states = states;
        self
    }
}

#[derive(Debug)]
struct Live {
    app: AppId,
    port: Arc<AppPort>,
    behavior: Arc<Mutex<Behavior>>,
    nodes: Arc<Mutex<HashMap<u64, ScriptedNode>>>,
}

#[derive(Debug, Default)]
struct Registry {
    apps: HashMap<Arc<str>, Live>,
}

/// The test's remote control of the scripted applications.
#[derive(Clone, Debug, Default)]
pub struct ScriptedHandle {
    inner: Arc<Mutex<Registry>>,
    announced: Arc<Notify>,
}

impl ScriptedHandle {
    fn with<R>(&self, scope: &str, f: impl FnOnce(&Live) -> R) -> Option<R> {
        let g = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        g.apps.get(scope).map(f)
    }

    /// Wait until `scope` is announced to the core.
    pub async fn wait_app(&self, scope: &str) -> AppId {
        loop {
            let notified = self.announced.notified();
            if let Some(app) = self.with(scope, |l| l.app) {
                return app;
            }
            notified.await;
        }
    }

    pub fn key(&self, scope: &str, local: u64) -> Option<NodeKey> {
        self.with(scope, |l| NodeKey {
            app: l.app,
            local: LocalId(local),
        })
    }

    pub fn push(&self, scope: &str, ev: NodeEvent) {
        let _ = self.with(scope, |l| l.port.push(ev));
    }

    /// Push `FocusChanged` for node `local` of `scope`.
    pub fn focus(&self, scope: &str, local: u64) {
        if let Some(node) = self.key(scope, local) {
            self.push(scope, NodeEvent::FocusChanged { node });
        }
    }

    pub fn set_behavior(&self, scope: &str, b: Behavior) {
        let _ = self.with(scope, |l| {
            *l.behavior.lock().unwrap_or_else(PoisonError::into_inner) = b;
        });
    }

    pub fn set_node(&self, scope: &str, local: u64, node: ScriptedNode) {
        let _ = self.with(scope, |l| {
            l.nodes
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(local, node);
        });
    }

    /// The application left the bus.
    pub fn gone(&self, scope: &str) {
        let _ = self.with(scope, |l| l.port.lifecycle(AppLifecycle::Gone));
    }
}

#[derive(Clone, Debug)]
pub struct ScriptedApp {
    pub scope: String,
    pub nodes: Vec<(u64, ScriptedNode)>,
}

#[derive(Debug)]
pub struct ScriptedConnector {
    apps: Vec<ScriptedApp>,
    handle: ScriptedHandle,
}

impl ScriptedConnector {
    pub fn new(apps: Vec<ScriptedApp>) -> (Self, ScriptedHandle) {
        let handle = ScriptedHandle::default();
        (
            Self {
                apps,
                handle: handle.clone(),
            },
            handle,
        )
    }
}

fn snapshot(key: NodeKey, n: &ScriptedNode) -> Arc<NodeSnapshot> {
    Arc::new(NodeSnapshot {
        key,
        role: n.role,
        states: n.states,
        name: n.name.as_deref().map(Arc::from),
        description: None,
        value: n.value.clone(),
        parent: None,
        children: ChildList::Unknown,
        interfaces: Ifaces::empty(),
        attrs: gorca_model::AttrMap::default(),
        relations: gorca_model::Relations::default(),
        bounds: None,
        have: gorca_model::PropMask::PRESENT,
        confirmed_seq: 0,
        fresh: Freshness::Fresh { at: Instant::now() },
        sem: gorca_model::NodeSem::NONE,
    })
}

async fn serve(
    app: AppId,
    mut requests: mpsc::Receiver<BackendRequest>,
    behavior: Arc<Mutex<Behavior>>,
    nodes: Arc<Mutex<HashMap<u64, ScriptedNode>>>,
) {
    let mut inflight = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            Some(req) = requests.recv() => {
                let b = *behavior.lock().unwrap_or_else(PoisonError::into_inner);
                let nodes = Arc::clone(&nodes);
                inflight.spawn(async move {
                    let reply = |req: BackendRequest| {
                        let r = match &req.op {
                            BackendOp::Fetch { node, .. } => nodes
                                .lock()
                                .unwrap_or_else(PoisonError::into_inner)
                                .get(&node.local.0)
                                .map(|n| BackendReply::Node(snapshot(NodeKey { app, local: node.local }, n)))
                                .ok_or(BackendError::Gone),
                            BackendOp::Probe => Ok(BackendReply::ActionDone),
                            // The resolver's answer is made from the node: the point's x is the node id, so a
                            // test can tell which node was located.
                            BackendOp::Locate { node, offset } => {
                                Ok(BackendReply::Target(gorca_output_api::PointTarget::Window {
                                    top: gorca_output_api::ToplevelHint {
                                        pid: 42,
                                        title: Arc::from("scripted"),
                                        atspi_origin: (0, 0),
                                        atspi_size: (100, 100),
                                        child_space: gorca_output_api::ChildSpace::Toplevel,
                                        pid_is_proxy: false,
                                        active: false,
                                    },
                                    #[allow(clippy::cast_precision_loss)] // test ids are tiny
                                    x: node.local.0 as f64,
                                    y: f64::from(offset.unwrap_or(7)),
                                }))
                            }
                            _ => Err(BackendError::NotSupported),
                        };
                        let _ = req.reply.send(r);
                    };
                    match b {
                        Behavior::Answer => reply(req),
                        Behavior::Delay(d) => {
                            tokio::time::sleep(d).await;
                            reply(req);
                        }
                        Behavior::Hang => {
                            // Keep the sender alive: the core must time out by itself.
                            std::future::pending::<()>().await;
                            drop(req);
                        }
                        Behavior::DropSender => drop(req),
                        Behavior::Busy => {
                            let _ = req.reply.send(Err(BackendError::Busy));
                        }
                    }
                });
            }
            Some(_) = inflight.join_next() => {}
            else => break,
        }
    }
}

impl Connector for ScriptedConnector {
    fn kind(&self) -> ConnectorKind {
        ConnectorKind::Fake
    }

    fn run(self: Box<Self>, ctx: ConnectorCtx) -> BoxFuture<'static, Result<(), ConnectorError>> {
        Box::pin(async move {
            let mut tasks = TaskSet::new(ctx.shutdown.child_token());
            for app in &self.apps {
                let info = AppInfo {
                    toolkit: Arc::from("scripted"),
                    app_name: Arc::from(app.scope.as_str()),
                    pid: None,
                    connector_kind: ConnectorKind::Fake,
                    verified_shell: false,
                    dump_complete: false,
                    text_unit: gorca_model::TextUnit::Scalar,
                    text_insert_slack: 0,
                    edit_quirks: gorca_model::EditQuirks::default(),
                    page_events_settle_ms: 0,
                    web_engine: gorca_model::WebEngine::None,
                    root: None,
                    sem_tables: None,
                };
                let deadline = Deadline::for_class(&ctx.clock, BudgetClass::Startup);
                let mut port = ctx
                    .registry
                    .announce(ScopeKey(Arc::from(app.scope.as_str())), info, deadline)
                    .await
                    .map_err(|e| ConnectorError::Startup(e.to_string()))?;
                let (_tx, closed) = mpsc::channel(1);
                let requests = std::mem::replace(&mut port.requests, closed);
                let behavior = Arc::new(Mutex::new(Behavior::Answer));
                let nodes = Arc::new(Mutex::new(
                    app.nodes.iter().cloned().collect::<HashMap<_, _>>(),
                ));
                let port = Arc::new(port);
                let id = port.app;
                self.handle
                    .inner
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .apps
                    .insert(
                        Arc::from(app.scope.as_str()),
                        Live {
                            app: id,
                            port,
                            behavior: Arc::clone(&behavior),
                            nodes: Arc::clone(&nodes),
                        },
                    );
                self.handle.announced.notify_waiters();
                tasks.spawn("scripted-app", serve(id, requests, behavior, nodes));
            }
            ctx.shutdown.cancelled().await;
            tasks.shutdown(Duration::from_millis(200)).await;
            Ok(())
        })
    }
}
