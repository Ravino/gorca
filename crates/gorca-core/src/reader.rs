//! The Reader (ADR-0001 sections 2.5, 3.3, 6.2).
//!
//! > "One Reader owns user-visible state (focus, mode, navigation, epoch). It never awaits. It
//! > reads mirrors lock-free and sends queries as messages. Its per-message work is bounded and
//! > O(1) or O(log n). Anything heavier is offloaded."
//! > "The Reader polls with a biased select: input, focus lane, query replies, background lane."
//!
//! [`Reader::handle`] is synchronous and allocation-light: it is the body of the `reader` lane.
//! [`ReaderInbox::poll`] implements the biased poll with `try_recv`. TBD: the lane thread's
//! parking/wake mechanism (a shared parker signalled by the four senders), decided with B-5.

use std::sync::Arc;
use std::time::Instant;

use gorca_connector_api::BackendError;
use gorca_input_api::{GrabSet, KeyIntent};
use gorca_mirror::fill::{DemandOutcome, DemandWhy};
use gorca_model::{AppId, Epoch, NodeKey, NodeSnapshot, Politeness, PropMask, UttId};
use gorca_rt::{BudgetClass, EpochCell, FocusedApp, mpsc};
use smallvec::SmallVec;

use crate::SpeechIntent;
use crate::browse::BrowseState;
use crate::browse::commands::WebEffect;
use crate::commands::{CmdState, ConfigRequest, ControlMsg, SpeechCtl};
use crate::directory::MirrorDirectory;
use crate::doc_actor::DocNotice;
use crate::focus::{FocusMsg, FocusState};
use crate::mode::{Mode, ModeKeys, ModeMsg};
use crate::present::{PresentLog, PresenterRegistry};
use crate::review::{BuildRequest, NavReqId, ProbeItem, ReviewMsg, ReviewState};
use crate::review::{ModelId, NavOp};
use crate::text::TextMsg;

/// Bounds from section 6.2.
const KEY_QUEUE: usize = 1024;
const FOCUS_QUEUE: usize = 256;
const REPLY_QUEUE: usize = 64;
const BACKGROUND_QUEUE: usize = 64;
const CONTROL_QUEUE: usize = 32;
const REVIEW_QUEUE: usize = 64;
const DOC_QUEUE: usize = 64;
/// At most this many messages per [`Reader::drain`] call, so one call stays bounded.
const MAX_BATCH: usize = 256;
/// A second focus event for the node that just got focus, within this window, is the same
/// focus change reported twice (GTK3 and atk-bridge emit both `focus:` and
/// `state-changed:focused`). It must not restart presentation.
pub const DUPLICATE_FOCUS_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

#[derive(Clone, Copy, Debug)]
pub enum FocusLaneMsg {
    FocusChanged {
        node: NodeKey,
    },
    /// An item became selected in an application whose profile speaks selection moves (GNOME
    /// Shell's Alt+Tab switcher). Handled like a focus change, except that it never touches
    /// the focused-application protection: the item does not hold real focus.
    ItemSelected {
        node: NodeKey,
    },
    NodeRemoved(NodeKey),
    AppGone(AppId),
    AppHung(AppId),
    /// The state or value of the focused node changed (B-1 S1, PW-12): the Reader fetches it
    /// and speaks the change.
    StateChanged {
        node: NodeKey,
        mask: PropMask,
    },
}

/// Why the Reader asked for a node, so the reply is handled the right way when it comes back.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FetchKind {
    /// The cold path of a focus change: present the reply.
    Focus,
    /// RV-1: the node was presented from the mirror; compare and correct.
    Revalidate,
    /// S1: the focused node's state or value changed; speak the change.
    StateChange,
    /// A command's answer (`info.where_am_i`, `info.window_title`, debug): speak the reply.
    Info,
}

/// Reply to a Reader query, tagged with the epoch it was asked in (E-3).
#[derive(Clone, Debug)]
pub struct QueryReply {
    pub node: NodeKey,
    pub epoch: Epoch,
    pub kind: FetchKind,
    pub result: Result<Arc<NodeSnapshot>, BackendError>,
}

#[derive(Clone, Debug)]
pub enum BackgroundMsg {
    Announcement {
        text: Arc<str>,
        politeness: Politeness,
    },
    /// A background event was dropped for this app; its mirror may be stale.
    ResyncNeeded(AppId),
}

#[derive(Debug)]
pub enum ReaderMsg {
    Key(KeyIntent),
    Focus(FocusLaneMsg),
    Reply(QueryReply),
    Background(BackgroundMsg),
    Mode(ModeMsg),
    /// Settings snapshots, input notices and lane alerts (ADR-0005 6, 7.6).
    Control(ControlMsg),
    Review(ReviewMsg),
    Text(TextMsg),
    /// A web document published, changed or went away (browse mode).
    Doc(DocNotice),
    Timer,
}

#[derive(Clone, Debug)]
pub enum ReaderEffect {
    Speak(SpeechIntent),
    /// Query an app (Interactive), asked in `epoch`.
    Fetch {
        node: NodeKey,
        mask: PropMask,
        class: BudgetClass,
        epoch: Epoch,
        kind: FetchKind,
    },
    /// Tell the node's app actor how the mirror served (or failed) a read, so its fill policy
    /// can react. Never blocks the Reader; dropped when the actor is behind.
    Demand {
        node: NodeKey,
        why: DemandWhy,
        outcome: DemandOutcome,
    },
    /// The focus state machine reached `Presented` for `node`.
    FocusSettled {
        node: NodeKey,
    },
    QueryActiveWindowFocus,
    /// Wake the Reader at this time with `ReaderMsg::Timer`.
    ArmTimer(Instant),
    /// The grab set Mutter should apply, with the lookup context it goes with.
    SetGrabSet(GrabSet),
    /// Ordered with utterances: applied by the speech lane before the next one.
    SpeechCtl(SpeechCtl),
    /// A request for the daemon's config task.
    Config(ConfigRequest),
    /// Object navigation needs a node, a child list or ancestors in the mirror (ADR-0006 5.3).
    ReviewNav {
        req: NavReqId,
        op: NavOp,
    },
    /// Build or refresh the line model of a surface (ADR-0006 6.1).
    ReviewBuild(BuildRequest),
    /// RR-1: re-read the extents and text of the nodes of the line just spoken.
    ReviewRevalidate {
        model: ModelId,
        app: AppId,
        probe: Vec<ProbeItem>,
        clip: gorca_model::Rect,
    },
    /// Find the status bar below `root` with one `Collection` call (ADR-0006 8).
    ReviewStatusBar {
        req: NavReqId,
        root: NodeKey,
    },
    /// `review.activate` and `review.move_focus`.
    ReviewAct {
        req: NavReqId,
        act: gorca_connector_api::ActionReq,
    },
    /// Browse mode: find a document, wake or sleep it, act on one of its nodes.
    Web(WebEffect),
}

pub type ReaderEffects = SmallVec<[ReaderEffect; 4]>;

/// Producer side handed to the input lane, app actors and workers.
#[derive(Clone, Debug)]
pub struct ReaderSenders {
    pub key: mpsc::Sender<KeyIntent>,
    pub focus: mpsc::Sender<FocusLaneMsg>,
    pub replies: mpsc::Sender<QueryReply>,
    pub background: mpsc::Sender<BackgroundMsg>,
    pub control: mpsc::Sender<ControlMsg>,
    /// Results of review fetches, builds and actions (ADR-0006 4.3).
    pub review: mpsc::Sender<ReviewMsg>,
    /// Document notices (browse mode).
    pub docs: mpsc::Sender<DocNotice>,
}

#[derive(Debug)]
pub struct ReaderInbox {
    key: mpsc::Receiver<KeyIntent>,
    focus: mpsc::Receiver<FocusLaneMsg>,
    replies: mpsc::Receiver<QueryReply>,
    background: mpsc::Receiver<BackgroundMsg>,
    control: mpsc::Receiver<ControlMsg>,
    review: mpsc::Receiver<ReviewMsg>,
    docs: mpsc::Receiver<DocNotice>,
}

pub fn reader_channels() -> (ReaderSenders, ReaderInbox) {
    let (key_tx, key) = mpsc::channel(KEY_QUEUE);
    let (focus_tx, focus) = mpsc::channel(FOCUS_QUEUE);
    let (replies_tx, replies) = mpsc::channel(REPLY_QUEUE);
    let (background_tx, background) = mpsc::channel(BACKGROUND_QUEUE);
    let (control_tx, control) = mpsc::channel(CONTROL_QUEUE);
    let (review_tx, review) = mpsc::channel(REVIEW_QUEUE);
    let (docs_tx, docs) = mpsc::channel(DOC_QUEUE);
    (
        ReaderSenders {
            key: key_tx,
            focus: focus_tx,
            replies: replies_tx,
            background: background_tx,
            control: control_tx,
            review: review_tx,
            docs: docs_tx,
        },
        ReaderInbox {
            key,
            focus,
            replies,
            background,
            control,
            review,
            docs,
        },
    )
}

impl ReaderInbox {
    /// Wait for the next message with the same bias as [`ReaderInbox::poll`]: input, focus
    /// lane, query replies, background lane. `None` once every sender is gone. Cancel-safe.
    pub async fn next(&mut self) -> Option<ReaderMsg> {
        tokio::select! {
            biased;
            Some(k) = self.key.recv() => Some(ReaderMsg::Key(k)),
            Some(c) = self.control.recv() => Some(ReaderMsg::Control(c)),
            Some(f) = self.focus.recv() => Some(ReaderMsg::Focus(f)),
            Some(r) = self.replies.recv() => Some(ReaderMsg::Reply(r)),
            Some(v) = self.review.recv() => Some(ReaderMsg::Review(v)),
            Some(n) = self.docs.recv() => Some(ReaderMsg::Doc(n)),
            Some(b) = self.background.recv() => Some(ReaderMsg::Background(b)),
            else => None,
        }
    }

    /// Biased poll: input, focus lane, query replies, background lane. Never waits.
    pub fn poll(&mut self) -> Option<ReaderMsg> {
        if let Ok(k) = self.key.try_recv() {
            return Some(ReaderMsg::Key(k));
        }
        if let Ok(c) = self.control.try_recv() {
            return Some(ReaderMsg::Control(c));
        }
        if let Ok(f) = self.focus.try_recv() {
            return Some(ReaderMsg::Focus(f));
        }
        if let Ok(r) = self.replies.try_recv() {
            return Some(ReaderMsg::Reply(r));
        }
        if let Ok(v) = self.review.try_recv() {
            return Some(ReaderMsg::Review(v));
        }
        if let Ok(n) = self.docs.try_recv() {
            return Some(ReaderMsg::Doc(n));
        }
        if let Ok(b) = self.background.try_recv() {
            return Some(ReaderMsg::Background(b));
        }
        None
    }
}

#[derive(Debug)]
pub struct Reader {
    pub(crate) focus: FocusState,
    pub(crate) mode: Mode,
    pub(crate) keys: ModeKeys,
    pub(crate) epoch: Arc<EpochCell>,
    pub(crate) dir: Arc<MirrorDirectory>,
    pub(crate) next_utt: u64,
    pub(crate) ambiguous_keys: u64,
    pub(crate) stale_replies_dropped: u64,
    pub(crate) duplicate_focus_dropped: u64,
    pub(crate) last_focus: Option<(NodeKey, Instant)>,
    /// The node of the latest `ItemSelected`, until a real focus change replaces it.
    pub(crate) selection_node: Option<NodeKey>,
    pub(crate) focused_app: Option<Arc<FocusedApp>>,
    pub(crate) presenters: PresenterRegistry,
    /// Settings, speech toggles and the pending command answer (ADR-0005).
    pub(crate) cmd: CmdState,
    /// The mirror write sequence of the app when the current focus began (RV-1: a presentation
    /// whose content was confirmed at or after it needs no revalidation).
    pub(crate) focus_seq: u64,
    /// The latest node presentation, for revalidation, refines and state-change speech.
    pub(crate) presented: Option<crate::present::Presented>,
    /// Time of the message being handled.
    pub(crate) now: Instant,
    pub(crate) present_log: Option<Arc<PresentLog>>,
    pub(crate) counters: ReaderCounters,
    /// Review cursor state, models and the pending review operation (ADR-0006).
    pub(crate) review: ReviewState,
    /// Browse mode (ADR-0003 section 11): off until the runtime attaches the document side.
    pub(crate) web: Option<BrowseState>,
}

/// Revalidation and correction counters (B-1 M-6).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ReaderCounters {
    /// Presentations served from the mirror.
    pub cached_presentations: u64,
    /// RV-1 fetches issued.
    pub revalidations: u64,
    /// RV-1 replies that agreed with what was presented.
    pub revalidations_agreed: u64,
    /// Corrections that replaced the earlier utterance.
    pub refined: u64,
    /// Corrections spoken as a follow-up.
    pub followups: u64,
    /// State-change (S1) utterances.
    pub state_changes_spoken: u64,
}

impl Reader {
    pub fn new(epoch: Arc<EpochCell>, dir: Arc<MirrorDirectory>, keys: ModeKeys) -> Self {
        Self {
            focus: FocusState::NoFocus,
            mode: Mode::Focus,
            keys,
            epoch,
            dir,
            next_utt: 0,
            ambiguous_keys: 0,
            stale_replies_dropped: 0,
            duplicate_focus_dropped: 0,
            last_focus: None,
            selection_node: None,
            focused_app: None,
            presenters: PresenterRegistry::builtin(),
            cmd: CmdState::new(),
            focus_seq: 0,
            presented: None,
            now: Instant::now(),
            present_log: None,
            counters: ReaderCounters::default(),
            review: ReviewState::new(),
            web: None,
        }
    }

    /// Record every node presentation in `log` (PW-10).
    #[must_use]
    pub fn with_present_log(mut self, log: Arc<PresentLog>) -> Self {
        self.present_log = Some(log);
        self
    }

    pub fn counters(&self) -> ReaderCounters {
        self.counters
    }

    /// Publish the focused application through `focused` (lock-free, read by the connector).
    #[must_use]
    pub fn with_focused(mut self, focused: Arc<FocusedApp>) -> Self {
        self.focused_app = Some(focused);
        self
    }

    /// Share the input lane's cells: the Reader acknowledges keys through them (LM-3).
    #[must_use]
    pub fn with_links(mut self, links: Arc<gorca_input_api::LaneLinks>) -> Self {
        self.cmd.links = Some(links);
        self
    }

    /// Publish the effective settings (file plus runtime changes) through `cell`.
    #[must_use]
    pub fn with_settings_cell(
        mut self,
        cell: Arc<arc_swap::ArcSwap<gorca_config::Settings>>,
    ) -> Self {
        self.cmd.published = Some(cell);
        self.cmd.republish();
        self
    }

    /// Replace the wall clock (`info.time_date`); tests use a fixed one.
    #[must_use]
    pub fn with_wall_clock(mut self, clock: Arc<dyn crate::commands::WallClock>) -> Self {
        self.cmd.clock = clock;
        self
    }

    /// The effective settings: the file plus runtime changes.
    pub fn settings(&self) -> Arc<gorca_config::Settings> {
        Arc::clone(&self.cmd.effective)
    }

    /// Focus events ignored as duplicates of the one just handled.
    pub fn duplicate_focus_dropped(&self) -> u64 {
        self.duplicate_focus_dropped
    }

    pub fn focus(&self) -> FocusState {
        self.focus
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    /// Ambiguous command chords that were not run and answered with "again".
    pub fn ambiguous_cues(&self) -> u64 {
        self.cmd.ambiguous_cues
    }

    /// Keys classified `Ambiguous` (M-16).
    pub fn ambiguous_keys(&self) -> u64 {
        self.ambiguous_keys
    }

    /// Replies discarded because their epoch was stale (E-3).
    pub fn stale_replies_dropped(&self) -> u64 {
        self.stale_replies_dropped
    }

    /// `speech.repeat_last` repeats what the user heard last: focus and state speech, not the
    /// echo of keys or the answers to commands (ADR-0005 6.4).
    fn remember_spoken(&mut self, from_key_or_control: bool, fx: &ReaderEffects) {
        if from_key_or_control {
            return;
        }
        if let Some(ReaderEffect::Speak(s)) = fx
            .iter()
            .rev()
            .find(|e| matches!(e, ReaderEffect::Speak(_)))
        {
            self.cmd.last_spoken = Some(s.utterance.clone());
        }
    }

    pub(crate) fn utt_id(&mut self) -> UttId {
        self.next_utt += 1;
        UttId(self.next_utt)
    }

    /// Process every pending message, biased, at most [`MAX_BATCH`] per call.
    pub fn drain(&mut self, inbox: &mut ReaderInbox, now: Instant) -> Vec<ReaderEffect> {
        let mut out = Vec::new();
        for _ in 0..MAX_BATCH {
            let Some(msg) = inbox.poll() else { break };
            out.extend(self.handle(msg, now));
        }
        out
    }

    /// One message, bounded work, no awaits.
    pub fn handle(&mut self, msg: ReaderMsg, now: Instant) -> ReaderEffects {
        let mut fx = ReaderEffects::new();
        let is_key = matches!(msg, ReaderMsg::Key(_) | ReaderMsg::Control(_));
        self.now = now;
        match msg {
            ReaderMsg::Key(intent) => self.on_key(intent, now, &mut fx),
            ReaderMsg::Control(c) => self.on_control(c, now, &mut fx),
            ReaderMsg::Focus(f) => self.on_focus(f, now, &mut fx),
            ReaderMsg::Reply(r) => self.on_reply(r, &mut fx),
            ReaderMsg::Background(b) => match b {
                BackgroundMsg::Announcement { .. } | BackgroundMsg::ResyncNeeded(_) => {
                    // TBD: live-region policy lives in gorca-doc; resync scheduling in the app actor.
                }
            },
            ReaderMsg::Mode(m) => self.on_mode(m, &mut fx),
            ReaderMsg::Review(m) => self.on_review(m, &mut fx),
            ReaderMsg::Text(m) => self.on_text(m, &mut fx),
            ReaderMsg::Doc(n) => self.on_doc(n, &mut fx),
            ReaderMsg::Timer => {
                self.apply_focus(FocusMsg::TimerElapsed { now }, &mut fx);
                self.pass_timer(now, &mut fx);
                self.review_timer(now, &mut fx);
                self.web_timer(now, &mut fx);
            }
        }
        self.remember_spoken(is_key, &fx);
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::{CmdId, KeyClass, KeyEvent, Modifiers, PressKind};
    use gorca_mirror::{MirrorWriter, mirror};
    use gorca_model::{
        ChildList, ConnectorId, Freshness, Ifaces, LocalId, Role, SnapshotBatch, StateSet,
    };
    use std::time::Duration;

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

    fn snap(n: u64, role: Role, name: &str) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: key(n),
            role,
            states: StateSet::empty(),
            name: Some(Arc::from(name)),
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: gorca_model::PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    struct Fixture {
        reader: Reader,
        writer: MirrorWriter,
        epoch: Arc<EpochCell>,
    }

    fn fixture() -> Fixture {
        let epoch = Arc::new(EpochCell::new());
        let dir = Arc::new(MirrorDirectory::new());
        let (writer, handle) = mirror();
        dir.insert(app(), &handle);
        let reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default());
        Fixture {
            reader,
            writer,
            epoch,
        }
    }

    fn spoken_fx(fx: &[ReaderEffect]) -> Vec<&ReaderEffect> {
        fx.iter()
            .filter(|e| matches!(e, ReaderEffect::Speak(_)))
            .collect()
    }

    fn spoken(fx: &ReaderEffects) -> Vec<String> {
        fx.iter()
            .filter_map(|e| match e {
                ReaderEffect::Speak(s) => Some(
                    s.utterance
                        .segments
                        .iter()
                        .map(|g| &*g.text)
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                ReaderEffect::Fetch { .. }
                | ReaderEffect::Demand { .. }
                | ReaderEffect::FocusSettled { .. }
                | ReaderEffect::QueryActiveWindowFocus
                | ReaderEffect::ArmTimer(_)
                | ReaderEffect::SetGrabSet(_)
                | ReaderEffect::SpeechCtl(_)
                | ReaderEffect::Config(_)
                | ReaderEffect::ReviewNav { .. }
                | ReaderEffect::ReviewBuild(_)
                | ReaderEffect::ReviewRevalidate { .. }
                | ReaderEffect::ReviewStatusBar { .. }
                | ReaderEffect::ReviewAct { .. }
                | ReaderEffect::Web(_) => None,
            })
            .collect()
    }

    #[test]
    fn cache_hit_focus_speaks_immediately_with_the_new_epoch() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(1, Role::Button, "OK")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let before = f.epoch.current();
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(1) }),
            Instant::now(),
        );
        assert_eq!(spoken(&fx), ["OK button"]);
        assert!(
            f.epoch.current() > before,
            "focus change advances the epoch"
        );
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("expected speech")
        };
        assert_eq!(s.utterance.epoch, f.epoch.current());
        assert!(matches!(
            f.reader.focus(),
            FocusState::Focused {
                presented: true,
                ..
            }
        ));
    }

    #[test]
    fn cache_miss_fetches_then_speaks_on_the_reply() {
        let mut f = fixture();
        let now = Instant::now();
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(2) }),
            now,
        );
        let epoch = f.epoch.current();
        assert!(matches!(
            fx.as_slice(),
            [
                ReaderEffect::Fetch {
                    class: BudgetClass::Interactive,
                    kind: FetchKind::Focus,
                    ..
                },
                ReaderEffect::Demand {
                    outcome: DemandOutcome::Miss,
                    ..
                }
            ]
        ));
        let reply = QueryReply {
            node: key(2),
            epoch,
            kind: FetchKind::Focus,
            result: Ok(snap(2, Role::Link, "Docs")),
        };
        let fx = f.reader.handle(ReaderMsg::Reply(reply), now);
        assert_eq!(spoken(&fx), ["Docs link"]);
    }

    #[test]
    fn a_reply_from_an_older_epoch_is_never_spoken() {
        let mut f = fixture();
        let now = Instant::now();
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(2) }),
            now,
        );
        let stale = f.epoch.current();
        // The user moves on before the reply arrives.
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(3) }),
            now,
        );
        let reply = QueryReply {
            node: key(2),
            epoch: stale,
            kind: FetchKind::Focus,
            result: Ok(snap(2, Role::Link, "Old")),
        };
        let fx = f.reader.handle(ReaderMsg::Reply(reply), now);
        assert!(spoken(&fx).is_empty());
        assert_eq!(f.reader.stale_replies_dropped(), 1);
    }

    #[test]
    fn a_failed_fetch_falls_back_to_a_stale_cache_entry() {
        let mut f = fixture();
        let now = Instant::now();
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(2) }),
            now,
        );
        let epoch = f.epoch.current();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(2, Role::Link, "Docs")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let reply = QueryReply {
            node: key(2),
            epoch,
            kind: FetchKind::Focus,
            result: Err(BackendError::Timeout),
        };
        let fx = f.reader.handle(ReaderMsg::Reply(reply), now);
        assert_eq!(spoken(&fx), ["Docs link"]);
    }

    #[test]
    fn flapping_roles_wait_for_the_settle_timer() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(1, Role::MenuItem, "Open")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(1) }),
            now,
        );
        assert!(spoken(&fx).is_empty());
        let ReaderEffect::ArmTimer(at) = fx[0] else {
            panic!("expected a timer")
        };
        assert_eq!(at, now + Duration::from_millis(30));
        let fx = f.reader.handle(ReaderMsg::Timer, at);
        assert_eq!(spoken(&fx), ["Open menu item"]);
    }

    #[test]
    fn key_presses_adopt_their_epoch_and_dispatch_commands() {
        let mut f = fixture();
        let ev = KeyEvent {
            keycode: 1,
            keysym: 1,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed: true,
        };
        let intent = KeyIntent {
            event: ev,
            class: KeyClass::Command(crate::commands::CoreCmd::SpeechRepeatLast.id()),
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: Instant::now(),
            epoch: Epoch(41),
        };
        let fx = f.reader.handle(ReaderMsg::Key(intent), Instant::now());
        assert_eq!(spoken(&fx), ["nothing to repeat"]);
        assert_eq!(f.epoch.current(), Epoch(41));
        let _ = CmdId(0);

        let amb = KeyIntent {
            class: KeyClass::Ambiguous { committed: None },
            ..intent
        };
        f.reader.handle(ReaderMsg::Key(amb), Instant::now());
        assert_eq!(f.reader.ambiguous_keys(), 1);
    }

    #[test]
    fn a_plain_key_press_is_echoed_with_the_epoch_of_the_press() {
        let mut f = fixture();
        let ev = |pressed| KeyEvent {
            keycode: 38,
            keysym: 0x61,
            unichar: 0x61,
            modifiers: Modifiers::empty(),
            pressed,
        };
        let intent = |pressed| KeyIntent {
            event: ev(pressed),
            class: KeyClass::PassToApp,
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: Instant::now(),
            epoch: Epoch(7),
        };
        let fx = f
            .reader
            .handle(ReaderMsg::Key(intent(true)), Instant::now());
        assert_eq!(spoken(&fx), ["a"]);
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("expected speech")
        };
        assert_eq!(s.utterance.epoch, Epoch(7));
        assert_eq!(
            f.epoch.current(),
            Epoch(7),
            "the press epoch is adopted (E-4)"
        );
        let fx = f
            .reader
            .handle(ReaderMsg::Key(intent(false)), Instant::now());
        assert!(fx.is_empty(), "releases are silent");
    }

    fn selected_snap(n: u64, name: &str) -> Arc<NodeSnapshot> {
        let mut s = (*snap(n, Role::Button, name)).clone();
        s.states = StateSet::SELECTED;
        Arc::new(s)
    }

    fn press(keysym: u32, modifiers: Modifiers, epoch: u64) -> ReaderMsg {
        ReaderMsg::Key(KeyIntent {
            event: KeyEvent {
                keycode: 23,
                keysym,
                unichar: 0,
                modifiers,
                pressed: true,
            },
            class: KeyClass::PassToApp,
            kind: PressKind::Press,
            repeat: 1,
            chord: None,
            at: Instant::now(),
            epoch: Epoch(epoch),
        })
    }

    /// The Alt+Tab switcher: every move to a new item is spoken, however many there are, and
    /// each move cancels the speech before it. The Tab presses themselves are silent.
    #[test]
    fn every_switcher_selection_is_spoken_and_alt_tab_is_not_echoed() {
        let mut f = fixture();
        let names = ["Terminal", "Files", "Firefox", "Settings", "Terminal"];
        let ids = [1u64, 2, 3, 4, 1];
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![
                    selected_snap(1, "Terminal"),
                    selected_snap(2, "Files"),
                    selected_snap(3, "Firefox"),
                    selected_snap(4, "Settings"),
                ],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let mut now = Instant::now();
        let mut last_epoch = f.epoch.current();
        for (i, (name, id)) in names.iter().zip(ids).enumerate() {
            let tab = f.reader.handle(press(0xff09, Modifiers::MOD1, 0), now);
            assert!(tab.is_empty(), "Alt+Tab press {i} is not echoed");
            now += std::time::Duration::from_millis(40);
            let fx = f.reader.handle(
                ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node: key(id) }),
                now,
            );
            assert_eq!(spoken(&fx), [format!("{name} button")], "selection {i}");
            let ReaderEffect::Speak(s) = &fx[0] else {
                panic!("expected speech")
            };
            assert!(
                s.utterance.epoch > last_epoch,
                "each move supersedes the last"
            );
            last_epoch = s.utterance.epoch;
        }
    }

    /// A key echo is never newer than the semantic speech it precedes: the focus or selection
    /// that follows advances the epoch past the echo's, so the speech lane drops the echo.
    #[test]
    fn selection_speech_outranks_the_echo_of_the_key_that_caused_it() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![selected_snap(1, "Files")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        let echo = f.reader.handle(press(0xff09, Modifiers::empty(), 5), now);
        let ReaderEffect::Speak(e) = &echo[0] else {
            panic!("a bare Tab is echoed")
        };
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node: key(1) }),
            now,
        );
        let ReaderEffect::Speak(s) = &fx[0] else {
            panic!("expected speech")
        };
        assert!(s.utterance.epoch > e.utterance.epoch);
        assert!(
            !e.utterance.is_valid(f.epoch.current()),
            "the echo is stale once the selection is spoken"
        );
        assert!(s.utterance.is_valid(f.epoch.current()));
    }

    /// A selected item is not real focus: it must not move the eviction protection.
    #[test]
    fn item_selection_leaves_focus_protection_alone() {
        let mut f = fixture();
        let published = Arc::new(FocusedApp::new());
        f.reader.focused_app = Some(Arc::clone(&published));
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![selected_snap(1, "Files")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let before = published.generation();
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node: key(1) }),
            Instant::now(),
        );
        assert_eq!(published.generation(), before);
    }

    /// Eviction immunity is earned by an accepted, presented, validated focus only.
    #[test]
    fn only_a_validated_focus_is_published_for_protection() {
        let mut f = fixture();
        let published = Arc::new(FocusedApp::new());
        f.reader.focused_app = Some(Arc::clone(&published));
        let now = Instant::now();

        // A focus event for a node the app has not shown to be FOCUSED (a hostile claim, or a
        // stale one): spoken as usual, but it earns no protection.
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(1, Role::Button, "Claim")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(1) }),
            now,
        );
        assert_eq!(published.get(), None, "no FOCUSED state, no protection");

        // A cache miss: nothing is presented yet, so nothing is published.
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(2) }),
            now + std::time::Duration::from_secs(1),
        );
        assert_eq!(published.get(), None, "fetch pending");

        // The reply shows the node FOCUSED: accepted, presented and published.
        let mut focused = (*snap(2, Role::Button, "Real")).clone();
        focused.states = StateSet::FOCUSED;
        let reply = QueryReply {
            node: key(2),
            epoch: f.epoch.current(),
            kind: FetchKind::Focus,
            result: Ok(Arc::new(focused)),
        };
        f.reader.handle(ReaderMsg::Reply(reply), now);
        assert_eq!(published.get(), Some(app()));

        // The focus moves to a node that shows no FOCUSED state: protection is not kept by the
        // application that held it; and an application that is gone loses it outright.
        let later = now + std::time::Duration::from_secs(2);
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(3, Role::Button, "Unvalidated")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(3) }),
            later,
        );
        assert_eq!(published.get(), None, "current protection dropped");
        f.reader
            .handle(ReaderMsg::Focus(FocusLaneMsg::AppGone(app())), later);
        assert!(!published.protects(app()), "a departed app keeps nothing");
    }

    #[test]
    fn hung_focus_app_is_announced() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(1, Role::Button, "OK")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(1) }),
            now,
        );
        let fx = f
            .reader
            .handle(ReaderMsg::Focus(FocusLaneMsg::AppHung(app())), now);
        assert_eq!(spoken(&fx), ["application not responding"]);
    }

    #[test]
    fn mode_change_advances_the_epoch_and_sets_the_grab_set() {
        let mut f = fixture();
        let before = f.epoch.current();
        let fx = f
            .reader
            .handle(ReaderMsg::Mode(ModeMsg::ToggleInputHelp), Instant::now());
        assert!(f.epoch.current() > before);
        assert!(matches!(fx.as_slice(), [ReaderEffect::SetGrabSet(g)] if g.grab_all));
        assert_eq!(
            f.reader.mode(),
            Mode::InputHelp {
                resume: crate::mode::BaseMode::Focus
            }
        );
    }

    #[test]
    fn inbox_is_biased_input_then_focus_then_replies_then_background() {
        let (tx, mut inbox) = reader_channels();
        tx.background
            .try_send(BackgroundMsg::ResyncNeeded(app()))
            .unwrap();
        tx.replies
            .try_send(QueryReply {
                node: key(1),
                epoch: Epoch(1),
                kind: FetchKind::Focus,
                result: Err(BackendError::Gone),
            })
            .unwrap();
        tx.focus
            .try_send(FocusLaneMsg::FocusChanged { node: key(1) })
            .unwrap();
        let ev = KeyEvent {
            keycode: 1,
            keysym: 1,
            unichar: 0,
            modifiers: Modifiers::empty(),
            pressed: true,
        };
        tx.key
            .try_send(KeyIntent {
                event: ev,
                class: KeyClass::PassToApp,
                kind: PressKind::Press,
                repeat: 1,
                chord: None,
                at: Instant::now(),
                epoch: Epoch(1),
            })
            .unwrap();
        let order: Vec<_> = std::iter::from_fn(|| inbox.poll())
            .map(|m| match m {
                ReaderMsg::Key(_) => "key",
                ReaderMsg::Focus(_) => "focus",
                ReaderMsg::Reply(_) => "reply",
                ReaderMsg::Background(_) => "background",
                ReaderMsg::Mode(_) => "mode",
                ReaderMsg::Control(_) => "control",
                ReaderMsg::Review(_) => "review",
                ReaderMsg::Text(_) => "text",
                ReaderMsg::Doc(_) => "doc",
                ReaderMsg::Timer => "timer",
            })
            .collect();
        assert_eq!(order, ["key", "focus", "reply", "background"]);
    }

    #[test]
    fn drain_processes_a_burst_in_one_call() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch {
                nodes: vec![snap(1, Role::Button, "OK")],
                ..Default::default()
            },
            gorca_mirror::ApplyOrigin::Push,
        );
        let (tx, mut inbox) = reader_channels();
        tx.focus
            .try_send(FocusLaneMsg::FocusChanged { node: key(1) })
            .unwrap();
        let fx = f.reader.drain(&mut inbox, Instant::now());
        assert_eq!(spoken_fx(&fx).len(), 1);
    }

    // ---- B-1: mirror reads, RV-1, S1 and the removed-focus rule ----

    fn effects_of(fx: &ReaderEffects) -> Vec<&'static str> {
        fx.iter()
            .map(|e| match e {
                ReaderEffect::Speak(_) => "speak",
                ReaderEffect::Fetch {
                    kind: FetchKind::Focus,
                    ..
                } => "fetch-focus",
                ReaderEffect::Fetch {
                    kind: FetchKind::Revalidate,
                    ..
                } => "fetch-rv1",
                ReaderEffect::Fetch {
                    kind: FetchKind::StateChange,
                    ..
                } => "fetch-s1",
                ReaderEffect::Fetch {
                    kind: FetchKind::Info,
                    ..
                } => "fetch-info",
                ReaderEffect::Demand { .. } => "demand",
                ReaderEffect::FocusSettled { .. } => "settled",
                ReaderEffect::QueryActiveWindowFocus => "query",
                ReaderEffect::ArmTimer(_) => "timer",
                ReaderEffect::SetGrabSet(_) => "grab",
                ReaderEffect::SpeechCtl(_) => "ctl",
                ReaderEffect::Config(_) => "config",
                ReaderEffect::ReviewNav { .. } => "review-nav",
                ReaderEffect::ReviewBuild(_) => "review-build",
                ReaderEffect::ReviewRevalidate { .. } => "review-revalidate",
                ReaderEffect::ReviewStatusBar { .. } => "review-status-bar",
                ReaderEffect::ReviewAct { .. } => "review-act",
                ReaderEffect::Web(_) => "web",
            })
            .collect()
    }

    fn utterance(fx: &ReaderEffects) -> gorca_model::Utterance {
        fx.iter()
            .find_map(|e| match e {
                ReaderEffect::Speak(s) => Some(s.utterance.clone()),
                ReaderEffect::Fetch { .. }
                | ReaderEffect::Demand { .. }
                | ReaderEffect::FocusSettled { .. }
                | ReaderEffect::QueryActiveWindowFocus
                | ReaderEffect::ArmTimer(_)
                | ReaderEffect::SetGrabSet(_)
                | ReaderEffect::SpeechCtl(_)
                | ReaderEffect::Config(_)
                | ReaderEffect::ReviewNav { .. }
                | ReaderEffect::ReviewBuild(_)
                | ReaderEffect::ReviewRevalidate { .. }
                | ReaderEffect::ReviewStatusBar { .. }
                | ReaderEffect::ReviewAct { .. }
                | ReaderEffect::Web(_) => None,
            })
            .expect("an utterance")
    }

    fn focus_on(f: &mut Fixture, n: u64, now: Instant) -> ReaderEffects {
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(n) }),
            now,
        )
    }

    fn reply(f: &Fixture, n: u64, kind: FetchKind, snap: Arc<NodeSnapshot>) -> ReaderMsg {
        ReaderMsg::Reply(QueryReply {
            node: key(n),
            epoch: f.epoch.current(),
            kind,
            result: Ok(snap),
        })
    }

    fn with_name(n: u64, name: &str, states: StateSet) -> Arc<NodeSnapshot> {
        let mut s = (*snap(n, Role::CheckBox, name)).clone();
        s.states = states;
        Arc::new(s)
    }

    /// A node confirmed before the focus began is served from memory, spoken as `may_refine`,
    /// and revalidated (5.3); the speech comes first.
    #[test]
    fn a_mirror_hit_is_spoken_first_tagged_may_refine_and_revalidated() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Wi-Fi", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let fx = focus_on(&mut f, 1, Instant::now());
        assert_eq!(
            effects_of(&fx),
            ["speak", "settled", "fetch-rv1", "demand"],
            "speech first, bookkeeping after"
        );
        assert!(utterance(&fx).may_refine, "every cache-served utterance");
        assert!(matches!(
            fx.last(),
            Some(ReaderEffect::Demand {
                outcome: DemandOutcome::Hit,
                ..
            })
        ));
        assert_eq!(f.reader.counters().revalidations, 1);
    }

    #[test]
    fn a_revalidation_that_agrees_is_silent() {
        let mut f = fixture();
        let s = with_name(1, "Wi-Fi", StateSet::CHECKED);
        f.writer.apply(
            SnapshotBatch::new(vec![Arc::clone(&s)]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        let fx = f.reader.handle(reply(&f, 1, FetchKind::Revalidate, s), now);
        assert!(fx.is_empty());
        assert_eq!(f.reader.counters().revalidations_agreed, 1);
        assert_eq!(f.reader.counters().refined, 0);
    }

    /// A silent rename (G-1) is found by RV-1 and replaces the utterance while it is young.
    #[test]
    fn a_young_stale_presentation_is_refined_by_replacement() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Old name", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        let first = utterance(&focus_on(&mut f, 1, now));
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::Revalidate,
                with_name(1, "New name", StateSet::empty()),
            ),
            now + Duration::from_millis(40),
        );
        let u = utterance(&fx);
        assert_eq!(u.refines, Some(first.id));
        assert_eq!(
            u.prio,
            gorca_model::Prio::Interrupt,
            "it replaces the old one"
        );
        assert!(!u.may_refine);
        assert_eq!(u.epoch, first.epoch);
        let words: Vec<_> = u.segments.iter().map(|s| s.text.to_string()).collect();
        assert_eq!(words, ["New name", "check box"]);
        assert_eq!(f.reader.counters().refined, 1);
    }

    /// Too late to replace: a short follow-up with the changed field only.
    #[test]
    fn an_old_stale_presentation_gets_a_followup() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Wi-Fi", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        let first = utterance(&focus_on(&mut f, 1, now));
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::Revalidate,
                with_name(1, "Wi-Fi", StateSet::CHECKED),
            ),
            now + Duration::from_millis(400),
        );
        let u = utterance(&fx);
        assert_eq!(u.refines, Some(first.id));
        assert_eq!(u.prio, gorca_model::Prio::Message);
        let words: Vec<_> = u.segments.iter().map(|s| s.text.to_string()).collect();
        assert_eq!(words, ["checked"]);
        assert_eq!(f.reader.counters().followups, 1);
    }

    /// A request issued after the focus began confirmed the content: nothing left to revalidate.
    /// (The role flaps, so the focus settles for 30 ms; a prefetch lands in between.)
    #[test]
    fn content_confirmed_by_a_request_issued_after_the_focus_began_needs_no_revalidation() {
        let mut f = fixture();
        let mut menu = (*with_name(1, "Open", StateSet::empty())).clone();
        menu.role = Role::MenuItem;
        let now = Instant::now();
        f.writer.apply(
            SnapshotBatch::new(vec![Arc::new(menu.clone())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let fx = focus_on(&mut f, 1, now);
        assert!(spoken(&fx).is_empty(), "settling");
        // A prefetch issued after the focus event lands before the settle timer.
        let issued_at = f.writer.next_seq();
        f.writer.apply(
            SnapshotBatch::new(vec![Arc::new(menu)]),
            gorca_mirror::ApplyOrigin::Reply { issued_at },
        );
        let fx = f
            .reader
            .handle(ReaderMsg::Timer, now + Duration::from_millis(31));
        assert_eq!(effects_of(&fx), ["speak", "settled", "demand"]);
    }

    /// Reviewer probe F1: a quiet app (one old event, then nothing) whose node was fetched on an
    /// earlier visit and renamed silently (GTK4 G-1). The revisit is revalidated.
    #[test]
    fn a_revisit_in_a_quiet_app_is_revalidated() {
        let mut f = fixture();
        let t0 = Instant::now();
        f.writer.invalidate(key(9), PropMask::NAME, t0);
        let issued_at = f.writer.next_seq();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Old", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Reply { issued_at },
        );
        let _ = focus_on(&mut f, 1, t0);
        let _ = focus_on(&mut f, 2, t0 + Duration::from_secs(2));
        let fx = focus_on(&mut f, 1, t0 + Duration::from_secs(10));
        assert!(
            effects_of(&fx).contains(&"fetch-rv1"),
            "revisit served from the mirror without RV-1: {:?}",
            effects_of(&fx)
        );
        // And the silent rename is found and corrected.
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::Revalidate,
                with_name(1, "New", StateSet::empty()),
            ),
            t0 + Duration::from_secs(10),
        );
        assert_eq!(f.reader.counters().refined, 1, "{:?}", effects_of(&fx));
    }

    /// F4: a storm of focus events on cached nodes is coalesced: last focus wins, one utterance.
    #[test]
    fn a_focus_storm_on_cached_nodes_speaks_once() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(
                (1..=4)
                    .map(|n| with_name(n, "n", StateSet::empty()))
                    .collect(),
            ),
            gorca_mirror::ApplyOrigin::Push,
        );
        let t0 = Instant::now();
        // 300 focus events, one millisecond apart, across four cached nodes.
        let mut all = Vec::new();
        let mut timer = None;
        for i in 0..300u64 {
            let now = t0 + Duration::from_millis(i);
            all.extend(focus_on(&mut f, 1 + i % 4, now));
            // The runtime would fire the timer only when it is due.
            if let Some(at) = timer
                && at <= now
            {
                timer = None;
                all.extend(f.reader.handle(ReaderMsg::Timer, now));
            }
            for e in all.iter().rev().take(4) {
                if let ReaderEffect::ArmTimer(at) = e {
                    timer = Some(*at);
                    break;
                }
            }
        }
        let end = t0 + Duration::from_millis(400);
        all.extend(f.reader.handle(ReaderMsg::Timer, end));
        let speaks = all
            .iter()
            .filter(|e| matches!(e, ReaderEffect::Speak(_)))
            .count();
        assert!(speaks <= 2, "{speaks} utterances for 300 focus events");
        assert!(speaks >= 1, "the last focus is spoken");
    }

    /// An isolated focus on a cached node is still spoken at once (L1h).
    #[test]
    fn an_isolated_hit_is_not_delayed() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "n", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let t0 = Instant::now();
        let fx = focus_on(&mut f, 1, t0);
        assert_eq!(spoken(&fx).len(), 1);
        // Slower than a storm (30 Hz key repeat): also at once.
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(2, "m", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let fx = focus_on(&mut f, 2, t0 + Duration::from_millis(33));
        assert_eq!(spoken(&fx).len(), 1);
    }

    /// 5.1: a fetch reply that says `defunct` is a removal; nothing is spoken from it.
    #[test]
    fn a_defunct_reply_is_not_spoken() {
        let mut f = fixture();
        let now = Instant::now();
        focus_on(&mut f, 2, now);
        let fx = f.reader.handle(
            reply(
                &f,
                2,
                FetchKind::Focus,
                with_name(2, "dying", StateSet::DEFUNCT),
            ),
            now,
        );
        assert!(spoken(&fx).is_empty());
        assert!(matches!(f.reader.focus(), FocusState::Orphaned { .. }));
    }

    /// 5.2: a stale NAME is not presented from memory; the node is fetched and the reply spoken.
    #[test]
    fn a_stale_name_is_fetched_but_a_stale_state_is_spoken_from_memory() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![
                with_name(1, "Renamed?", StateSet::empty()),
                with_name(2, "Toggle", StateSet::empty()),
            ]),
            gorca_mirror::ApplyOrigin::Push,
        );
        f.writer.invalidate(key(1), PropMask::NAME, Instant::now());
        f.writer
            .invalidate(key(2), PropMask::STATES, Instant::now());
        let fx = focus_on(&mut f, 1, Instant::now());
        assert_eq!(effects_of(&fx), ["fetch-focus", "demand"]);
        let fx = focus_on(&mut f, 2, Instant::now() + Duration::from_secs(1));
        assert_eq!(effects_of(&fx), ["speak", "settled", "fetch-rv1", "demand"]);
        assert!(matches!(
            fx.last(),
            Some(ReaderEffect::Demand {
                outcome: DemandOutcome::Stale { .. },
                ..
            })
        ));
    }

    /// A partial node (a dump row without a value) that lacks nothing blocking is spoken.
    #[test]
    fn a_partial_node_missing_only_non_blocking_fields_is_spoken() {
        let mut f = fixture();
        let mut row = (*with_name(1, "Row", StateSet::empty())).clone();
        row.have = PropMask::CACHE_ITEM;
        row.interfaces = gorca_model::Ifaces::ACTION;
        f.writer.apply(
            SnapshotBatch::new(vec![Arc::new(row)]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let fx = focus_on(&mut f, 1, Instant::now());
        // No Value or Text interface: VALUE is not needed, so this is a full hit.
        assert!(matches!(
            fx.last(),
            Some(ReaderEffect::Demand {
                outcome: DemandOutcome::Hit,
                ..
            })
        ));
        let mut slider = (*with_name(2, "Vol", StateSet::empty())).clone();
        slider.have = PropMask::CACHE_ITEM;
        slider.interfaces = gorca_model::Ifaces::VALUE;
        f.writer.apply(
            SnapshotBatch::new(vec![Arc::new(slider)]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let fx = focus_on(&mut f, 2, Instant::now() + Duration::from_secs(1));
        assert!(matches!(
            fx.last(),
            Some(ReaderEffect::Demand {
                outcome: DemandOutcome::Partial { .. },
                ..
            })
        ));
        assert_eq!(utterance(&fx).segments[0].text.as_ref(), "Vol");
    }

    /// 5.5: focus on a removed key is not final; one Interactive fetch decides.
    #[test]
    fn focus_on_a_removed_key_asks_once_and_a_live_reply_revives_it() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Row", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        f.writer
            .tombstone(key(1), Instant::now(), gorca_mirror::RemovalCause::Event);
        let now = Instant::now();
        let fx = focus_on(&mut f, 1, now);
        assert_eq!(effects_of(&fx), ["fetch-focus", "demand"]);
        assert!(!matches!(f.reader.focus(), FocusState::Orphaned { .. }));
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::Focus,
                with_name(1, "Row", StateSet::empty()),
            ),
            now,
        );
        assert_eq!(spoken(&fx), ["Row check box"]);
    }

    #[test]
    fn unknown_object_keeps_a_removed_focus_dead() {
        let mut f = fixture();
        f.writer
            .tombstone(key(1), Instant::now(), gorca_mirror::RemovalCause::Event);
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        let fx = f.reader.handle(
            ReaderMsg::Reply(QueryReply {
                node: key(1),
                epoch: f.epoch.current(),
                kind: FetchKind::Focus,
                result: Err(BackendError::Gone),
            }),
            now,
        );
        assert!(spoken(&fx).is_empty());
        assert!(matches!(f.reader.focus(), FocusState::Orphaned { .. }));
    }

    /// `Unresolved` (path evicted) is a miss, never a removal: the cache answers.
    #[test]
    fn unresolved_falls_back_to_the_cache_not_to_removal() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Row", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        f.writer.invalidate(key(1), PropMask::NAME, Instant::now());
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        let fx = f.reader.handle(
            ReaderMsg::Reply(QueryReply {
                node: key(1),
                epoch: f.epoch.current(),
                kind: FetchKind::Focus,
                result: Err(BackendError::Unresolved),
            }),
            now,
        );
        assert_eq!(spoken(&fx), ["Row check box"]);
        assert!(utterance(&fx).may_refine);
        assert!(!matches!(f.reader.focus(), FocusState::Orphaned { .. }));
    }

    /// S1 (5.6): a state change of the focused node is fetched and spoken as the change.
    #[test]
    fn a_state_change_of_the_focused_node_is_spoken() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Wi-Fi", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::StateChanged {
                node: key(1),
                mask: PropMask::STATES,
            }),
            now,
        );
        assert!(effects_of(&fx).contains(&"fetch-s1"));
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::StateChange,
                with_name(1, "Wi-Fi", StateSet::CHECKED),
            ),
            now,
        );
        assert_eq!(spoken(&fx), ["checked"]);
        // And back: the opposite flip is spoken too.
        let fx = f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::StateChange,
                with_name(1, "Wi-Fi", StateSet::empty()),
            ),
            now,
        );
        assert_eq!(spoken(&fx), ["not checked"]);
        assert_eq!(f.reader.counters().state_changes_spoken, 2);
    }

    #[test]
    fn a_state_change_of_another_node_is_not_the_readers_business() {
        let mut f = fixture();
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "A", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        let fx = f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::StateChanged {
                node: key(2),
                mask: PropMask::STATES,
            }),
            now,
        );
        assert!(fx.is_empty());
    }

    /// PW-10: presentations and corrections are logged in the documented shape.
    #[test]
    fn the_presentation_log_records_hits_and_corrections() {
        use crate::present::{NoLabels, PresentLog};
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let log = PresentLog::new(sink.clone(), Arc::new(NoLabels)).unwrap();
        let mut f = fixture();
        f.reader = f.reader.with_present_log(Arc::clone(&log));
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Old", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        let now = Instant::now();
        focus_on(&mut f, 1, now);
        f.reader.handle(
            reply(
                &f,
                1,
                FetchKind::Revalidate,
                with_name(1, "New", StateSet::empty()),
            ),
            now,
        );
        f.reader = Reader::new(
            Arc::clone(&f.epoch),
            Arc::new(MirrorDirectory::new()),
            ModeKeys::default(),
        );
        drop(log);
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let lines: Vec<serde_json::Value> = text
            .lines()
            .skip(1) // the header line with the redaction key
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0]["source"], "hit");
        assert_eq!(lines[0]["name"], "Old");
        assert_eq!(lines[0]["may_refine"], true);
        assert_eq!(lines[1]["source"], "refine");
        assert_eq!(lines[1]["name"], "New");
        assert_eq!(lines[1]["refines"], lines[0]["utt_id"]);
        assert_eq!(lines[1]["role"], "check_box");
    }

    /// A switcher item presented as `SelectedItem` is logged with `selected` even when the
    /// cached snapshot predates the event (the spoken text omits it).
    #[test]
    fn a_selected_item_is_logged_with_selected_set() {
        use crate::present::{NoLabels, PresentLog};
        use std::io::Write;
        use std::sync::Mutex;

        #[derive(Clone, Default)]
        struct Sink(Arc<Mutex<Vec<u8>>>);
        impl Write for Sink {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(b);
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let sink = Sink::default();
        let log = PresentLog::new(sink.clone(), Arc::new(NoLabels)).unwrap();
        let mut f = fixture();
        f.reader = f.reader.with_present_log(Arc::clone(&log));
        f.writer.apply(
            SnapshotBatch::new(vec![with_name(1, "Window 1", StateSet::empty())]),
            gorca_mirror::ApplyOrigin::Push,
        );
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::ItemSelected { node: key(1) }),
            Instant::now(),
        );
        f.reader = Reader::new(
            Arc::clone(&f.epoch),
            Arc::new(MirrorDirectory::new()),
            ModeKeys::default(),
        );
        drop(log);
        let text = String::from_utf8(sink.0.lock().unwrap().clone()).unwrap();
        let first: serde_json::Value = serde_json::from_str(text.lines().nth(1).unwrap()).unwrap();
        assert_eq!(first["name"], "Window 1", "{text}");
        let states = first["states"].as_array().unwrap();
        assert!(states.iter().any(|s| s == "selected"), "{text}");
    }
}
