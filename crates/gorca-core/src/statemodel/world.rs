// SPDX-License-Identifier: GPL-3.0-or-later
//! The simulated desktop behind the model-based tests: one application with a window, two text
//! fields, a button, a terminal and an optional dialog. It owns the ground truth (focus, text,
//! caret, which nodes are alive) and answers every effect the Reader emits the way the lanes and
//! app actors would, so the Reader is driven end to end without any IPC.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gorca_connector_api::BackendError;
use gorca_input_api::{KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
use gorca_mirror::{ApplyOrigin, MirrorRead, MirrorWriter, RemovalCause, mirror};
use gorca_model::{
    AppId, AttrMap, ChildList, ConnectorId, Freshness, Ifaces, LazyText, LocalId, NodeKey,
    NodeSnapshot, PropMask, Rect, Relations, Role, SnapshotBatch, StateSet, TextDelta,
    TextWindowFill,
};
use gorca_rt::EpochCell;

use super::{KeyKind, Spoke};
use crate::commands::CoreCmd;
use crate::directory::MirrorDirectory;
use crate::mode::ModeKeys;
use crate::reader::{
    BackgroundMsg, FocusLaneMsg, QueryReply, Reader, ReaderEffect, ReaderEffects, ReaderMsg,
};
use crate::review::cluster::{BuildInput, build_line_model};
use crate::review::collect::collect;
use crate::review::{ReviewLimits, ReviewMsg, RrVerdict};
use crate::text::caret::Reply;
use crate::text::{TextMsg, TextOp};

pub(super) const WIN: u64 = 1;
pub(super) const ENTRY: u64 = 2;
pub(super) const BUTTON: u64 = 3;
pub(super) const TERM: u64 = 4;
pub(super) const AREA: u64 = 5;
pub(super) const DLG: u64 = 6;
pub(super) const DLG_ENTRY: u64 = 7;
pub(super) const DLG_BTN: u64 = 8;

pub(super) fn app() -> AppId {
    AppId {
        connector: ConnectorId(1),
        serial: 1,
    }
}

pub(super) fn key(n: u64) -> NodeKey {
    NodeKey {
        app: app(),
        local: LocalId(n),
    }
}

fn role_of(n: u64) -> Role {
    match n {
        WIN | DLG => Role::Window,
        ENTRY | AREA | DLG_ENTRY => Role::TextEntry,
        BUTTON | DLG_BTN => Role::Button,
        TERM => Role::Terminal,
        _ => Role::Panel,
    }
}

fn name_of(n: u64) -> &'static str {
    match n {
        WIN => "Main",
        ENTRY => "Name",
        BUTTON => "OK",
        TERM => "Terminal",
        AREA => "Notes",
        DLG => "Dialog",
        DLG_ENTRY => "Code",
        DLG_BTN => "Close",
        _ => "",
    }
}

fn parent_of(n: u64) -> Option<u64> {
    match n {
        ENTRY | BUTTON | TERM | AREA => Some(WIN),
        DLG_ENTRY | DLG_BTN => Some(DLG),
        _ => None,
    }
}

fn kids_of(n: u64, dialog: bool) -> Vec<u64> {
    match n {
        WIN => vec![ENTRY, BUTTON, TERM, AREA],
        DLG if dialog => vec![DLG_ENTRY, DLG_BTN],
        _ => vec![],
    }
}

/// A text buffer with a caret, in scalars.
#[derive(Clone, Debug, Default)]
pub(super) struct Buf {
    pub text: Vec<char>,
    pub caret: usize,
}

impl Buf {
    pub fn string(&self) -> String {
        self.text.iter().collect()
    }
    fn fill(&self) -> TextWindowFill {
        TextWindowFill {
            base: 0,
            text: Arc::from(self.string().as_str()),
            len: u32::try_from(self.text.len()).unwrap_or(u32::MAX),
            caret: u32::try_from(self.caret).ok(),
        }
    }
}

/// Ground truth of the simulated desktop.
pub(super) struct World {
    pub bufs: HashMap<u64, Buf>,
    pub alive: HashSet<u64>,
    /// The node that truly holds the keyboard focus.
    pub focus: Option<u64>,
    /// Has the Reader been told (a focus event was delivered for `focus`)?
    pub told: bool,
    pub history: usize,
    pub recalls: usize,
    pub prompt: usize,
}

/// The extension slot of the application, as the host reports it at the moment a terminal is
/// tracked.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum ExtState {
    /// No extension matches the application: the native terminal policy of the daemon runs.
    Absent,
    /// An extension host serves the terminal policy.
    Healthy,
    /// The policy slot is busy, faulted or disabled: the host gives no session.
    Busy,
}

#[derive(Debug)]
struct FaultExt {
    state: Arc<std::sync::atomic::AtomicU8>,
}

#[derive(Debug)]
struct FaultApp {
    state: Arc<std::sync::atomic::AtomicU8>,
}

impl crate::ExtSource for FaultExt {
    fn attach(&self, _app: AppId, _info: &gorca_model::AppInfo) -> Option<Arc<dyn crate::AppExt>> {
        None
    }
    fn detach(&self, _app: AppId) {}
    fn for_app(&self, _app: AppId) -> Option<Arc<dyn crate::AppExt>> {
        match self.state.load(Ordering::Relaxed) {
            0 => None,
            _ => Some(Arc::new(FaultApp {
                state: Arc::clone(&self.state),
            })),
        }
    }
    fn set_sink(&self, _sink: Arc<dyn crate::ExtSink>) {}
}

impl crate::AppExt for FaultApp {
    fn on_event(&self, _ev: &gorca_model::NodeEvent) -> crate::ExtVerdict {
        crate::ExtVerdict::Pass
    }
    fn present(
        &self,
        _snap: &NodeSnapshot,
        _ctx: crate::PresentContext,
        _epoch: u64,
    ) -> Option<Vec<gorca_model::Segment>> {
        None
    }
    fn command(&self, _ext: &str, _local: u32) {}
    fn terminal(
        &self,
        cfg: &gorca_ext_api::terminal::TermConfig,
    ) -> Option<Box<dyn crate::TermSession>> {
        use crate::TermProvider;
        (self.state.load(Ordering::Relaxed) == 1)
            .then(|| crate::terminal::tests::NativeTerm.open(cfg))
    }
}

pub(super) struct Sim {
    pub ext: Arc<std::sync::atomic::AtomicU8>,
    pub native: bool,
    pub r: Reader,
    pub w: World,
    pub writer: MirrorWriter,
    pub epoch: Arc<EpochCell>,
    pub t0: Instant,
    pub at: u64,
    pub dropped: Arc<AtomicU64>,
    /// Replies held back (a slow application), delivered at the start of the next step.
    pub held: Vec<ReaderMsg>,
    pub hold_replies: bool,
    pub fail_fetches: Option<BackendError>,
    /// While true the application's text events never reach the Reader (a flood overflow).
    pub lose_events: bool,
    /// A read was answered with a failure since the last focus event.
    pub failed_reads: bool,
    /// GTK4 text widgets: no caret event for typed or erased text, and a `text-selection-changed`
    /// behind every caret move (docs/reports/unified-editing.md).
    pub gtk4: bool,
    pub spoken: Vec<Spoke>,
    /// Cues the Reader asked the cue sink for.
    pub cues: Vec<gorca_output_api::CueId>,
    pub timers: Vec<Instant>,
}

impl Sim {
    /// `native`: the daemon's native terminal policy is installed (`--terminal-policy=native`);
    /// otherwise the policy comes from the extension host only (the WASM deployment), which
    /// starts healthy.
    pub fn new(native: bool) -> Self {
        let epoch = Arc::new(EpochCell::new());
        let dir = Arc::new(MirrorDirectory::new());
        let (writer, handle) = mirror();
        dir.insert(app(), &handle);
        let dropped = Arc::new(AtomicU64::new(0));
        let ext = Arc::new(std::sync::atomic::AtomicU8::new(u8::from(!native)));
        let mut r = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default());
        if native {
            r = r.with_term_provider(Arc::new(crate::terminal::tests::NativeTerm));
        }
        let r = r
            .with_text_dropped(Arc::clone(&dropped))
            .with_ext(Arc::new(FaultExt {
                state: Arc::clone(&ext),
            }));
        let mut bufs = HashMap::new();
        for n in [ENTRY, AREA, DLG_ENTRY] {
            bufs.insert(n, Buf::default());
        }
        let prompt = "me@box:~$ ".chars().count();
        bufs.insert(
            TERM,
            Buf {
                text: "me@box:~$ ".chars().collect(),
                caret: prompt,
            },
        );
        let mut s = Self {
            r,
            w: World {
                bufs,
                alive: [WIN, ENTRY, BUTTON, TERM, AREA].into_iter().collect(),
                focus: None,
                told: false,
                history: 0,
                recalls: 0,
                prompt,
            },
            writer,
            epoch,
            t0: Instant::now(),
            at: 1000,
            dropped,
            ext,
            native,
            held: Vec::new(),
            hold_replies: false,
            fail_fetches: None,
            lose_events: false,
            failed_reads: false,
            gtk4: false,
            spoken: Vec::new(),
            cues: Vec::new(),
            timers: Vec::new(),
        };
        s.sync_mirror(&[WIN, ENTRY, BUTTON, TERM, AREA]);
        s
    }

    pub fn now(&self) -> Instant {
        self.t0 + Duration::from_millis(self.at)
    }

    pub fn snapshot(&self, n: u64) -> Arc<NodeSnapshot> {
        let focused = self.w.focus == Some(n);
        let mut states = StateSet::SHOWING | StateSet::VISIBLE;
        if matches!(role_of(n), Role::TextEntry | Role::Button | Role::Terminal) {
            states |= StateSet::FOCUSABLE;
        }
        if matches!(role_of(n), Role::TextEntry) {
            states |= StateSet::EDITABLE;
        }
        if n == AREA {
            states |= StateSet::MULTILINE;
        }
        if focused {
            states |= StateSet::FOCUSED;
        }
        let have = PropMask::PRESENT | PropMask::CHILDREN | PropMask::PARENT | PropMask::BOUNDS;
        let rect = Some(Rect {
            x: 0,
            y: i32::try_from(n).unwrap_or(0) * 30,
            w: 200,
            h: 24,
        });
        Arc::new(NodeSnapshot {
            key: key(n),
            role: role_of(n),
            states,
            name: Some(Arc::from(name_of(n))),
            description: None,
            value: None,
            parent: parent_of(n).map(key),
            children: ChildList::Known(
                kids_of(n, self.w.alive.contains(&DLG))
                    .iter()
                    .map(|k| key(*k))
                    .collect(),
            ),
            interfaces: Ifaces::empty(),
            attrs: AttrMap::default(),
            relations: Relations::default(),
            bounds: rect,
            have,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    pub fn sync_mirror(&mut self, ns: &[u64]) {
        let nodes = ns
            .iter()
            .filter(|n| self.w.alive.contains(n))
            .map(|n| self.snapshot(*n))
            .collect();
        self.writer.apply(
            SnapshotBatch {
                nodes,
                truncated: false,
            },
            ApplyOrigin::Push,
        );
    }

    // ---- delivering messages and answering effects ---------------------------------------

    pub fn handle(&mut self, m: ReaderMsg) {
        let now = self.now();
        let fx = self.r.handle(m, now);
        self.pump(fx);
    }

    /// Take the effects of one message, record speech, answer every request, and recurse.
    fn pump(&mut self, fx: ReaderEffects) {
        let mut queue: Vec<ReaderEffect> = fx.into_iter().collect();
        queue.reverse();
        let mut budget = 2000;
        while let Some(e) = queue.pop() {
            budget -= 1;
            assert!(budget > 0, "a reply storm: the Reader keeps asking");
            for m in self.answer(e) {
                if self.hold_replies {
                    self.held.push(m);
                } else {
                    let now = self.now();
                    let fx = self.r.handle(m, now);
                    let mut v: Vec<ReaderEffect> = fx.into_iter().collect();
                    v.reverse();
                    v.extend(queue.drain(..));
                    queue = v;
                }
            }
        }
    }

    fn speak(&mut self, s: &crate::SpeechIntent) {
        let text = s
            .utterance
            .segments
            .iter()
            .map(|g| g.text.to_string())
            .collect::<Vec<_>>()
            .join(" ");
        self.spoken.push(Spoke {
            text,
            prio: s.utterance.prio,
            epoch: s.utterance.epoch,
        });
    }

    fn answer(&mut self, e: ReaderEffect) -> Vec<ReaderMsg> {
        if self.fail_fetches.is_some()
            && matches!(
                e,
                ReaderEffect::Fetch { .. } | ReaderEffect::TextFetch { .. }
            )
        {
            self.failed_reads = true;
        }
        match e {
            ReaderEffect::Speak(s) => {
                self.speak(&s);
                vec![]
            }
            ReaderEffect::Cue(c) => {
                self.cues.push(c);
                vec![]
            }
            ReaderEffect::SayAllChunk { intent, .. } => {
                self.speak(&intent);
                vec![]
            }
            ReaderEffect::Fetch {
                node, epoch, kind, ..
            } => {
                let result = match &self.fail_fetches {
                    Some(err) => Err(clone_err(err)),
                    None if self.w.alive.contains(&node.local.0) => Ok(self.snapshot(node.local.0)),
                    None => Err(BackendError::Gone),
                };
                vec![ReaderMsg::Reply(QueryReply {
                    node,
                    epoch,
                    kind,
                    result,
                })]
            }
            ReaderEffect::TextFetch { req, op, epoch, .. } => {
                let result = match (&self.fail_fetches, op) {
                    (Some(err), _) => Err(clone_err(err)),
                    (None, TextOp::Window { node, .. } | TextOp::Screen { node }) => {
                        match self.w.bufs.get(&node.local.0) {
                            Some(b) if self.w.alive.contains(&node.local.0) => {
                                Ok(Reply::Window(b.fill()))
                            }
                            Some(_) | None => Err(BackendError::Gone),
                        }
                    }
                    (None, TextOp::SetCaret { .. }) => Ok(Reply::Done),
                    (
                        None,
                        TextOp::AtOffset { .. } | TextOp::Selections { .. } | TextOp::Format { .. },
                    ) => Err(BackendError::NotSupported),
                };
                vec![ReaderMsg::Text(TextMsg::Fetched { req, epoch, result })]
            }
            ReaderEffect::ReviewBuild(b) => {
                let h = self.writer.reader();
                let c = collect(
                    &crate::review::build::HandleSrc(&h),
                    b.surface,
                    ReviewLimits::default(),
                );
                let result = if c.misses.is_empty() {
                    Ok(Arc::new(build_line_model(
                        &BuildInput {
                            app: app(),
                            surface: b.surface,
                            nodes: c.nodes,
                            built_rev: h.revision(),
                            built_at: Instant::now(),
                            truncated: c.truncated,
                        },
                        &ReviewLimits::default(),
                    )))
                } else {
                    Err(crate::review::BuildError::Hung)
                };
                vec![ReaderMsg::Review(ReviewMsg::Built {
                    build: b.build,
                    result,
                })]
            }
            ReaderEffect::ReviewNav { req, .. } => vec![ReaderMsg::Review(ReviewMsg::NavFetched {
                req,
                result: Ok(()),
            })],
            ReaderEffect::ReviewRevalidate { model, .. } => {
                vec![ReaderMsg::Review(ReviewMsg::Revalidated {
                    model,
                    verdict: RrVerdict::Same,
                })]
            }
            ReaderEffect::ReviewStatusBar { req, .. } => {
                vec![ReaderMsg::Review(ReviewMsg::StatusBarFound {
                    req,
                    result: Ok(None),
                })]
            }
            ReaderEffect::ReviewAct { req, .. } => vec![ReaderMsg::Review(ReviewMsg::ActionDone {
                req,
                result: Ok(()),
            })],
            ReaderEffect::ArmTimer(t) => {
                // As the runtime: a set, identical deadlines collapse (runtime.rs, N1).
                if !self.timers.contains(&t) {
                    self.timers.push(t);
                }
                vec![]
            }
            ReaderEffect::Demand { .. }
            | ReaderEffect::FocusSettled { .. }
            | ReaderEffect::QueryActiveWindowFocus
            | ReaderEffect::SetGrabSet(_)
            | ReaderEffect::SpeechCtl(_)
            | ReaderEffect::Config(_)
            | ReaderEffect::Web(_)
            | ReaderEffect::Desktop(_) => vec![],
        }
    }

    pub fn flush_held(&mut self, reverse: bool) {
        let mut held = std::mem::take(&mut self.held);
        if reverse {
            held.reverse();
        }
        let was = self.hold_replies;
        self.hold_replies = false;
        for m in held {
            self.handle(m);
        }
        self.hold_replies = was;
    }

    /// Let `ms` pass: every timer the Reader armed fires at its due time (in order, including
    /// the ones those firings arm), and the clock ends `ms` later.
    pub fn settle(&mut self, ms: u64) {
        let end = self.at + ms;
        let mut fired = 0;
        loop {
            fired += 1;
            assert!(
                fired < 300,
                "timer storm: the Reader keeps re-arming a due timer (at {} ms, next due {:?} ms, window end {end} ms)",
                self.at,
                self.timers
                    .first()
                    .map(|t| t.saturating_duration_since(self.t0).as_millis())
            );
            self.timers.sort();
            let Some(&t) = self.timers.first() else {
                break;
            };
            // Rounded up: a timer must not fire a fraction of a millisecond before it is due.
            let due = u64::try_from(
                t.saturating_duration_since(self.t0)
                    .as_micros()
                    .div_ceil(1000),
            )
            .unwrap_or(u64::MAX);
            if due > end {
                break;
            }
            self.timers.remove(0);
            if fired < 8 && std::env::var("GORCA_STATE_TIMERS").is_ok() {
                eprintln!(
                    "   timer fires: due {due} ms, clock {} ms, queued {}",
                    self.at,
                    self.timers.len()
                );
            }
            self.at = self.at.max(due);
            self.handle(ReaderMsg::Timer);
        }
        self.at = end;
    }

    // ---- the world's own events --------------------------------------------------------

    /// The application moves its focus to `n`: the truth changes, and (when `tell`) the focus
    /// events reach the Reader, `dups` more times.
    pub fn focus_to(&mut self, n: u64, tell: bool, dups: u8) {
        if !self.w.alive.contains(&n) {
            return;
        }
        let old = self.w.focus;
        self.w.focus = Some(n);
        let mut touched = vec![n];
        touched.extend(old);
        self.sync_mirror(&touched);
        self.w.told = tell;
        if tell {
            for _ in 0..=dups {
                self.handle(ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
                    node: key(n),
                }));
            }
        }
    }

    pub fn remove(&mut self, n: u64) {
        if !self.w.alive.remove(&n) {
            return;
        }
        self.writer
            .tombstone(key(n), Instant::now(), RemovalCause::Event);
        if self.w.focus == Some(n) {
            self.w.focus = None;
            self.w.told = false;
        }
        self.handle(ReaderMsg::Focus(FocusLaneMsg::NodeRemoved(key(n))));
    }

    pub fn open_dialog(&mut self) {
        for n in [DLG, DLG_ENTRY, DLG_BTN] {
            self.w.alive.insert(n);
        }
        self.w.bufs.insert(DLG_ENTRY, Buf::default());
        self.sync_mirror(&[DLG, DLG_ENTRY, DLG_BTN]);
        self.handle(ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
            window: key(DLG),
        }));
        self.focus_to(DLG_ENTRY, true, 0);
    }

    pub fn close_dialog(&mut self, refocus: bool, to: u64) {
        for n in [DLG_ENTRY, DLG_BTN, DLG] {
            self.remove(n);
        }
        if refocus {
            self.handle(ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
                window: key(WIN),
            }));
            self.focus_to(to, true, 0);
        }
    }

    pub fn set_ext(&mut self, st: ExtState) {
        self.ext.store(
            match st {
                ExtState::Absent => 0,
                ExtState::Healthy => 1,
                ExtState::Busy => 2,
            },
            Ordering::Relaxed,
        );
    }

    pub fn ext_state(&self) -> ExtState {
        match self.ext.load(Ordering::Relaxed) {
            0 => ExtState::Absent,
            1 => ExtState::Healthy,
            _ => ExtState::Busy,
        }
    }

    /// A node that went away comes back (a dialog reopened, a widget re-created).
    pub fn revive(&mut self, n: u64) {
        if self.w.alive.contains(&n)
            || (n == DLG_ENTRY || n == DLG_BTN) && !self.w.alive.contains(&DLG)
        {
            return;
        }
        self.w.alive.insert(n);
        self.sync_mirror(&[n]);
    }

    pub fn announce(&mut self) {
        self.handle(ReaderMsg::Background(BackgroundMsg::Announcement {
            app: app(),
            text: Arc::from("Battery low"),
            politeness: gorca_model::Politeness::Polite,
        }));
    }

    pub fn ext_speech(&mut self, interrupt: bool) {
        self.handle(ReaderMsg::Background(BackgroundMsg::ExtSpeech {
            text: Arc::from("extension says"),
            interrupt,
        }));
    }

    pub fn command(&mut self, cmd: CoreCmd, repeat: u8) {
        let epoch = self.epoch.advance();
        let at = self.now();
        self.handle(ReaderMsg::Key(KeyIntent {
            event: KeyEvent {
                keycode: 79,
                keysym: 0xff95,
                unichar: 0,
                modifiers: Modifiers::empty(),
                pressed: true,
            },
            kind: PressKind::Press,
            class: KeyClass::Command(cmd.id()),
            repeat,
            chord: None,
            epoch,
            at,
        }));
    }

    // ---- typing --------------------------------------------------------------------------

    /// The key reaches the application (class `PassToApp`), then the application reacts.
    pub fn type_key(&mut self, k: KeyKind, mods: Modifiers) {
        let epoch = self.epoch.advance();
        self.type_key_stamped(k, mods, epoch);
    }

    /// Typeahead: the input lane stamps every press before the Reader has handled the first.
    pub fn type_keys_ahead(&mut self, ks: &[KeyKind]) {
        let stamped: Vec<_> = ks.iter().map(|k| (*k, self.epoch.advance())).collect();
        for (k, epoch) in stamped {
            self.type_key_stamped(k, Modifiers::empty(), epoch);
        }
    }

    pub fn type_key_stamped(&mut self, k: KeyKind, mods: Modifiers, epoch: gorca_model::Epoch) {
        let at = self.now();
        let (keysym, unichar) = k.sym();
        self.handle(ReaderMsg::Key(KeyIntent {
            event: KeyEvent {
                keycode: 0,
                keysym,
                unichar,
                modifiers: mods,
                pressed: true,
            },
            kind: PressKind::Press,
            class: KeyClass::PassToApp,
            repeat: 1,
            chord: None,
            epoch,
            at,
        }));
        if mods.intersects(Modifiers::CONTROL | Modifiers::MOD1 | Modifiers::MOD4) {
            return;
        }
        self.app_reacts(k);
    }

    fn emit(&mut self, node: u64, delta: Option<TextDelta>, caret: Option<usize>) {
        let gtk4_field = self.gtk4 && matches!(node, ENTRY | AREA | DLG_ENTRY);
        let edited = delta.is_some();
        let node = key(node);
        if self.lose_events {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        if let Some(delta) = delta {
            self.handle(ReaderMsg::Text(TextMsg::Edit { node, delta }));
        }
        if let Some(c) = caret
            && !(gtk4_field && edited)
        {
            self.handle(ReaderMsg::Text(TextMsg::Caret {
                node,
                offset: u32::try_from(c).unwrap_or(0),
            }));
            if gtk4_field {
                self.handle(ReaderMsg::Text(TextMsg::Selection { node }));
            }
        }
    }

    fn lazy(c: char) -> LazyText {
        let mut tmp = [0u8; 4];
        LazyText {
            msg: Arc::from(c.encode_utf8(&mut tmp).as_bytes()),
            field: 0,
        }
    }

    fn insert_at(&mut self, n: u64, c: char) {
        let Some(b) = self.w.bufs.get_mut(&n) else {
            return;
        };
        let off = b.caret;
        b.text.insert(off, c);
        b.caret += 1;
        let caret = b.caret;
        self.emit(
            n,
            Some(TextDelta::Insert {
                offset: u32::try_from(off).unwrap_or(0),
                len: 1,
                text: Some(Self::lazy(c)),
            }),
            Some(caret),
        );
    }

    fn delete_before(&mut self, n: u64, floor: usize) {
        let Some(b) = self.w.bufs.get_mut(&n) else {
            return;
        };
        if b.caret <= floor || b.caret != b.text.len() && n == TERM {
            return;
        }
        b.caret -= 1;
        let (off, ch) = (b.caret, b.text.remove(b.caret));
        let caret = b.caret;
        self.emit(
            n,
            Some(TextDelta::Delete {
                offset: u32::try_from(off).unwrap_or(0),
                len: 1,
                text: Some(Self::lazy(ch)),
            }),
            Some(caret),
        );
    }

    fn app_reacts(&mut self, k: KeyKind) {
        let Some(n) = self.w.focus.filter(|n| self.w.alive.contains(n)) else {
            return;
        };
        match n {
            ENTRY | AREA | DLG_ENTRY => self.edit_field(n, k),
            TERM => self.terminal_reacts(k),
            BUTTON | DLG_BTN => {
                if k == KeyKind::Tab {
                    let next = if n == BUTTON { AREA } else { DLG_ENTRY };
                    self.focus_to(next, true, 0);
                }
            }
            _ => {}
        }
    }

    fn move_caret(&mut self, n: u64, k: KeyKind, floor: usize) {
        let Some(b) = self.w.bufs.get_mut(&n) else {
            return;
        };
        b.caret = match k {
            KeyKind::Left => b.caret.saturating_sub(1).max(floor),
            KeyKind::Right => (b.caret + 1).min(b.text.len()),
            KeyKind::Home => floor,
            _ => b.text.len(),
        };
        let c = b.caret;
        self.emit(n, None, Some(c));
    }

    fn edit_field(&mut self, n: u64, k: KeyKind) {
        match k {
            KeyKind::Letter(c) | KeyKind::Punct(c) => self.insert_at(n, c),
            KeyKind::Space => self.insert_at(n, ' '),
            KeyKind::Backspace => self.delete_before(n, 0),
            KeyKind::Enter => {
                if n == AREA {
                    self.insert_at(n, '\n');
                }
            }
            KeyKind::Tab => {
                // A text view inserts a tab character; an entry moves the focus on.
                if n == AREA {
                    self.insert_at(n, '\t');
                } else {
                    self.focus_to(if n == ENTRY { BUTTON } else { DLG_BTN }, true, 0);
                }
            }
            KeyKind::Left | KeyKind::Right | KeyKind::Home | KeyKind::End => {
                self.move_caret(n, k, 0);
            }
            KeyKind::Up | KeyKind::Down => {}
        }
    }

    fn terminal_reacts(&mut self, k: KeyKind) {
        match k {
            KeyKind::Letter(c) | KeyKind::Punct(c) => self.insert_at(TERM, c),
            KeyKind::Space => self.insert_at(TERM, ' '),
            KeyKind::Backspace => {
                let prompt = self.w.prompt;
                self.delete_before(TERM, prompt);
            }
            KeyKind::Enter => {
                // Run the command: a line of output and a fresh prompt.
                self.w.history += 1;
                let out = format!("\nout {}\nme@box:~$ ", self.w.history);
                self.terminal_replace(None, &out);
            }
            KeyKind::Up | KeyKind::Down => {
                // History recall: the line after the prompt is redrawn.
                self.w.recalls += 1;
                let recalled = format!("cmd{}", self.w.recalls);
                self.terminal_replace(Some(self.w.prompt), &recalled);
            }
            KeyKind::Tab => self.terminal_replace(None, "x"),
            KeyKind::Left | KeyKind::Right | KeyKind::Home | KeyKind::End => {
                let prompt = self.w.prompt;
                self.move_caret(TERM, k, prompt);
            }
        }
    }

    /// The terminal redraws: `from` is where the text is cut (the end when `None`), then `s`
    /// follows and the cursor goes to the end.
    fn terminal_replace(&mut self, from: Option<usize>, s: &str) {
        let Some(b) = self.w.bufs.get_mut(&TERM) else {
            return;
        };
        if let Some(f) = from {
            b.text.truncate(f);
        }
        b.text.extend(s.chars());
        b.caret = b.text.len();
        let c = b.caret;
        if s.starts_with('\n') {
            self.w.prompt = c;
        }
        self.emit(TERM, Some(TextDelta::Replaced { text: None }), Some(c));
    }

    /// A program streams a line of output above the prompt and the screen scrolls by one row
    /// (the first row leaves): the lines that stay keep their text.
    pub fn terminal_scroll_output(&mut self) {
        let Some(b) = self.w.bufs.get_mut(&TERM) else {
            return;
        };
        let old_len = b.text.len();
        let tail = old_len.saturating_sub(self.w.prompt);
        let text: String = b.text.iter().collect();
        let mut lines: Vec<String> = text.split('\n').map(str::to_owned).collect();
        if lines.len() >= 3 {
            lines.remove(0);
        }
        self.w.history += 1;
        let last = lines.pop().unwrap_or_default();
        lines.push(format!("flood {}", self.w.history));
        lines.push(last);
        b.text = lines.join("\n").chars().collect();
        b.caret = b.text.len();
        let c = b.caret;
        self.w.prompt = c.saturating_sub(tail);
        self.emit(TERM, Some(TextDelta::Replaced { text: None }), Some(c));
    }

    /// Output the shell prints on its own (a background job, a prompt refresh).
    pub fn terminal_async_output(&mut self) {
        let Some(b) = self.w.bufs.get_mut(&TERM) else {
            return;
        };
        b.text.clear();
        self.terminal_replace(None, "[1]+ done\nme@box:~$ ");
        self.w.prompt = self.w.bufs.get(&TERM).map_or(0, |b| b.text.len());
    }
}

fn clone_err(e: &BackendError) -> BackendError {
    match e {
        BackendError::Gone => BackendError::Gone,
        BackendError::Busy => BackendError::Busy,
        BackendError::NotSupported => BackendError::NotSupported,
        BackendError::Unresolved => BackendError::Unresolved,
        BackendError::Protocol(s) => BackendError::Protocol(s.clone()),
        BackendError::Timeout => BackendError::Timeout,
    }
}
