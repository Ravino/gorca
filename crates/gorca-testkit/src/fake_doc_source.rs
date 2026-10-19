// SPDX-License-Identifier: GPL-3.0-or-later
//! A scripted web document source and a fake source factory, for the state machines of the
//! document side (doc actor, registry, Reader) without a browser.
//!
//! * [`ScriptedDocSource`] pushes the batches the test hands it, records every request the doc
//!   actor sends (actions, activity changes, rebuilds) and ends on demand.
//! * [`FakeDocFactory`] answers `is_web`, `needs_warm`, `warm`, `locate` and `create` from a
//!   table, with optional delays, and keeps the handle of every source it made.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gorca_doc_source_api::{
    ActionCaps, Activity, DocRequest, DocSource, DocSourceCtx, DocSourceFactory, JoinKeyCaps,
    SourceCaps, SourceError, SourceKind, WarmOutcome, WarmReport,
};
use gorca_model::{AppId, AppInfo, DocAction, DocDelta, NodeKey, SrcNodeId, TextUnit};
use gorca_rt::{BoxFuture, mpsc};

/// A request the source received from the doc actor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Seen {
    Action(SrcNodeId, DocAction),
    Activity(Activity),
    Rebuild,
    /// A repair request (answered with an empty report).
    Repair,
    Other,
}

/// The test's side of a [`ScriptedDocSource`].
#[derive(Debug)]
pub struct ScriptHandle {
    /// Batches to push; each batch is delivered in order. Include a `Settled` marker.
    pub batches: mpsc::Sender<Vec<DocDelta>>,
    pub seen: mpsc::Receiver<Seen>,
    /// Ends the source with `Detached` (navigation, a dead browser).
    pub end: mpsc::Sender<()>,
}

/// A source driven by a [`ScriptHandle`].
#[derive(Debug)]
pub struct ScriptedDocSource {
    batches: mpsc::Receiver<Vec<DocDelta>>,
    seen: mpsc::Sender<Seen>,
    end: mpsc::Receiver<()>,
}

impl ScriptedDocSource {
    pub fn new() -> (Self, ScriptHandle) {
        let (batches_tx, batches) = mpsc::channel(64);
        let (seen, seen_rx) = mpsc::channel(256);
        let (end_tx, end) = mpsc::channel(1);
        (
            Self { batches, seen, end },
            ScriptHandle {
                batches: batches_tx,
                seen: seen_rx,
                end: end_tx,
            },
        )
    }
}

impl DocSource for ScriptedDocSource {
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
            let Self {
                mut batches,
                seen,
                mut end,
            } = *self;
            let DocSourceCtx {
                sink,
                mut requests,
                shutdown,
                ..
            } = ctx;
            loop {
                tokio::select! {
                    biased;
                    () = shutdown.cancelled() => return Ok(()),
                    _ = end.recv() => return Err(SourceError::Detached),
                    b = batches.recv() => {
                        let Some(b) = b else { return Ok(()) };
                        for d in b { sink.push(d); }
                    }
                    req = requests.recv() => {
                        let Some(req) = req else { return Err(SourceError::Detached) };
                        let s = match req {
                            DocRequest::Action { node, act, reply, .. } => {
                                let _ = reply.send(Ok(()));
                                Seen::Action(node, act)
                            }
                            DocRequest::SetActivity(a) => Seen::Activity(a),
                            DocRequest::Rebuild => Seen::Rebuild,
                            DocRequest::Repair { reply, .. } => {
                                let _ = reply.send(Ok(gorca_doc_source_api::RepairReport::default()));
                                Seen::Repair
                            }
                            DocRequest::Text { reply, .. } => {
                                let _ = reply.send(Ok(String::new()));
                                Seen::Other
                            }
                            DocRequest::Subtree { reply, .. } => {
                                let _ = reply.send(Ok(Vec::new()));
                                Seen::Other
                            }
                            DocRequest::Validate { nodes, reply, .. } => {
                                let _ = reply.send(Ok(vec![true; nodes.len()]));
                                Seen::Other
                            }
                            DocRequest::Revalidate { reply, .. } => {
                                let _ = reply.send(Ok(smallvec::SmallVec::default()));
                                Seen::Other
                            }
                            DocRequest::Prioritize { .. } | DocRequest::DriftCheck => Seen::Other,
                        };
                        let _ = seen.try_send(s);
                    }
                }
            }
        })
    }
}

/// How the fake factory answers `warm`.
#[derive(Clone, Debug)]
pub struct WarmScript {
    pub delay: Duration,
    pub outcome: Result<WarmOutcome, SourceError>,
}

impl Default for WarmScript {
    fn default() -> Self {
        Self {
            delay: Duration::ZERO,
            outcome: Ok(WarmOutcome::Done(WarmReport {
                ready: true,
                ..WarmReport::default()
            })),
        }
    }
}

#[derive(Debug, Default)]
struct Inner {
    web: bool,
    needs_warm: bool,
    warm: Vec<WarmScript>,
    warm_calls: usize,
    /// Focus node to its top-level document root.
    docs: HashMap<NodeKey, NodeKey>,
    locate_delay: Duration,
    created: Vec<(NodeKey, ScriptHandle)>,
    warm_hints: Vec<Option<NodeKey>>,
    locate_calls: usize,
}

/// A scripted [`DocSourceFactory`].
#[derive(Clone, Debug, Default)]
pub struct FakeDocFactory {
    inner: Arc<Mutex<Inner>>,
}

impl FakeDocFactory {
    pub fn new(web: bool, needs_warm: bool) -> Self {
        let f = Self::default();
        f.with(|i| {
            i.web = web;
            i.needs_warm = needs_warm;
        });
        f
    }

    fn with<R>(&self, f: impl FnOnce(&mut Inner) -> R) -> R {
        f(&mut self.inner.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Answers of successive `warm` calls (the last one repeats).
    pub fn script_warm(&self, s: Vec<WarmScript>) {
        self.with(|i| i.warm = s);
    }

    /// `locate(focus)` returns `root`.
    pub fn map_document(&self, focus: NodeKey, root: NodeKey) {
        self.with(|i| {
            i.docs.insert(focus, root);
        });
    }

    pub fn set_locate_delay(&self, d: Duration) {
        self.with(|i| i.locate_delay = d);
    }

    /// The handle of the next source created, if any (FIFO).
    pub fn take_created(&self) -> Option<(NodeKey, ScriptHandle)> {
        self.with(|i| (!i.created.is_empty()).then(|| i.created.remove(0)))
    }

    pub fn warm_calls(&self) -> usize {
        self.with(|i| i.warm_calls)
    }

    pub fn warm_hints(&self) -> Vec<Option<NodeKey>> {
        self.with(|i| i.warm_hints.clone())
    }

    pub fn locate_calls(&self) -> usize {
        self.with(|i| i.locate_calls)
    }
}

impl DocSourceFactory for FakeDocFactory {
    fn is_web(&self, _info: &AppInfo) -> bool {
        self.with(|i| i.web)
    }

    fn needs_warm(&self, _info: &AppInfo) -> bool {
        self.with(|i| i.needs_warm)
    }

    fn warm(
        &self,
        _app: AppId,
        _info: &AppInfo,
        hint: Option<NodeKey>,
    ) -> BoxFuture<'static, Result<WarmOutcome, SourceError>> {
        let script = self.with(|i| {
            let n = i.warm_calls;
            i.warm_calls += 1;
            i.warm_hints.push(hint);
            i.warm
                .get(n)
                .or_else(|| i.warm.last())
                .cloned()
                .unwrap_or_default()
        });
        Box::pin(async move {
            if !script.delay.is_zero() {
                tokio::time::sleep(script.delay).await;
            }
            script.outcome
        })
    }

    fn locate(
        &self,
        _app: AppId,
        _info: &AppInfo,
        focus: NodeKey,
    ) -> BoxFuture<'static, Result<Option<NodeKey>, SourceError>> {
        let (root, delay) = self.with(|i| {
            i.locate_calls += 1;
            (i.docs.get(&focus).copied(), i.locate_delay)
        });
        Box::pin(async move {
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            Ok(root)
        })
    }

    fn create(
        &self,
        _app: AppId,
        _info: &AppInfo,
        root: NodeKey,
    ) -> BoxFuture<'static, Result<Box<dyn DocSource>, SourceError>> {
        let (src, handle) = ScriptedDocSource::new();
        self.with(|i| i.created.push((root, handle)));
        Box::pin(async move { Ok(Box::new(src) as Box<dyn DocSource>) })
    }
}
