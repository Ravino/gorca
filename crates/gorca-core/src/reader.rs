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
use gorca_input_api::{CmdId, GrabSet, KeyIntent};
use gorca_model::{AppId, Epoch, NodeKey, NodeSnapshot, Politeness, PropMask, UttId};
use gorca_rt::{BudgetClass, EpochCell, FocusedApp, mpsc};
use smallvec::SmallVec;

use crate::SpeechIntent;
use crate::directory::MirrorDirectory;
use crate::focus::{FocusMsg, FocusState};
use crate::mode::{Mode, ModeKeys, ModeMsg};
use crate::present::PresenterRegistry;
use crate::review::ReviewMsg;
use crate::text::TextMsg;

/// Bounds from section 6.2.
const KEY_QUEUE: usize = 64;
const FOCUS_QUEUE: usize = 256;
const REPLY_QUEUE: usize = 64;
const BACKGROUND_QUEUE: usize = 64;
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
}

/// Reply to a Reader query, tagged with the epoch it was asked in (E-3).
#[derive(Clone, Debug)]
pub struct QueryReply {
    pub node: NodeKey,
    pub epoch: Epoch,
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
    Review(ReviewMsg),
    Text(TextMsg),
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
    },
    QueryActiveWindowFocus,
    /// Wake the Reader at this time with `ReaderMsg::Timer`.
    ArmTimer(Instant),
    SetGrabSet(GrabSet),
    RunCommand(CmdId),
}

pub type ReaderEffects = SmallVec<[ReaderEffect; 4]>;

/// Producer side handed to the input lane, app actors and workers.
#[derive(Clone, Debug)]
pub struct ReaderSenders {
    pub key: mpsc::Sender<KeyIntent>,
    pub focus: mpsc::Sender<FocusLaneMsg>,
    pub replies: mpsc::Sender<QueryReply>,
    pub background: mpsc::Sender<BackgroundMsg>,
}

#[derive(Debug)]
pub struct ReaderInbox {
    key: mpsc::Receiver<KeyIntent>,
    focus: mpsc::Receiver<FocusLaneMsg>,
    replies: mpsc::Receiver<QueryReply>,
    background: mpsc::Receiver<BackgroundMsg>,
}

pub fn reader_channels() -> (ReaderSenders, ReaderInbox) {
    let (key_tx, key) = mpsc::channel(KEY_QUEUE);
    let (focus_tx, focus) = mpsc::channel(FOCUS_QUEUE);
    let (replies_tx, replies) = mpsc::channel(REPLY_QUEUE);
    let (background_tx, background) = mpsc::channel(BACKGROUND_QUEUE);
    (
        ReaderSenders {
            key: key_tx,
            focus: focus_tx,
            replies: replies_tx,
            background: background_tx,
        },
        ReaderInbox {
            key,
            focus,
            replies,
            background,
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
            Some(f) = self.focus.recv() => Some(ReaderMsg::Focus(f)),
            Some(r) = self.replies.recv() => Some(ReaderMsg::Reply(r)),
            Some(b) = self.background.recv() => Some(ReaderMsg::Background(b)),
            else => None,
        }
    }

    /// Biased poll: input, focus lane, query replies, background lane. Never waits.
    pub fn poll(&mut self) -> Option<ReaderMsg> {
        if let Ok(k) = self.key.try_recv() {
            return Some(ReaderMsg::Key(k));
        }
        if let Ok(f) = self.focus.try_recv() {
            return Some(ReaderMsg::Focus(f));
        }
        if let Ok(r) = self.replies.try_recv() {
            return Some(ReaderMsg::Reply(r));
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
        }
    }

    /// Publish the focused application through `focused` (lock-free, read by the connector).
    #[must_use]
    pub fn with_focused(mut self, focused: Arc<FocusedApp>) -> Self {
        self.focused_app = Some(focused);
        self
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

    /// Keys classified `Ambiguous` (M-16).
    pub fn ambiguous_keys(&self) -> u64 {
        self.ambiguous_keys
    }

    /// Replies discarded because their epoch was stale (E-3).
    pub fn stale_replies_dropped(&self) -> u64 {
        self.stale_replies_dropped
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
        match msg {
            ReaderMsg::Key(intent) => self.on_key(intent, &mut fx),
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
            ReaderMsg::Timer => self.apply_focus(FocusMsg::TimerElapsed { now }, &mut fx),
        }
        fx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_input_api::{KeyClass, KeyEvent, Modifiers};
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
                | ReaderEffect::QueryActiveWindowFocus
                | ReaderEffect::ArmTimer(_)
                | ReaderEffect::SetGrabSet(_)
                | ReaderEffect::RunCommand(_) => None,
            })
            .collect()
    }

    #[test]
    fn cache_hit_focus_speaks_immediately_with_the_new_epoch() {
        let mut f = fixture();
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(1, Role::Button, "OK")],
            ..Default::default()
        });
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
            [ReaderEffect::Fetch {
                class: BudgetClass::Interactive,
                ..
            }]
        ));
        let reply = QueryReply {
            node: key(2),
            epoch,
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
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(2, Role::Link, "Docs")],
            ..Default::default()
        });
        let reply = QueryReply {
            node: key(2),
            epoch,
            result: Err(BackendError::Timeout),
        };
        let fx = f.reader.handle(ReaderMsg::Reply(reply), now);
        assert_eq!(spoken(&fx), ["Docs link"]);
    }

    #[test]
    fn flapping_roles_wait_for_the_settle_timer() {
        let mut f = fixture();
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(1, Role::MenuItem, "Open")],
            ..Default::default()
        });
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
            class: KeyClass::Command(CmdId(9)),
            epoch: Epoch(41),
        };
        let fx = f.reader.handle(ReaderMsg::Key(intent), Instant::now());
        assert!(matches!(
            fx.as_slice(),
            [ReaderEffect::RunCommand(CmdId(9))]
        ));
        assert_eq!(f.epoch.current(), Epoch(41));

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
        f.writer.apply(SnapshotBatch {
            nodes: vec![
                selected_snap(1, "Terminal"),
                selected_snap(2, "Files"),
                selected_snap(3, "Firefox"),
                selected_snap(4, "Settings"),
            ],
            ..Default::default()
        });
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
        f.writer.apply(SnapshotBatch {
            nodes: vec![selected_snap(1, "Files")],
            ..Default::default()
        });
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
        f.writer.apply(SnapshotBatch {
            nodes: vec![selected_snap(1, "Files")],
            ..Default::default()
        });
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
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(1, Role::Button, "Claim")],
            ..Default::default()
        });
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
            result: Ok(Arc::new(focused)),
        };
        f.reader.handle(ReaderMsg::Reply(reply), now);
        assert_eq!(published.get(), Some(app()));

        // The focus moves to a node that shows no FOCUSED state: protection is not kept by the
        // application that held it; and an application that is gone loses it outright.
        let later = now + std::time::Duration::from_secs(2);
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(3, Role::Button, "Unvalidated")],
            ..Default::default()
        });
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
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(1, Role::Button, "OK")],
            ..Default::default()
        });
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
        assert_eq!(f.reader.mode(), Mode::InputHelp);
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
                ReaderMsg::Timer => "timer",
            })
            .collect();
        assert_eq!(order, ["key", "focus", "reply", "background"]);
    }

    #[test]
    fn drain_processes_a_burst_in_one_call() {
        let mut f = fixture();
        f.writer.apply(SnapshotBatch {
            nodes: vec![snap(1, Role::Button, "OK")],
            ..Default::default()
        });
        let (tx, mut inbox) = reader_channels();
        tx.focus
            .try_send(FocusLaneMsg::FocusChanged { node: key(1) })
            .unwrap();
        let fx = f.reader.drain(&mut inbox, Instant::now());
        assert_eq!(fx.len(), 1);
    }
}
