//! App actor (ADR-0001 section 6.1): one per application. Owns the mirror writer, drains the
//! ingress, tracks `AppHealth` and routes user-relevant events to the Reader.
//!
//! > "Per-event cost in the app actor is O(1) (coalescer drain plus routing). Reply decoding and
//! > subtree builds fan out to `cpu`."
//! > "background: drop oldest non-focus event, set `resync_needed`. Focus lane: coalesced in the
//! > app actor; `FocusChanged` is never dropped."  (section 6.2)
//!
//! [`AppActor::pump`] is the synchronous body of one actor iteration; the async task around it
//! (wait on `IngressReceiver::notified`, then pump, with a deadline on every await) is TBD with
//! the supervisor. tokio's bounded channel cannot drop its oldest element, so a full background
//! lane drops the *newest* event and sets `resync_needed`; the Reader's view of that app is then
//! refreshed by a resync. That is a stated deviation from "drop oldest".

use std::time::Instant;

use gorca_connector_api::AppControl;
use gorca_ingress::{DirtySet, IngressReceiver};
use gorca_mirror::MirrorWriter;
use gorca_model::{AppId, AppLifecycle, NodeEvent, NodeKey, PropMask};
use gorca_rt::mpsc;
use smallvec::SmallVec;

use crate::builtin_ext::AppProfile;
use crate::health::{AppHealth, HealthEffect, HealthMsg, HealthStepCtx};
use crate::reader::{BackgroundMsg, FocusLaneMsg};

/// Cap on control messages handled per pump, so one app cannot monopolise a worker.
const MAX_CONTROL_PER_PUMP: usize = 64;

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
    /// Nodes to resync lazily after a flood.
    pub resync: Option<DirtySet>,
    pub gone: bool,
}

#[derive(Debug)]
pub struct AppActor {
    app: AppId,
    health: AppHealth,
    writer: MirrorWriter,
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
}

impl AppActor {
    pub fn new(
        app: AppId,
        writer: MirrorWriter,
        ingress: IngressReceiver,
        control: mpsc::Receiver<AppControl>,
        links: ReaderLinks,
    ) -> Self {
        Self {
            app,
            health: AppHealth::Healthy,
            writer,
            ingress,
            control,
            links,
            resync_needed: false,
            pending_focus: None,
            pending_control: None,
            control_closed: false,
            profile: AppProfile::default(),
        }
    }

    /// The per-application behaviour chosen when the app announced itself.
    pub fn profile(&self) -> AppProfile {
        self.profile
    }

    /// Resolves when a [`pump`](Self::pump) has something to do: events were pushed into an
    /// empty ingress, or the connector sent a control message. A connector that dropped its
    /// port without saying `Gone` is treated as gone. Cancel-safe.
    pub async fn ready(&mut self) {
        if self.pending_control.is_some() {
            return;
        }
        if self.control_closed {
            self.ingress.notified().await;
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

    /// One iteration: control messages, then the ingress.
    pub fn pump(&mut self, now: Instant) -> AppRoute {
        let mut route = AppRoute::default();
        self.pump_control(now, &mut route);

        if let Some(node) = self.pending_focus.take() {
            if self.send_focus(FocusLaneMsg::FocusChanged { node }) {
                route.forwarded_to_focus_lane += 1;
            } else {
                self.pending_focus = Some(node);
            }
        }

        let drained = self.ingress.drain();
        route.events = drained.events.len();
        if let Some(dirty) = drained.dirty {
            // Flooded: the ingress kept only the essentials and marked the rest dirty. We have
            // drained the backlog, so the app recovers with a lazy resync (R-3).
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
        // Only the newest "became selected" of a batch is worth speaking: the older ones are
        // superseded before the user could hear them (stale speech never starts).
        let last_selected = if self.profile.speak_selection {
            drained.events.iter().rposition(became_selected)
        } else {
            None
        };
        for (i, ev) in drained.events.into_iter().enumerate() {
            if became_selected(&ev) {
                if Some(i) == last_selected
                    && let NodeEvent::PropertiesChanged { node, .. } = ev
                {
                    if self.send_focus(FocusLaneMsg::ItemSelected { node }) {
                        route.forwarded_to_focus_lane += 1;
                    } else {
                        self.resync_needed = true;
                    }
                } else {
                    self.resync_needed = true;
                }
                continue;
            }
            self.route(ev, &mut route);
        }
        route
    }

    fn pump_control(&mut self, now: Instant, route: &mut AppRoute) {
        for _ in 0..MAX_CONTROL_PER_PUMP {
            let next = match self.pending_control.take() {
                Some(m) => Ok(m),
                None => self.control.try_recv(),
            };
            match next {
                Ok(AppControl::Snapshot(batch)) => self.writer.apply(batch),
                Ok(AppControl::Lifecycle(AppLifecycle::Gone)) => {
                    self.on_health(HealthMsg::OwnerVanished, now);
                    self.send_focus(FocusLaneMsg::AppGone(self.app));
                    route.gone = true;
                }
                Ok(AppControl::Lifecycle(AppLifecycle::Appeared(info))) => {
                    self.profile = AppProfile::for_app(&info);
                }
                Ok(AppControl::Lifecycle(AppLifecycle::Ready)) => {}
                Err(_) => break,
            }
        }
    }

    fn route(&mut self, ev: NodeEvent, route: &mut AppRoute) {
        match ev {
            NodeEvent::FocusChanged { node } => {
                if self.send_focus(FocusLaneMsg::FocusChanged { node }) {
                    route.forwarded_to_focus_lane += 1;
                } else {
                    // Never dropped: retry on the next pump.
                    self.pending_focus = Some(node);
                }
            }
            NodeEvent::NodeRemoved { node } => {
                self.writer.tombstone(node);
                if self.send_focus(FocusLaneMsg::NodeRemoved(node)) {
                    route.forwarded_to_focus_lane += 1;
                } else {
                    self.resync_needed = true;
                }
            }
            NodeEvent::Announcement {
                text, politeness, ..
            } => {
                self.forward_background(BackgroundMsg::Announcement { text, politeness }, route);
            }
            // Events that only dirty the mirror. TBD: refresh scheduling via the request mux;
            // for now they only mark the mirror as needing a resync.
            NodeEvent::ActiveDescendantChanged { .. }
            | NodeEvent::PropertiesChanged { .. }
            | NodeEvent::ChildrenChanged { .. }
            | NodeEvent::TextChanged { .. }
            | NodeEvent::CaretMoved { .. }
            | NodeEvent::SelectionChanged { .. }
            | NodeEvent::WindowEvent { .. }
            | NodeEvent::DocumentEvent { .. } => {
                self.resync_needed = true;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::reader_channels;
    use gorca_connector_api::app_channel;
    use gorca_ingress::IngressConfig;
    use gorca_mirror::{MirrorRead, mirror};
    use gorca_model::{
        ChildList, ConnectorId, Freshness, Ifaces, LocalId, Lookup, NodeSnapshot, Politeness,
        PropMask, Role, SnapshotBatch, StateSet,
    };
    use std::sync::Arc;

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: 1,
        }
    }

    fn key(n: u64) -> NodeKey {
        NodeKey {
            app: app(),
            local: LocalId(n),
        }
    }

    struct Rig {
        actor: AppActor,
        port: gorca_connector_api::AppPort,
        read: gorca_mirror::MirrorHandle,
        inbox: crate::reader::ReaderInbox,
    }

    fn rig(cfg: IngressConfig) -> Rig {
        let ch = app_channel(app(), cfg);
        let (writer, read) = mirror();
        let (tx, inbox) = reader_channels();
        let links = ReaderLinks {
            focus: tx.focus,
            background: tx.background,
        };
        Rig {
            actor: AppActor::new(app(), writer, ch.ingress, ch.control, links),
            port: ch.port,
            read,
            inbox,
        }
    }

    fn snap(n: u64) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: key(n),
            role: Role::Button,
            states: StateSet::empty(),
            name: None,
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    #[test]
    fn focus_goes_to_the_focus_lane_and_snapshots_fill_the_mirror() {
        let mut r = rig(IngressConfig::default());
        r.port
            .push_snapshot(SnapshotBatch {
                nodes: vec![snap(1)],
            })
            .unwrap();
        r.port.push(NodeEvent::FocusChanged { node: key(1) });
        r.port.push(NodeEvent::FocusChanged { node: key(2) });
        let route = r.actor.pump(Instant::now());
        assert_eq!(route.events, 1, "focus events are coalesced");
        assert_eq!(route.forwarded_to_focus_lane, 1);
        assert!(r.read.get(key(1)).hit().is_some());
        assert!(matches!(
            r.inbox.poll(),
            Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node })) if node == key(2)
        ));
    }

    #[test]
    fn removal_tombstones_the_mirror_and_tells_the_reader() {
        let mut r = rig(IngressConfig::default());
        r.port
            .push_snapshot(SnapshotBatch {
                nodes: vec![snap(1)],
            })
            .unwrap();
        r.actor.pump(Instant::now());
        r.port.push(NodeEvent::NodeRemoved { node: key(1) });
        r.actor.pump(Instant::now());
        assert_eq!(r.read.get(key(1)), Lookup::Gone);
        assert!(matches!(
            r.inbox.poll(),
            Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::NodeRemoved(
                _
            )))
        ));
    }

    #[test]
    fn property_noise_only_marks_the_mirror_for_resync() {
        let mut r = rig(IngressConfig::default());
        r.port.push(NodeEvent::PropertiesChanged {
            node: key(1),
            mask: PropMask::NAME,
        });
        let route = r.actor.pump(Instant::now());
        assert_eq!(
            route.forwarded_to_focus_lane + route.forwarded_to_background,
            0
        );
        assert!(r.actor.resync_needed());
        assert!(r.inbox.poll().is_none());
    }

    #[test]
    fn a_flood_ends_with_a_lazy_resync_and_a_healthy_app() {
        let mut r = rig(IngressConfig {
            max_keys: 8,
            ..IngressConfig::default()
        });
        for n in 0..100 {
            r.port.push(NodeEvent::ChildrenChanged { node: key(n) });
        }
        let route = r.actor.pump(Instant::now());
        let dirty = route.resync.expect("flooded ingress yields a resync set");
        assert!(dirty.contains(key(99)));
        assert_eq!(*r.actor.health(), AppHealth::Healthy);
    }

    #[test]
    fn a_full_background_lane_drops_and_requests_a_resync() {
        let mut r = rig(IngressConfig::default());
        for i in 0..70 {
            r.port.push(NodeEvent::Announcement {
                node: None,
                text: Arc::from(format!("a{i}")),
                politeness: Politeness::Polite,
            });
        }
        let route = r.actor.pump(Instant::now());
        assert_eq!(
            route.forwarded_to_background, 16,
            "ingress caps announcements per drain"
        );
        assert_eq!(route.dropped_background, 0);
        // Fill the lane to its bound, then overflow it.
        for round in 0..6 {
            for i in 0..16 {
                r.port.push(NodeEvent::Announcement {
                    node: None,
                    text: Arc::from(format!("r{round}-{i}")),
                    politeness: Politeness::Polite,
                });
            }
            r.actor.pump(Instant::now());
        }
        assert!(r.actor.resync_needed());
    }

    #[test]
    fn owner_gone_marks_the_app_gone_and_notifies_the_reader() {
        let mut r = rig(IngressConfig::default());
        r.port.lifecycle(AppLifecycle::Gone).unwrap();
        let route = r.actor.pump(Instant::now());
        assert!(route.gone);
        assert_eq!(*r.actor.health(), AppHealth::Gone);
        assert!(matches!(
            r.inbox.poll(),
            Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::AppGone(_)))
        ));
    }

    #[test]
    fn hung_transition_tells_the_reader_once() {
        let mut r = rig(IngressConfig::default());
        let now = Instant::now();
        let t = || HealthMsg::Timeout {
            class: gorca_rt::BudgetClass::Interactive,
        };
        r.actor.on_health(t(), now);
        r.actor.on_health(t(), now);
        r.actor.on_health(t(), now);
        assert!(matches!(r.actor.health(), AppHealth::Hung { .. }));
        assert!(matches!(
            r.inbox.poll(),
            Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::AppHung(_)))
        ));
        assert!(r.inbox.poll().is_none());
    }

    fn shell_info() -> gorca_model::AppInfo {
        gorca_model::AppInfo {
            toolkit: Arc::from("clutter"),
            app_name: Arc::from("gnome-shell"),
            pid: None,
            connector_kind: gorca_model::ConnectorKind::Atspi,
        }
    }

    fn selected(n: u64) -> NodeEvent {
        NodeEvent::PropertiesChanged {
            node: key(n),
            mask: PropMask::STATES | PropMask::SELECTED_NOW,
        }
    }

    #[test]
    fn the_shell_forwards_only_the_newest_selection_of_a_batch() {
        let mut r = rig(IngressConfig::default());
        r.port
            .lifecycle(AppLifecycle::Appeared(shell_info()))
            .unwrap();
        r.port.push(selected(1));
        r.port.push(selected(2));
        r.port.push(selected(3));
        let route = r.actor.pump(Instant::now());
        assert!(r.actor.profile().speak_selection);
        assert_eq!(route.forwarded_to_focus_lane, 1);
        assert!(matches!(
            r.inbox.poll(),
            Some(crate::reader::ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node })) if node == key(3)
        ));
        assert!(r.inbox.poll().is_none());
    }

    #[test]
    fn other_apps_never_speak_selection() {
        let mut r = rig(IngressConfig::default());
        r.port.push(selected(1));
        let route = r.actor.pump(Instant::now());
        assert_eq!(route.forwarded_to_focus_lane, 0);
        assert!(r.inbox.poll().is_none());
        assert!(r.actor.resync_needed());
    }
}
