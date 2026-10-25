// SPDX-License-Identifier: GPL-3.0-or-later
//! App actor (ADR-0001 section 6.1): one per application. Owns the mirror writer, drains the
//! ingress, applies the invalidation contract of B-1 section 5.1 to the mirror, tracks
//! `AppHealth`, runs the shared fill driver and routes user-relevant events to the Reader.
//!
//! > "Per-event cost in the app actor is O(1) (coalescer drain plus routing). Reply decoding and
//! > subtree builds fan out to `cpu`."
//! > "background: drop oldest non-focus event, set `resync_needed`. Focus lane: coalesced in the
//! > app actor; `FocusChanged` is never dropped."  (section 6.2)
//!
//! [`AppActor::pump`] is the synchronous body of one actor iteration; the async task around it
//! (`runtime::run_app_actor`: wait on [`AppActor::ready`], pump, send the requests the fill driver
//! issued under their deadlines, feed replies back) is in the runtime. tokio's bounded channel
//! cannot drop its oldest element, so a full background lane drops the *newest* event and sets
//! `resync_needed`; that is a stated deviation from "drop oldest".

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_connector_api::{AppControl, BackendError, BackendReply};
use gorca_ingress::{DirtySet, IngressReceiver};
use gorca_mirror::driver::{
    DriverCx, FillDriver, FillError, FillReply, FillStats, GlobalRate, HealthSignal, IssuedFill,
    ReplyReport,
};
use gorca_mirror::fill::{
    AppClass, DemandOutcome, DemandWhy, FillInput, FillPolicy, FillReqId, HealthView, NullPolicy,
};
use gorca_mirror::{FillLimits, MirrorHandle, MirrorRead, MirrorWriter, RemovalCause, SweepStats};
use gorca_model::sem::SemRole;
use gorca_model::{
    AppId, AppLifecycle, NodeEvent, NodeKey, NodeSnapshot, PropMask, SnapshotBatch, WindowKind,
};
use gorca_rt::mpsc;
use smallvec::SmallVec;

use crate::builtin_ext::AppProfile;
use crate::health::{AppHealth, HealthEffect, HealthMsg, HealthStepCtx};
use crate::reader::{BackgroundMsg, FocusLaneMsg};
use crate::text::TextMsg;

mod doc_route;
pub mod fuse;
#[cfg(test)]
mod tests;

pub use doc_route::WebLink;

/// Cap on control messages handled per pump, so one app cannot monopolise a worker.
const MAX_CONTROL_PER_PUMP: usize = 64;
/// Fill messages (Reader demands, Reader fetch replies) handled per pump.
const MAX_FILL_MSGS_PER_PUMP: usize = 128;
/// Queue of Reader-to-actor fill messages.
pub const FILL_QUEUE: usize = 256;
/// The fields a property event can invalidate. `BOUNDS` joined for review (ADR-0006 17, IC-24).
const EVENTED: PropMask = PropMask::NAME
    .union(PropMask::DESCRIPTION)
    .union(PropMask::VALUE)
    .union(PropMask::ROLE)
    .union(PropMask::STATES)
    .union(PropMask::ATTRS)
    .union(PropMask::BOUNDS);
/// The fields the fused entry is made of: a property event on one of them invalidates the
/// fusion engine's registers too (M22a review W-6).
const FUSED_FIELDS: PropMask = PropMask::NAME
    .union(PropMask::DESCRIPTION)
    .union(PropMask::VALUE)
    .union(PropMask::ROLE)
    .union(PropMask::STATES)
    .union(PropMask::ATTRS);
/// Known ancestors whose name may derive from content that a NAME change invalidates (P9-3).
const NAME_ANCESTOR_LEVELS: usize = 8;
/// Pause before a fusion step that ran out of budget continues.
const FUSE_RESUME: Duration = Duration::from_millis(1);
// The engine's aging bound (8 budget-cut runs) assumes `run` is re-armed within 4 ms.
const _: () = assert!(FUSE_RESUME.as_millis() <= 4);
/// On-read TTL of the Shell; every other app uses [`gorca_mirror::DEFAULT_TTL`] (B-1 2.2).
const SHELL_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// What the Reader tells an app actor about how it used the mirror, and the result of its own
/// fetches (the shared cold path), so the actor can apply them to the mirror it owns.
#[derive(Debug)]
pub enum FillMsg {
    Demand {
        node: NodeKey,
        why: DemandWhy,
        outcome: DemandOutcome,
    },
    /// The focus state machine reached `Presented` for `node`.
    FocusSettled { node: NodeKey },
    /// A Reader fetch finished. `issued_at` is the mirror write sequence read when it was
    /// issued; the reply is applied against it (late-reply rules).
    ReaderFetch {
        node: NodeKey,
        issued_at: u64,
        result: Result<Arc<NodeSnapshot>, BackendError>,
    },
    /// Review fetched nodes (object navigation, model builds, revalidation): apply them to the
    /// mirror through the shared driver (ADR-0006 OB-5), then acknowledge, so the Reader never
    /// reads the mirror before the nodes it was told about are in it.
    ReviewApply {
        node: NodeKey,
        issued_at: u64,
        result: Result<SnapshotBatch, BackendError>,
        ack: Option<gorca_rt::oneshot::Sender<()>>,
    },
}

/// Where an app actor sends the caret, text and selection events of the focused app (ADR-0007
/// EV-3). `dropped` counts events the full queue refused: the Reader then distrusts its window.
#[derive(Clone, Debug)]
pub struct TextLink {
    pub tx: mpsc::Sender<TextMsg>,
    pub dropped: Arc<std::sync::atomic::AtomicU64>,
}

/// The Reader-facing half the actor holds.
#[derive(Clone, Debug)]
pub struct ReaderLinks {
    pub focus: mpsc::Sender<FocusLaneMsg>,
    pub background: mpsc::Sender<BackgroundMsg>,
}

/// What one pump did.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct AppRoute {
    pub events: usize,
    pub forwarded_to_focus_lane: usize,
    pub forwarded_to_background: usize,
    /// Background events dropped because the lane was full (`resync_needed` is set).
    pub dropped_background: usize,
    /// Text events of a focused-looking app that the Reader did not want (focus not protected).
    pub text_unprotected: usize,
    /// Nodes to resync lazily after a flood.
    pub resync: Option<DirtySet>,
    pub gone: bool,
    /// Fill requests the shell must send now, under their deadlines.
    pub issued: Vec<IssuedFill>,
}

pub fn health_view(h: &AppHealth) -> HealthView {
    match h {
        AppHealth::Healthy => HealthView::Healthy,
        AppHealth::Slow { .. } => HealthView::Slow,
        AppHealth::Hung { .. } => HealthView::Hung,
        AppHealth::Flooded { .. } => HealthView::Flooded,
        AppHealth::Gone => HealthView::Gone,
    }
}

fn fill_error(e: &BackendError) -> FillError {
    match e {
        BackendError::Timeout => FillError::Timeout,
        BackendError::Gone => FillError::Gone,
        BackendError::NotSupported => FillError::NotSupported,
        BackendError::Busy => FillError::Busy,
        BackendError::Protocol(_) => FillError::Protocol,
        BackendError::Unresolved => FillError::Unresolved,
    }
}

fn fill_reply(r: Result<BackendReply, BackendError>) -> FillReply {
    match r {
        Ok(BackendReply::Node(n)) => FillReply::Ok(SnapshotBatch::new(vec![n])),
        Ok(BackendReply::Subtree(b)) => FillReply::Ok(b),
        Ok(
            BackendReply::Children(_)
            | BackendReply::Text(_)
            | BackendReply::CaretOffset(_)
            | BackendReply::TextAttributes(_)
            | BackendReply::ActionDone
            | BackendReply::Hit(_)
            | BackendReply::Matches(_)
            | BackendReply::Lines(_)
            | BackendReply::Unit(_)
            | BackendReply::Selections(_)
            | BackendReply::Format(_)
            | BackendReply::Target(_)
            | BackendReply::Window(_),
        ) => FillReply::Err(FillError::Protocol),
        Err(e) => FillReply::Err(fill_error(&e)),
    }
}

#[derive(Debug)]
// Four independent latches (resync, two closed channels, shell); an enum would only rename them.
#[allow(clippy::struct_excessive_bools)]
pub struct AppActor {
    app: AppId,
    health: AppHealth,
    writer: MirrorWriter,
    read: MirrorHandle,
    ingress: IngressReceiver,
    control: mpsc::Receiver<AppControl>,
    links: ReaderLinks,
    resync_needed: bool,
    /// A `FocusChanged` that could not be queued yet; retried first on the next pump.
    pending_focus: Option<NodeKey>,
    /// A control message taken by [`AppActor::ready`], handled first by the next pump.
    pending_control: Option<AppControl>,
    control_closed: bool,
    profile: AppProfile,
    driver: FillDriver,
    fill_rx: Option<mpsc::Receiver<FillMsg>>,
    pending_fill: Option<FillMsg>,
    fill_closed: bool,
    /// The focus last routed to the Reader from this app (S1 and the policy's focus).
    focus: Option<NodeKey>,
    /// The connector verified this app as the Shell.
    shell: bool,
    /// The focused application, published by the Reader: it is protected like the Shell.
    focused: Option<Arc<gorca_rt::FocusedApp>>,
    /// Routes a web application's events to its documents (browse mode).
    web: Option<doc_route::DocRouter>,
    /// Text events go to the Reader through this, when the runtime wired it (ADR-0007 EV-3).
    text: Option<TextLink>,
    text_unprotected: u64,
    /// Where the app's text capability is published for the Reader.
    directory: Option<Arc<crate::directory::MirrorDirectory>>,
    /// Bounds the `showing` events forwarded to the Reader (M4b).
    showing_budget: ShowingBudget,
    level_budget: ShowingBudget,
    /// The extension host and the extensions that matched this app (ADR-0001 section 10).
    ext: Option<Arc<dyn crate::ExtSource>>,
    app_ext: Option<Arc<dyn crate::AppExt>>,
    /// The registers and the fusion engine of this app (M22a-C1b), fed from the source records
    /// of the snapshots this actor applies to its mirror.
    fuse: fuse::FuseWriter,
    /// The plugin runtime (M18a); `None` when plugins are off.
    plugin_service: Option<Arc<dyn crate::plugin_port::PluginService>>,
}

/// `showing` events forwarded per [`SHOWING_WINDOW`]: a banner is one event, an overview maps
/// hundreds.
const SHOWING_BURST: u32 = 64;
const SHOWING_WINDOW: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
struct ShowingBudget {
    since: Option<Instant>,
    used: u32,
}

impl ShowingBudget {
    fn take(&mut self, now: Instant) -> bool {
        match self.since {
            Some(at) if now.saturating_duration_since(at) < SHOWING_WINDOW => {}
            Some(_) | None => {
                self.since = Some(now);
                self.used = 0;
            }
        }
        if self.used < SHOWING_BURST {
            self.used += 1;
            true
        } else {
            false
        }
    }
}

impl AppActor {
    pub fn new(
        app: AppId,
        writer: MirrorWriter,
        ingress: IngressReceiver,
        control: mpsc::Receiver<AppControl>,
        links: ReaderLinks,
    ) -> Self {
        let read = writer.reader();
        Self {
            app,
            health: AppHealth::Healthy,
            writer,
            read,
            ingress,
            control,
            links,
            resync_needed: false,
            pending_focus: None,
            pending_control: None,
            control_closed: false,
            profile: AppProfile::default(),
            driver: FillDriver::new(
                app,
                AppClass::Unknown,
                Box::new(NullPolicy),
                FillLimits::default(),
                None,
            ),
            fill_rx: None,
            pending_fill: None,
            fill_closed: false,
            focus: None,
            shell: false,
            showing_budget: ShowingBudget::default(),
            level_budget: ShowingBudget::default(),
            focused: None,
            web: None,
            text: None,
            text_unprotected: 0,
            directory: None,
            ext: None,
            app_ext: None,
            fuse: fuse::FuseWriter::new(None),
            plugin_service: None,
        }
    }

    /// Give the fusion writer of this app the plugin runtime (M18a, ADR-0026 K1).
    #[must_use]
    pub fn with_plugins(mut self, svc: Arc<dyn crate::plugin_port::PluginService>) -> Self {
        self.plugin_service = Some(svc);
        self
    }

    /// Offer this app's events to the extensions that match it (`on-event`).
    #[must_use]
    pub fn with_ext(mut self, src: Arc<dyn crate::ExtSource>) -> Self {
        self.ext = Some(src);
        self
    }

    /// Publish what the app can do with text (its offset unit) to the Reader's directory when it
    /// announces itself.
    #[must_use]
    pub fn with_directory(mut self, dir: Arc<crate::directory::MirrorDirectory>) -> Self {
        self.directory = Some(dir);
        self
    }

    /// Forward the caret, text and selection events of this app to the Reader while it holds
    /// the focus.
    #[must_use]
    pub fn with_text(mut self, link: TextLink) -> Self {
        self.text = Some(link);
        self
    }

    /// Does the Reader want text events from this app? Only the application that holds the
    /// user's focus (or, when nobody published one, one that has a focus of its own).
    fn wants_text_events(&self) -> bool {
        match &self.focused {
            Some(f) => f.protects(self.app),
            None => self.focus.is_some(),
        }
    }

    /// Forward a text event to the Reader when this app holds the user's focus. An app that has a
    /// focus of its own but is not (yet) protected loses the event: that is counted, so a focus
    /// that never earned its protection shows in the numbers instead of as silence.
    fn route_text(&mut self, msg: TextMsg, route: &mut AppRoute) {
        if self.text.is_none() {
            return;
        }
        if self.wants_text_events() {
            self.send_text(msg);
        } else if self.focus.is_some() {
            route.text_unprotected += 1;
            self.text_unprotected = self.text_unprotected.saturating_add(1);
        }
    }

    /// Text events of this app with a focus that were not forwarded because the Reader had not
    /// (yet) published the app as the focus owner.
    pub fn text_events_unprotected(&self) -> u64 {
        self.text_unprotected
    }

    /// Text events were lost on the way (a flood collapsed them): say so, like a full queue does.
    fn note_text_lost(&self) {
        if let Some(l) = &self.text {
            l.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Queue a text event for the Reader; a full queue drops it and says so.
    fn send_text(&mut self, msg: TextMsg) {
        let Some(l) = &self.text else { return };
        if l.tx.try_send(msg).is_err() {
            l.dropped.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Route this application's events to its documents and its lifecycle to the registry.
    #[must_use]
    pub fn with_web(mut self, link: WebLink) -> Self {
        self.web = Some(doc_route::DocRouter::new(self.app, link));
        self
    }

    /// The focused-application cell: while this app holds the user's focus its mirror is not
    /// asked to shed for other apps.
    #[must_use]
    pub fn with_focused(mut self, focused: Arc<gorca_rt::FocusedApp>) -> Self {
        self.focused = Some(focused);
        self
    }

    fn refresh_protection(&mut self) {
        let focused = self.focused.as_ref().is_some_and(|f| f.protects(self.app));
        self.writer.set_protected(self.shell || focused);
    }

    /// Choose the fill policy and its limits; share the all-apps rate cap.
    #[must_use]
    pub fn with_policy(
        mut self,
        policy: Box<dyn FillPolicy>,
        limits: FillLimits,
        global: Option<Arc<GlobalRate>>,
    ) -> Self {
        self.driver = FillDriver::new(self.app, AppClass::Unknown, policy, limits, global);
        self
    }

    /// The channel the Reader (through the runtime) uses to tell this actor about demands and
    /// its own fetch replies.
    #[must_use]
    pub fn with_fill_channel(mut self, rx: mpsc::Receiver<FillMsg>) -> Self {
        self.fill_rx = Some(rx);
        self
    }

    /// The per-application behaviour chosen when the app announced itself.
    pub fn profile(&self) -> AppProfile {
        self.profile
    }

    /// Resolves when a [`pump`](Self::pump) has something to do: events were pushed into an
    /// empty ingress, the connector sent a control message, or the Reader sent a fill message.
    /// A connector that dropped its port without saying `Gone` is treated as gone. Cancel-safe.
    pub async fn ready(&mut self) {
        if self.pending_control.is_some() || self.pending_fill.is_some() {
            return;
        }
        let fill = async {
            match self.fill_rx.as_mut() {
                Some(rx) if !self.fill_closed => rx.recv().await,
                Some(_) | None => std::future::pending().await,
            }
        };
        if self.control_closed {
            tokio::select! {
                () = self.ingress.notified() => {}
                m = fill => self.take_fill(m),
            }
            return;
        }
        tokio::select! {
            () = self.ingress.notified() => {}
            msg = self.control.recv() => {
                if let Some(m) = msg {
                    self.pending_control = Some(m);
                } else {
                    self.control_closed = true;
                    self.pending_control = Some(AppControl::Lifecycle(AppLifecycle::Gone));
                }
            }
            m = fill => self.take_fill(m),
        }
    }

    fn take_fill(&mut self, m: Option<FillMsg>) {
        match m {
            Some(m) => self.pending_fill = Some(m),
            None => self.fill_closed = true,
        }
    }

    pub fn app(&self) -> AppId {
        self.app
    }

    pub fn health(&self) -> &AppHealth {
        &self.health
    }

    pub fn resync_needed(&self) -> bool {
        self.resync_needed
    }

    /// Is this app's mirror protected from shedding for other apps (focused app or Shell)?
    pub fn mirror_protected(&self) -> bool {
        self.writer.usage().protected()
    }

    /// Read handle of this app's mirror.
    /// The counters of this app's fusion writer (M22a-C1b).
    pub fn fuse_stats(&self) -> fuse::FuseStatsView {
        self.fuse.stats().view()
    }

    /// Nodes the fusion engine holds.
    pub fn fuse_nodes(&self) -> usize {
        self.fuse.node_count()
    }

    /// Snapshots queued, entries waiting for the mirror and nodes still dirty: zero when the
    /// writer is idle.
    pub fn fuse_work_left(&self) -> usize {
        self.fuse.work_left()
    }

    pub fn mirror(&self) -> &MirrorHandle {
        &self.read
    }

    pub fn fill_stats(&self) -> FillStats {
        self.driver.stats()
    }

    pub fn fill_policy(&self) -> &'static str {
        self.driver.policy_name()
    }

    /// Feed a health message (timeouts and successes come from the request mux).
    pub fn on_health(&mut self, msg: HealthMsg, now: Instant) -> SmallVec<[HealthEffect; 3]> {
        let (next, effects) =
            std::mem::replace(&mut self.health, AppHealth::Gone).step(msg, &HealthStepCtx { now });
        self.health = next;
        for e in &effects {
            if matches!(e, HealthEffect::AnnounceNotResponding) {
                self.send_focus(FocusLaneMsg::AppHung(self.app));
            }
        }
        effects
    }

    fn send_focus(&mut self, msg: FocusLaneMsg) -> bool {
        self.links.focus.try_send(msg).is_ok()
    }

    /// Requests the shell must send now (also returned by [`AppActor::pump`]).
    pub fn take_issued(&mut self) -> Vec<IssuedFill> {
        self.driver.take_issued()
    }

    /// When the shell should call [`AppActor::poll_timers`] next.
    pub fn next_wake(&self) -> Option<Instant> {
        if matches!(self.health, AppHealth::Gone) {
            None
        } else {
            let fuse = self.fuse.has_work().then(|| Instant::now() + FUSE_RESUME);
            match (self.driver.next_wake(), fuse) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            }
        }
    }

    pub fn poll_timers(&mut self, now: Instant) -> SweepStats {
        let health = health_view(&self.health);
        let cx = DriverCx {
            now,
            health,
            mirror: &self.read,
            focus: self.focus,
        };
        self.driver.poll_timers(&cx, &mut self.writer);
        self.writer.sweep(now)
    }

    /// A fill request finished (the shell awaited it under its deadline).
    pub fn on_fill_reply(
        &mut self,
        id: FillReqId,
        issued_at: u64,
        result: Result<BackendReply, BackendError>,
        now: Instant,
    ) -> SmallVec<[HealthEffect; 3]> {
        let health = health_view(&self.health);
        let cx = DriverCx {
            now,
            health,
            mirror: &self.read,
            focus: self.focus,
        };
        match &result {
            Ok(BackendReply::Node(n)) => self.fuse.enqueue(std::slice::from_ref(n), issued_at),
            Ok(BackendReply::Subtree(b)) => self.fuse.enqueue(&b.nodes, issued_at),
            Ok(_) | Err(_) => {}
        }
        let report = self
            .driver
            .on_reply(&cx, &mut self.writer, id, issued_at, fill_reply(result));
        self.health_effects(&report, now)
    }

    fn health_effects(
        &mut self,
        report: &ReplyReport,
        now: Instant,
    ) -> SmallVec<[HealthEffect; 3]> {
        match report.health {
            HealthSignal::None => SmallVec::new(),
            HealthSignal::Success => self.on_health(HealthMsg::Success, now),
            HealthSignal::Timeout(class) => self.on_health(HealthMsg::Timeout { class }, now),
        }
    }

    /// One iteration: control messages, Reader fill messages, then the ingress.
    pub fn pump(&mut self, now: Instant) -> AppRoute {
        let mut route = AppRoute::default();
        self.refresh_protection();
        self.pump_control(now, &mut route);
        self.pump_fill(now);

        if let Some(node) = self.pending_focus.take() {
            if self.send_focus(FocusLaneMsg::FocusChanged { node }) {
                route.forwarded_to_focus_lane += 1;
            } else {
                self.pending_focus = Some(node);
            }
        }

        let drained = self.ingress.drain();
        route.events = drained.events.len();
        if let Some(r) = &mut self.web {
            r.route(&drained.events, now);
        }
        // 5.1, step 1: the invalidation table, applied before the Reader sees the batch.
        self.invalidate_for(&drained.events, drained.dirty.as_ref(), now);
        if let Some(dirty) = drained.dirty {
            // Flooded: the ingress kept only the essentials and marked the rest dirty. We have
            // drained the backlog, so the app recovers with a lazy resync (R-3).
            if self.text.is_some() && self.wants_text_events() {
                // The text events of the flood are gone: the Reader must not trust its window or
                // its screen (a terminal reads it again).
                self.note_text_lost();
            }
            let ctx = HealthStepCtx { now };
            let (h, _) = std::mem::replace(&mut self.health, AppHealth::Gone)
                .step(HealthMsg::IngressOverWatermark { dirty }, &ctx);
            let (h, effects) = h.step(HealthMsg::BacklogDrained, &ctx);
            self.health = h;
            for e in effects {
                match e {
                    HealthEffect::Resync(d) => route.resync = Some(d),
                    HealthEffect::HalveConcurrency
                    | HealthEffect::RestoreConcurrency
                    | HealthEffect::RestrictToCache
                    | HealthEffect::AnnounceNotResponding
                    | HealthEffect::SendProbe
                    | HealthEffect::DeregisterEvents
                    | HealthEffect::ReRegisterEvents
                    | HealthEffect::TombstoneAll
                    | HealthEffect::NotifyGone => {}
                }
            }
        }
        // 5.1, step 2: route to the Reader. Only the newest "became selected" of a batch is
        // worth speaking: the older ones are superseded before the user could hear them.
        let last_selected = if self.profile.speak_selection {
            drained.events.iter().rposition(became_selected)
        } else {
            None
        };
        let mut s1_sent = SmallVec::<[NodeKey; 2]>::new();
        for (i, ev) in drained.events.iter().enumerate() {
            if self.ext_verdict(ev, &mut route) {
                continue;
            }
            if self.profile.speak_selection && became_selected(ev) {
                if Some(i) == last_selected
                    && let NodeEvent::PropertiesChanged { node, .. } = ev
                {
                    if self.send_focus(FocusLaneMsg::ItemSelected { node: *node }) {
                        route.forwarded_to_focus_lane += 1;
                    } else {
                        self.resync_needed = true;
                    }
                } else {
                    self.resync_needed = true;
                }
                continue;
            }
            self.route(ev, &mut route, &mut s1_sent);
        }
        // 5.1, step 3: the policy.
        self.run_policy(&drained.events, route.resync.as_ref(), now);
        if !route.gone && !matches!(self.health, AppHealth::Gone) {
            self.poll_timers(now);
        }
        route.issued = self.driver.take_issued();
        // Last, after the events are routed: the fusion of what this pump applied (bounded).
        self.fuse.step(&mut self.writer, fuse::STEP_BUDGET);
        route
    }

    fn run_policy(&mut self, events: &[NodeEvent], dirty: Option<&DirtySet>, now: Instant) {
        let health = health_view(&self.health);
        let cx = DriverCx {
            now,
            health,
            mirror: &self.read,
            focus: self.focus,
        };
        if let Some(d) = dirty {
            self.driver
                .on_input(&cx, &mut self.writer, FillInput::Resync { dirty: d });
        }
        for ev in events {
            if let NodeEvent::WindowEvent {
                node,
                kind: WindowKind::Activate,
            } = ev
            {
                self.driver.on_input(
                    &cx,
                    &mut self.writer,
                    FillInput::WindowActivated { window: *node },
                );
            }
        }
        if !events.is_empty() {
            self.driver
                .on_input(&cx, &mut self.writer, FillInput::Events(events));
        }
    }

    fn pump_fill(&mut self, now: Instant) {
        for _ in 0..MAX_FILL_MSGS_PER_PUMP {
            let next = match self.pending_fill.take() {
                Some(m) => Some(m),
                None => self.fill_rx.as_mut().and_then(|rx| rx.try_recv().ok()),
            };
            let Some(msg) = next else { break };
            self.on_fill_msg(msg, now);
        }
    }

    fn on_fill_msg(&mut self, msg: FillMsg, now: Instant) {
        let health = health_view(&self.health);
        match msg {
            FillMsg::Demand { node, why, outcome } => {
                let cx = DriverCx {
                    now,
                    health,
                    mirror: &self.read,
                    focus: self.focus,
                };
                self.driver.on_input(
                    &cx,
                    &mut self.writer,
                    FillInput::Demand { node, why, outcome },
                );
            }
            FillMsg::FocusSettled { node } => {
                if node.app == self.app {
                    self.focus = Some(node);
                    self.writer.set_focus(Some(node));
                    self.fuse.set_focus(Some(node));
                }
                let cx = DriverCx {
                    now,
                    health,
                    mirror: &self.read,
                    focus: self.focus,
                };
                self.driver
                    .on_input(&cx, &mut self.writer, FillInput::FocusSettled { node });
            }
            FillMsg::ReviewApply {
                node,
                issued_at,
                result,
                ack,
            } => {
                let cx = DriverCx {
                    now,
                    health,
                    mirror: &self.read,
                    focus: self.focus,
                };
                let reply = match result {
                    Ok(b) => {
                        self.fuse.enqueue(&b.nodes, issued_at);
                        FillReply::Ok(b)
                    }
                    Err(e) => FillReply::Err(fill_error(&e)),
                };
                self.driver
                    .on_external_reply(&cx, &mut self.writer, node, issued_at, reply);
                if let Some(a) = ack {
                    let _ = a.send(());
                }
            }
            FillMsg::ReaderFetch {
                node,
                issued_at,
                result,
            } => {
                let cx = DriverCx {
                    now,
                    health,
                    mirror: &self.read,
                    focus: self.focus,
                };
                let reply = match result {
                    Ok(n) => {
                        self.fuse.enqueue(std::slice::from_ref(&n), issued_at);
                        FillReply::Ok(SnapshotBatch::new(vec![n]))
                    }
                    Err(e) => FillReply::Err(fill_error(&e)),
                };
                self.driver
                    .on_external_reply(&cx, &mut self.writer, node, issued_at, reply);
            }
        }
    }

    fn pump_control(&mut self, now: Instant, route: &mut AppRoute) {
        for _ in 0..MAX_CONTROL_PER_PUMP {
            let next = match self.pending_control.take() {
                Some(m) => Ok(m),
                None => self.control.try_recv(),
            };
            match next {
                Ok(AppControl::Snapshot(batch)) => {
                    self.fuse.enqueue(&batch.nodes, self.writer.seq());
                    self.driver.apply_push(&mut self.writer, batch);
                }
                Ok(AppControl::Lifecycle(AppLifecycle::Gone)) => {
                    if let Some(src) = &self.ext {
                        src.detach(self.app);
                    }
                    self.app_ext = None;
                    if let Some(r) = &self.web {
                        r.lifecycle(&AppLifecycle::Gone);
                    }
                    self.on_health(HealthMsg::OwnerVanished, now);
                    self.send_focus(FocusLaneMsg::AppGone(self.app));
                    // 5.1: the whole mirror and its tombstones are released.
                    self.driver.on_gone();
                    self.writer.retire_all();
                    route.gone = true;
                }
                Ok(AppControl::Lifecycle(AppLifecycle::Appeared(info))) => {
                    if let Some(r) = &self.web {
                        r.lifecycle(&AppLifecycle::Appeared(info.clone()));
                    }
                    self.profile = AppProfile::for_app(&info);
                    self.fuse.set_tables(info.sem_tables);
                    if let Some(svc) = &self.plugin_service {
                        let (app, verified) =
                            crate::plugin_port::scope_identity(&info.app_name, info.exe.as_ref());
                        self.fuse.set_plugins(
                            Arc::clone(svc),
                            crate::plugin_port::ScopeSpec {
                                app,
                                url: None,
                                engine: 0,
                                verified: verified || info.verified_shell,
                                doc: false,
                            },
                        );
                    }
                    if let Some(src) = &self.ext {
                        self.app_ext = src.attach(self.app, &info);
                    }
                    if let Some(d) = &self.directory {
                        d.set_text_unit(self.app, self.profile.text_unit);
                        d.set_text_insert_slack(self.app, self.profile.text_insert_slack);
                        d.set_edit_quirks(self.app, self.profile.edit_quirks);
                        d.set_page_events_settle(self.app, info.page_events_settle_ms);
                        d.set_pid(self.app, info.pid);
                        d.set_app_name(self.app, Some(&info.app_name));
                        d.set_policy(self.app, self.profile.policy);
                    }
                    self.driver.set_class(AppClass::from_info(&info));
                    self.driver.set_dump_complete(info.dump_complete);
                    self.shell = info.verified_shell;
                    self.refresh_protection();
                    self.writer.set_ttl(if info.verified_shell {
                        SHELL_TTL
                    } else {
                        gorca_mirror::DEFAULT_TTL
                    });
                    // The policy learns of the app (class and dump eligibility are already set,
                    // so its context carries the verified identity) before anything is focused.
                    // Announced again when the identity is verified late: it is told again.
                    if let Some(root) = info.root {
                        let cx = DriverCx {
                            now,
                            health: health_view(&self.health),
                            mirror: &self.read,
                            focus: self.focus,
                        };
                        self.driver
                            .on_input(&cx, &mut self.writer, FillInput::Appeared { root });
                    }
                }
                Ok(AppControl::Lifecycle(AppLifecycle::Ready)) => {
                    if let Some(r) = &self.web {
                        r.lifecycle(&AppLifecycle::Ready);
                    }
                    let health = health_view(&self.health);
                    let cx = DriverCx {
                        now,
                        health,
                        mirror: &self.read,
                        focus: self.focus,
                    };
                    self.driver
                        .on_input(&cx, &mut self.writer, FillInput::Ready);
                }
                Err(_) => break,
            }
        }
    }

    fn parent_of(&self, k: NodeKey) -> Option<NodeKey> {
        self.read.view(k).hit().and_then(|v| v.snap.parent())
    }

    /// Mark fields of `k` untrustworthy in the mirror and, when one of them feeds the fused entry,
    /// in the fusion engine too (M22a W-6): the entry must not read fresh next to a stale
    /// snapshot. The engine's invalidation carries the mirror sequence the invalidation got, so a
    /// reply issued after it (same or newer sequence) is fresh again and one issued before it is not.
    fn invalidate_node(&mut self, k: NodeKey, mask: PropMask, now: Instant) {
        self.writer.invalidate(k, mask, now);
        if mask.intersects(FUSED_FIELDS) {
            self.fuse.invalidate(k, self.writer.seq());
        }
    }

    /// B-1 5.1: the event to mirror-effect table (same for every policy).
    fn invalidate_for(&mut self, events: &[NodeEvent], dirty: Option<&DirtySet>, now: Instant) {
        if let Some(d) = dirty {
            if d.is_all() {
                self.writer.invalidate_all(now);
                self.fuse.invalidate_all(self.writer.seq());
            } else {
                for k in d.iter() {
                    self.invalidate_node(k, PropMask::ALL, now);
                }
            }
        }
        for ev in events {
            match ev {
                NodeEvent::PropertiesChanged { node, mask } => {
                    let m = *mask & EVENTED;
                    if !m.is_empty() {
                        self.invalidate_node(*node, m, now);
                    }
                    if m.contains(PropMask::NAME) {
                        self.invalidate_name_ancestors(*node, now);
                    }
                }
                NodeEvent::ChildrenChanged { node } => {
                    self.invalidate_node(*node, PropMask::CHILDREN, now);
                }
                NodeEvent::NodeRemoved { node } => {
                    let parent = self.parent_of(*node);
                    self.writer.tombstone(*node, now, RemovalCause::Event);
                    self.fuse.forget(*node);
                    if let Some(p) = parent {
                        self.invalidate_node(p, PropMask::CHILDREN, now);
                    }
                }
                NodeEvent::TextChanged { node, .. } => {
                    self.writer
                        .invalidate(*node, PropMask::NAME | PropMask::VALUE, now);
                }
                NodeEvent::ActiveDescendantChanged { container, .. } => {
                    self.invalidate_node(*container, PropMask::STATES, now);
                }
                NodeEvent::WindowEvent { node, kind } => match kind {
                    WindowKind::Destroy => {
                        self.writer
                            .tombstone(*node, now, RemovalCause::WindowDestroyed);
                        self.fuse.forget(*node);
                    }
                    WindowKind::Activate | WindowKind::Deactivate | WindowKind::Create => {
                        self.invalidate_node(*node, PropMask::STATES | PropMask::NAME, now);
                    }
                },
                NodeEvent::FocusChanged { .. }
                | NodeEvent::CaretMoved { .. }
                | NodeEvent::SelectionChanged { .. }
                | NodeEvent::DocumentEvent { .. }
                | NodeEvent::Announcement { .. } => {}
            }
        }
    }

    /// P9-3: a changed name may be the name of ancestors that derive theirs from content.
    fn invalidate_name_ancestors(&mut self, n: NodeKey, now: Instant) {
        let mut cur = self.parent_of(n);
        for _ in 0..NAME_ANCESTOR_LEVELS {
            let Some(a) = cur else { break };
            let Some(v) = self.read.view(a).hit() else {
                break;
            };
            self.invalidate_node(a, PropMask::NAME, now);
            if !crate::nodekind::role_of(&v.snap).takes_name_from_content() {
                break;
            }
            cur = v.snap.parent();
        }
    }

    fn route(
        &mut self,
        ev: &NodeEvent,
        route: &mut AppRoute,
        s1_sent: &mut SmallVec<[NodeKey; 2]>,
    ) {
        match ev {
            NodeEvent::FocusChanged { node } => {
                self.focus = Some(*node);
                self.writer.set_focus(Some(*node));
                self.fuse.set_focus(Some(*node));
                if self.send_focus(FocusLaneMsg::FocusChanged { node: *node }) {
                    route.forwarded_to_focus_lane += 1;
                } else {
                    // Never dropped: retry on the next pump.
                    self.pending_focus = Some(*node);
                }
            }
            NodeEvent::NodeRemoved { node } => {
                if self.send_focus(FocusLaneMsg::NodeRemoved(*node)) {
                    route.forwarded_to_focus_lane += 1;
                } else {
                    self.resync_needed = true;
                }
            }
            NodeEvent::Announcement {
                text, politeness, ..
            } => {
                self.forward_background(
                    BackgroundMsg::Announcement {
                        app: self.app,
                        text: Arc::clone(text),
                        politeness: *politeness,
                    },
                    route,
                );
            }
            // S1: a state or value change of the focused node is spoken (PW-12).
            NodeEvent::PropertiesChanged { node, mask } => {
                if mask.contains(PropMask::SHOWING_NOW) {
                    self.route_showing(*node, route);
                }
                if mask.contains(PropMask::VALUE) && self.focus != Some(*node) {
                    self.route_level(*node, route);
                }
                // A name change rides along: the Reader speaks it only within a short window after
                // the focus (a label a page sets after the focus event).
                if self.focus == Some(*node)
                    && mask.intersects(PropMask::STATES | PropMask::VALUE | PropMask::NAME)
                    && !s1_sent.contains(node)
                {
                    s1_sent.push(*node);
                    if self.send_focus(FocusLaneMsg::StateChanged {
                        node: *node,
                        mask: *mask & (PropMask::STATES | PropMask::VALUE | PropMask::NAME),
                    }) {
                        route.forwarded_to_focus_lane += 1;
                    } else {
                        self.resync_needed = true;
                    }
                }
            }
            // Caret, text and selection events matter to the Reader only for the focused app.
            NodeEvent::CaretMoved { node, offset, .. } => {
                self.route_text(
                    TextMsg::Caret {
                        node: *node,
                        offset: *offset,
                    },
                    route,
                );
            }
            NodeEvent::TextChanged { node, delta } => {
                self.route_text(
                    TextMsg::Edit {
                        node: *node,
                        delta: delta.clone(),
                    },
                    route,
                );
            }
            NodeEvent::SelectionChanged { node } => {
                self.route_text(TextMsg::Selection { node: *node }, route);
            }
            // A window became the active one: its title leads the focus speech (M4b). Best
            // effort: a full focus lane loses the title, never the focus.
            NodeEvent::WindowEvent {
                node,
                kind: WindowKind::Activate,
            } => {
                if self.send_focus(FocusLaneMsg::WindowActivated { window: *node }) {
                    route.forwarded_to_focus_lane += 1;
                }
            }
            // Events that only dirty the mirror, which `invalidate_for` already did.
            NodeEvent::ActiveDescendantChanged { .. }
            | NodeEvent::ChildrenChanged { .. }
            | NodeEvent::WindowEvent { .. }
            | NodeEvent::DocumentEvent { .. } => {}
        }
    }

    /// A node became visible (M4b): the Reader reads it when it may be a notice. A node the
    /// mirror knows to be anything but an alert is dropped here, at no cost; the rest is bounded
    /// (a Shell overview maps hundreds of actors at once) and dropped silently when over budget,
    /// which is not a lost event and never asks for a resync.
    fn route_showing(&mut self, node: NodeKey, route: &mut AppRoute) {
        let known = self.read.view(node).hit();
        let alert = known
            .as_ref()
            .is_some_and(|v| crate::present::is_notice_node(&v.snap));
        if !(alert || (known.is_none() && self.profile.watch_showing)) {
            return;
        }
        if self.showing_budget.take(Instant::now()) {
            self.forward_background_lossy(BackgroundMsg::Showing { node }, route);
        }
    }

    /// The value of a node that does not hold focus changed (M4b): when it may be a level bar
    /// the Reader reads it back, once it stops changing. Only the verified Shell is watched; a
    /// node the mirror knows to be anything but a level or slider costs nothing.
    fn route_level(&mut self, node: NodeKey, route: &mut AppRoute) {
        if !self.profile.watch_showing {
            return;
        }
        let level = self.read.view(node).hit().is_none_or(|v| {
            // A level bar is a `meter` to the model.
            matches!(
                crate::nodekind::role_of(&v.snap),
                SemRole::ProgressBar | SemRole::Slider | SemRole::Meter
            )
        });
        if level && self.level_budget.take(Instant::now()) {
            self.forward_background_lossy(BackgroundMsg::LevelChanged { node }, route);
        }
    }

    /// Like [`Self::forward_background`] for a message whose loss costs nothing.
    fn forward_background_lossy(&mut self, msg: BackgroundMsg, route: &mut AppRoute) {
        if self.links.background.try_send(msg).is_ok() {
            route.forwarded_to_background += 1;
        }
    }

    /// Run the `on-event` hooks for `ev`. True: the extensions consumed the event, so it is not
    /// routed. Focus and removal events are always routed whatever the verdict (their
    /// bookkeeping keeps the focus state machine and the mirror consistent); a `Rewrite` there
    /// still speaks.
    fn ext_verdict(&mut self, ev: &NodeEvent, route: &mut AppRoute) -> bool {
        let Some(app_ext) = &self.app_ext else {
            return false;
        };
        let bookkeeping = matches!(
            ev,
            NodeEvent::FocusChanged { .. } | NodeEvent::NodeRemoved { .. }
        );
        match app_ext.on_event(ev) {
            crate::ExtVerdict::Pass => false,
            crate::ExtVerdict::Suppress => !bookkeeping,
            crate::ExtVerdict::Replace(segs) => {
                if segs.iter().any(|s| !s.text.is_empty()) {
                    let ir = crate::ext::ext_words_ir(
                        segs.iter().map(|s| (s.kind, &*s.text)),
                        crate::ext::VIA_EXT_REWRITE,
                    );
                    self.forward_background(
                        BackgroundMsg::ExtSay {
                            ir: Box::new(ir),
                            interrupt: false,
                        },
                        route,
                    );
                }
                !bookkeeping
            }
            crate::ExtVerdict::ReEvaluate => {
                self.resync_needed = true;
                false
            }
        }
    }

    fn forward_background(&mut self, msg: BackgroundMsg, route: &mut AppRoute) {
        if self.links.background.try_send(msg).is_ok() {
            route.forwarded_to_background += 1;
        } else {
            route.dropped_background += 1;
            self.resync_needed = true;
        }
    }
}

/// `state-changed:selected(1)`, as the connector flags it.
fn became_selected(ev: &NodeEvent) -> bool {
    matches!(ev, NodeEvent::PropertiesChanged { mask, .. } if mask.contains(PropMask::SELECTED_NOW))
}
