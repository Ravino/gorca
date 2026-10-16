//! Window activation, dialogs and alerts (M4b, phase-2.md M4; Orca `on_window_activated`).
//!
//! When a window is activated the user hears its title and, for a dialog or an alert, what it
//! says, followed by the control that took focus: one utterance, because the speech lane keeps
//! a single interrupting slot and several utterances submitted together replace each other
//! (M3 lesson). The pieces:
//!
//! - [`WindowState`] is the small state machine that pairs the `window:activate` event with
//!   the focus event that normally follows it (a few milliseconds later, in either order);
//! - [`dialog_content`] collects the static text of a dialog from the mirror (pure, bounded);
//! - [`fetch_content`] fills what the mirror lacks, in a task the runtime owns (deadline per
//!   request, never in the Reader);
//! - the `impl Reader` block wires them: lead-in text on the focus utterance, the title alone
//!   when no focus follows, and the late content as a follow-up that never interrupts.
//!
//! Nothing here names an application: it works on roles, states and the mirror, so GTK, Qt and
//! the Shell are all covered (ADR-0008 WE-1).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::{
    AppId, ChildList, Epoch, Lookup, NodeKey, NodeSnapshot, Prio, PropMask, Role, Segment,
    SegmentKind, StateSet, Utterance,
};
use gorca_rt::BudgetClass;
use smallvec::SmallVec;

use super::fair::Kind;
use super::utterance::seg;
use crate::SpeechIntent;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use crate::review::NavSource;
use crate::review::objnav::{PresClass, class};

/// How long an activation waits for the focus event that completes it. GTK sends the focus a few
/// milliseconds after the activation; a window without any focusable widget never does.
pub const ALONE_DELAY: Duration = Duration::from_millis(120);
/// A focus event this long after an activation (or before it) is still part of it.
pub const PAIR_WINDOW: Duration = Duration::from_millis(250);
/// An activation this soon after a focus presentation in the same app is the second half of the
/// pair (the focus came first): the title follows instead of leading.
pub const FOCUS_FIRST_WINDOW: Duration = Duration::from_millis(100);
/// How long a paired activation stays valid for the presentation of its focus (a cold focus
/// fetch can take its whole deadline).
const LEAD_VALID: Duration = Duration::from_millis(500);
/// Two activations of the same window closer than this are one (atk-bridge and the toolkit both
/// report it).
pub const DUPLICATE_ACTIVATION: Duration = Duration::from_millis(100);
/// Nodes visited, tree depth and spoken pieces of one dialog read (bounded, no allocation beyond
/// the result).
pub const CONTENT_MAX_VISITS: usize = 64;
pub const CONTENT_MAX_DEPTH: usize = 6;
pub const CONTENT_MAX_TEXTS: usize = 8;
/// Longest dialog text spoken, in characters; the rest is the user's to read with review.
pub const CONTENT_MAX_CHARS: usize = 600;
/// Expansions (one `Fetch` plus `ChildSnapshots`) a content read may spend, and its total time.
const FETCH_MAX_EXPANSIONS: usize = 8;
const FETCH_BUDGET: Duration = Duration::from_millis(600);

/// The window state machine: an activation waiting for its focus.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WindowState {
    Idle,
    /// `window:activate` seen at `since`; the focus event decides what happens next.
    Pending {
        window: NodeKey,
        since: Instant,
    },
}

#[derive(Copy, Clone, Debug)]
pub enum WindowMsg {
    /// `window:activate`. `after_focus`: a focus in the same app was presented moments ago.
    Activated {
        window: NodeKey,
        now: Instant,
        after_focus: bool,
    },
    /// A focus change began in `app`.
    FocusBegan {
        app: AppId,
        now: Instant,
    },
    Timer {
        now: Instant,
    },
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WindowEffect {
    None,
    /// Put the title (and dialog content) of `window` in front of the focus utterance.
    Lead {
        window: NodeKey,
    },
    /// Speak the title of `window` on its own now.
    Alone {
        window: NodeKey,
    },
    /// Wake the Reader at this time.
    Arm(Instant),
}

impl WindowState {
    pub fn step(self, msg: WindowMsg) -> (WindowState, WindowEffect) {
        match (self, msg) {
            (
                _,
                WindowMsg::Activated {
                    window,
                    now,
                    after_focus,
                },
            ) => {
                if after_focus {
                    (WindowState::Idle, WindowEffect::Alone { window })
                } else {
                    (
                        WindowState::Pending { window, since: now },
                        WindowEffect::Arm(now + ALONE_DELAY),
                    )
                }
            }
            (WindowState::Idle, WindowMsg::FocusBegan { .. } | WindowMsg::Timer { .. }) => {
                (WindowState::Idle, WindowEffect::None)
            }
            (WindowState::Pending { window, since }, WindowMsg::FocusBegan { app, now }) => {
                if window.app != app {
                    // Another app's focus says nothing about this window: it keeps waiting.
                    (self, WindowEffect::None)
                } else if now.saturating_duration_since(since) <= PAIR_WINDOW {
                    (WindowState::Idle, WindowEffect::Lead { window })
                } else {
                    // The activation is stale: it was nothing.
                    (WindowState::Idle, WindowEffect::None)
                }
            }
            (WindowState::Pending { window, since }, WindowMsg::Timer { now }) => {
                let due = since + ALONE_DELAY;
                if now >= due {
                    (WindowState::Idle, WindowEffect::Alone { window })
                } else {
                    // N1: a timer that fires for another subsystem re-arms what is not due.
                    (self, WindowEffect::Arm(due))
                }
            }
        }
    }
}

/// What a window utterance leads with: the title, and the role word for dialogs and alerts (a
/// plain window is its title; "window" after every application name is noise).
pub fn lead_segments(win: &NodeSnapshot) -> SmallVec<[Segment; 2]> {
    let mut out = SmallVec::new();
    if let Some(name) = win.name.as_deref().filter(|s| !s.trim().is_empty()) {
        out.push(seg(name, SegmentKind::Text));
    }
    match win.role {
        Role::Dialog | Role::Alert => out.push(seg(win.role.spoken(), SegmentKind::Role)),
        Role::Unknown
        | Role::Application
        | Role::Window
        | Role::Panel
        | Role::Document
        | Role::DocumentWeb
        | Role::Heading
        | Role::Paragraph
        | Role::Link
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Popover
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Label
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark => {}
    }
    out
}

/// Windows whose static text is read on open: dialogs and alerts (ARIA `alertdialog`).
pub fn reads_content(role: Role) -> bool {
    matches!(role, Role::Dialog | Role::Alert)
}

/// How a node takes part in a dialog read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Part {
    /// Text to speak (a label, a paragraph).
    Text,
    /// A container to look inside.
    Container,
    /// A control or anything else: not read here (the focus utterance or the user reaches it).
    Skip,
}

fn part(role: Role) -> Part {
    match role {
        Role::Label | Role::StaticText | Role::Paragraph | Role::Heading => Part::Text,
        Role::Unknown
        | Role::Window
        | Role::Dialog
        | Role::Alert
        | Role::Panel
        | Role::Landmark
        | Role::List
        | Role::ListItem
        | Role::Row
        | Role::Cell
        | Role::Popover
        | Role::Application => Part::Container,
        Role::Document
        | Role::DocumentWeb
        | Role::Link
        | Role::Image
        | Role::Table
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Button
        | Role::ToggleButton
        | Role::CheckBox
        | Role::RadioButton
        | Role::ComboBox
        | Role::TextEntry
        | Role::PasswordEntry
        | Role::SpinButton
        | Role::Slider
        | Role::ProgressBar
        | Role::Menu
        | Role::MenuBar
        | Role::MenuItem
        | Role::Tab
        | Role::TabList
        | Role::Tree
        | Role::TreeItem
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Terminal => Part::Skip,
    }
}

/// What a dialog read found in the mirror.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Content {
    /// The static text, in tree order, without the focused node, duplicates or the title.
    pub texts: Vec<Arc<str>>,
    /// Nodes to `expand` (fetch with their children) for a fuller answer.
    pub misses: Vec<NodeKey>,
    /// A bound stopped the walk.
    pub truncated: bool,
}

struct Walk<'a, S: NavSource> {
    src: &'a S,
    skip: Option<NodeKey>,
    title: Option<Arc<str>>,
    visits: usize,
    chars: usize,
    out: Content,
}

impl<S: NavSource> Walk<'_, S> {
    fn done(&self) -> bool {
        self.out.truncated
    }

    fn push_text(&mut self, name: &Arc<str>) {
        let text = name.trim();
        if text.is_empty()
            || self.title.as_deref().is_some_and(|t| t.trim() == text)
            || self.out.texts.iter().any(|t| t.trim() == text)
        {
            return;
        }
        if self.out.texts.len() >= CONTENT_MAX_TEXTS || self.chars + text.len() > CONTENT_MAX_CHARS
        {
            self.out.truncated = true;
            return;
        }
        self.chars += text.len();
        self.out.texts.push(Arc::from(text));
    }

    fn visit(&mut self, node: NodeKey, depth: usize, parent: NodeKey) {
        if self.done() {
            return;
        }
        if self.visits >= CONTENT_MAX_VISITS {
            self.out.truncated = true;
            return;
        }
        self.visits += 1;
        if self.skip == Some(node) {
            return;
        }
        let Some(v) = self.src.view(node).hit() else {
            // A child the mirror does not hold: its parent's listing brings it in.
            if !self.out.misses.contains(&parent) {
                self.out.misses.push(parent);
            }
            return;
        };
        let snap = &v.snap;
        if class(snap) == PresClass::Unavailable {
            return;
        }
        match part(snap.role) {
            Part::Skip => {}
            Part::Text => {
                if snap.have.contains(PropMask::NAME) {
                    if let Some(name) = &snap.name {
                        self.push_text(name);
                    }
                } else if !self.out.misses.contains(&node) {
                    self.out.misses.push(node);
                }
            }
            Part::Container => self.children(snap, depth),
        }
    }

    fn children(&mut self, snap: &NodeSnapshot, depth: usize) {
        if depth >= CONTENT_MAX_DEPTH {
            self.out.truncated = true;
            return;
        }
        match &snap.children {
            ChildList::Known(kids) => {
                for k in kids {
                    self.visit(*k, depth + 1, snap.key);
                }
            }
            ChildList::Count(0) => {}
            ChildList::Count(_) | ChildList::Unknown => {
                if !self.out.misses.contains(&snap.key) {
                    self.out.misses.push(snap.key);
                }
            }
        }
    }
}

/// The static text of `window` as the mirror holds it. `skip` (the focused node) is left out, and
/// so is a text equal to the title. Bounded by [`CONTENT_MAX_VISITS`], [`CONTENT_MAX_DEPTH`],
/// [`CONTENT_MAX_TEXTS`] and [`CONTENT_MAX_CHARS`].
pub fn dialog_content<S: NavSource>(src: &S, window: NodeKey, skip: Option<NodeKey>) -> Content {
    let mut w = Walk {
        src,
        skip,
        title: None,
        visits: 0,
        chars: 0,
        out: Content::default(),
    };
    match src.view(window) {
        Lookup::Hit(v) => {
            w.title.clone_from(&v.snap.name);
            w.visits = 1;
            let snap = Arc::clone(&v.snap);
            w.children(&snap, 0);
        }
        Lookup::Miss | Lookup::Gone => w.out.misses.push(window),
    }
    w.out
}

/// Spoken pieces of a dialog's text.
pub fn content_segments(texts: &[Arc<str>]) -> impl Iterator<Item = Segment> + '_ {
    texts.iter().map(|t| Segment {
        text: Arc::clone(t),
        kind: SegmentKind::Text,
    })
}

/// A follow-up utterance: it queues behind what is being spoken and dies when the user moves on
/// (`survives_focus_change` is false), because a stale dialog text is worse than none.
pub fn follow_up(
    segments: impl IntoIterator<Item = Segment>,
    id: gorca_model::UttId,
    epoch: Epoch,
) -> Utterance {
    Utterance {
        id,
        segments: segments.into_iter().collect(),
        lang: None,
        epoch,
        prio: Prio::Notification,
        may_refine: false,
        survives_focus_change: false,
        refines: None,
    }
}

/// Reader effects of the desktop presenters that need an application round trip. One variant of
/// `ReaderEffect` carries them all, so a presenter added later does not touch every exhaustive
/// match over effects.
#[derive(Clone, Debug)]
pub enum DesktopEffect {
    /// Read window `window`: its title and, for a dialog or an alert, its static text. The mirror
    /// lacked some of it. The answer comes back as `BackgroundMsg::Window`.
    Window { window: NodeKey },
    /// A node that just appeared and may be a notification (`present::notification`): fetch it
    /// and its text. The answer comes back as `BackgroundMsg::Notice`.
    Notice { node: NodeKey, epoch: Epoch },
    /// A level bar stopped changing (the Shell's OSD): read it and the label beside it. The answer
    /// comes back as `BackgroundMsg::LevelRead`.
    Level { node: NodeKey },
}

/// What a window read found. `snap` is `None` when the window could not be read at all.
#[derive(Clone, Debug)]
pub struct WindowInfo {
    pub window: NodeKey,
    pub snap: Option<Arc<NodeSnapshot>>,
    pub texts: Vec<Arc<str>>,
}

/// Fill what a window read lacks and return it: the node itself, then a few `expand` rounds for
/// a dialog or an alert, all under one budget. A refused, hung or gone application ends the
/// loop with what was found so far.
pub async fn fetch_window(io: &crate::review::ReviewIo, window: NodeKey) -> WindowInfo {
    use crate::review::NavOp;
    use crate::review::build::{HandleSrc, expand, nav_fetch};
    let src = HandleSrc(&io.mirror);
    let run = async {
        if src.view(window).hit().is_none() {
            let _ = nav_fetch(io, NavOp::Node { node: window }).await;
        }
        let Some(snap) = src.view(window).hit().map(|v| v.snap) else {
            return WindowInfo {
                window,
                snap: None,
                texts: Vec::new(),
            };
        };
        if !reads_content(snap.role) {
            return WindowInfo {
                window,
                snap: Some(snap),
                texts: Vec::new(),
            };
        }
        let mut spent = 0;
        let mut found = dialog_content(&src, window, None);
        while !found.misses.is_empty() && spent < FETCH_MAX_EXPANSIONS {
            let mut progressed = false;
            for m in std::mem::take(&mut found.misses) {
                if spent >= FETCH_MAX_EXPANSIONS {
                    break;
                }
                spent += 1;
                if expand(io, m, BudgetClass::Interactive, false).await.is_ok() {
                    progressed = true;
                }
            }
            if !progressed {
                break;
            }
            found = dialog_content(&src, window, None);
        }
        let snap = src.view(window).hit().map_or(snap, |v| v.snap);
        WindowInfo {
            window,
            snap: Some(snap),
            texts: found.texts,
        }
    };
    match tokio::time::timeout(FETCH_BUDGET, run).await {
        Ok(i) => i,
        Err(_) => WindowInfo {
            window,
            snap: src.view(window).hit().map(|v| v.snap),
            texts: dialog_content(&src, window, None).texts,
        },
    }
}

/// How long the focus utterance of a freshly activated dialog may wait for the window read that is
/// in flight, counted from the activation: the title and text then lead it in reading order. A
/// slower application gets the focus first and the text as a follow-up.
pub const WINDOW_HOLD: Duration = Duration::from_millis(10);

/// Activations, answers and reads kept at once; the oldest is dropped beyond it.
const PENDING_MAX: usize = 8;

/// A window answer arriving this soon after the focus utterance went out replaces that
/// utterance (still playing) with the complete line; later ones only add what is missing (N-1).
const REPLACE_WITHIN: Duration = Duration::from_millis(150);

/// An input event this recent before an activation makes the window the user's own (N-2).
const USER_INPUT_WINDOW: Duration = Duration::from_millis(600);

/// How long an app the user switched to stays the user's app without a focus event (N-2).
const USER_APP_VALID: Duration = Duration::from_secs(2);
/// Waiting activations one application may hold.
const PENDING_PER_APP: usize = 2;

/// Read a level bar and the label next to it (`texts[0]`): the node itself, then its parent's
/// children for a sibling label, under one budget.
pub async fn fetch_level(io: &crate::review::ReviewIo, node: NodeKey) -> WindowInfo {
    use crate::review::NavOp;
    use crate::review::build::{HandleSrc, expand, nav_fetch};
    let src = HandleSrc(&io.mirror);
    let run = async {
        let _ = nav_fetch(io, NavOp::Node { node }).await;
        let snap = src.view(node).hit().map(|v| v.snap);
        let mut texts = Vec::new();
        if let Some(parent) = snap.as_ref().and_then(|s| s.parent) {
            let _ = expand(io, parent, BudgetClass::Interactive, false).await;
            if let Some(p) = src.view(parent).hit()
                && let ChildList::Known(kids) = &p.snap.children
            {
                let label = kids.iter().find_map(|k| {
                    let v = src.view(*k).hit()?;
                    (v.snap.role == Role::Label)
                        .then(|| v.snap.name.clone())
                        .flatten()
                        .filter(|n| !n.trim().is_empty())
                });
                texts.extend(label);
            }
        }
        WindowInfo {
            window: node,
            snap,
            texts,
        }
    };
    match tokio::time::timeout(FETCH_BUDGET, run).await {
        Ok(i) => i,
        Err(_) => WindowInfo {
            window: node,
            snap: src.view(node).hit().map(|v| v.snap),
            texts: Vec::new(),
        },
    }
}

/// What the Reader remembers about window speech.
#[derive(Debug)]
pub(crate) struct WindowCtx {
    /// Activations waiting for their focus, oldest first. Several, so one application's
    /// activations cannot overwrite another's (fairness: each owner is also rate-limited).
    pending: SmallVec<[WindowState; PENDING_MAX]>,
    /// The activation to put in front of the focus utterance about to be presented, and when it
    /// was paired (a lead whose focus never presented must not attach itself to a later one).
    lead: Option<(NodeKey, Instant)>,
    last_activation: Option<(NodeKey, Instant)>,
    /// The latest window reads the applications answered (windows the mirror lacked), oldest
    /// first.
    known: SmallVec<[WindowInfo; PENDING_MAX]>,
    /// Window reads asked for and not answered yet, and when they were asked, oldest first.
    inflight: SmallVec<[(NodeKey, Instant); PENDING_MAX]>,
    /// The window whose title and text were last spoken, and what of it.
    led: Option<Led>,
    /// A focus utterance waiting for the window read (at most [`WINDOW_HOLD`]).
    held: Option<Held>,
    /// Latest key or control input, for telling a user's window switch from a background one.
    pub(crate) last_input: Option<Instant>,
    /// The app of the window the user switched to (input followed by its activation, or the
    /// window holds the `active` state) and when.
    user_app: Option<(AppId, Instant)>,
}

/// How the rest of a window's speech goes out when the application's answer arrives late.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum LedMode {
    /// The window led (or was meant to lead) the focus utterance of the focused app: the late
    /// answer is re-announced in reading order, ending with the focus again (M-2).
    Lead,
    /// The title of a window that followed the focus: queued behind it.
    Queued,
    /// The title of the focused app's window that no focus followed: the user's own context
    /// change, so it cancels stale speech, but only when something is actually said (H-1).
    Focused,
    /// Another application's window: a polite notice within that owner's speech budget, never
    /// touching the epoch (H-1).
    Background,
}

#[derive(Clone, Debug)]
struct Led {
    window: NodeKey,
    epoch: Epoch,
    title_done: bool,
    spoken: Vec<Arc<str>>,
    mode: LedMode,
    /// The focus utterance's own segments, repeated at the end of a [`LedMode::Lead`]
    /// re-announcement.
    tail: SmallVec<[Segment; 4]>,
    /// When the focus utterance this led went out ([`LedMode::Lead`] only).
    focus_at: Option<Instant>,
}

impl Led {
    fn new(window: NodeKey, epoch: Epoch, mode: LedMode) -> Self {
        Self {
            window,
            epoch,
            title_done: false,
            spoken: Vec::new(),
            mode,
            tail: SmallVec::new(),
            focus_at: None,
        }
    }
}

#[derive(Debug)]
struct Held {
    utt: Utterance,
    window: NodeKey,
    /// The focused control: its name is not read twice.
    focus: NodeKey,
    focus_name: Option<Arc<str>>,
    release_at: Instant,
}

impl WindowCtx {
    pub(crate) fn new() -> Self {
        Self {
            pending: SmallVec::new(),
            lead: None,
            last_activation: None,
            known: SmallVec::new(),
            inflight: SmallVec::new(),
            led: None,
            held: None,
            last_input: None,
            user_app: None,
        }
    }
}

impl Reader {
    /// Is a window activation paired with the focus about to be presented?
    pub(crate) fn window_lead_pending(&self) -> bool {
        self.win.lead.is_some()
    }

    /// `window:activate` of `window` (forwarded by its app actor).
    pub(crate) fn on_window_activated(&mut self, window: NodeKey, fx: &mut ReaderEffects) {
        let now = self.now;
        if let Some((w, at)) = self.win.last_activation
            && w == window
            && now.saturating_duration_since(at) < DUPLICATE_ACTIVATION
        {
            return;
        }
        // A window that activates over and over (a background app, a hostile one) must not cost
        // a read per event: bounded per owner. Over the budget the read is skipped but the
        // activation is kept, so it is the owner's oldest pending one that goes (never the
        // newest: the last Alt+Tab window is always spoken, L-2).
        let owner = self.dir.owner(window.app);
        let reads = self.notice.allow_activation(owner, now);
        let by_input = self
            .win
            .last_input
            .is_some_and(|at| now.saturating_duration_since(at) < USER_INPUT_WINDOW);
        let active = self
            .dir
            .lookup(window)
            .hit()
            .is_some_and(|s| s.states.contains(StateSet::ACTIVE));
        if by_input || active {
            self.win.user_app = Some((window.app, now));
        }
        self.win.last_activation = Some((window, now));
        self.win.known.retain(|k| k.window != window);
        // Ask the application for what the mirror lacks now, so the answer is in by the time the
        // focus event arrives a few milliseconds later.
        let complete = self.dir.lookup(window).hit().is_some_and(|snap| {
            !reads_content(snap.role) || dialog_content(&*self.dir, window, None).misses.is_empty()
        });
        self.win.inflight.retain(|(w, _)| *w != window);
        if !complete && reads {
            if self.win.inflight.len() >= PENDING_MAX {
                self.win.inflight.remove(0);
            }
            self.win.inflight.push((window, now));
            fx.push(ReaderEffect::Desktop(DesktopEffect::Window { window }));
        }
        // The focus came first only if the node that just took focus lives in this window: a
        // focus in the previous window of the same app is no part of this activation.
        let after_focus = self.last_focus.is_some_and(|(n, at)| {
            now.saturating_duration_since(at) < FOCUS_FIRST_WINDOW && self.is_inside(n, window)
        });
        self.window_step(
            WindowMsg::Activated {
                window,
                now,
                after_focus,
            },
            fx,
        );
    }

    /// Is `node` below `window` in the mirror (a few parent hops)? Unknown parents answer no.
    fn is_inside(&self, node: NodeKey, window: NodeKey) -> bool {
        let mut cur = node;
        for _ in 0..8 {
            if cur == window {
                return true;
            }
            let Some(p) = self.dir.lookup(cur).hit().and_then(|s| s.parent) else {
                return false;
            };
            cur = p;
        }
        false
    }

    pub(crate) fn window_timer(&mut self, fx: &mut ReaderEffects) {
        let now = self.now;
        if self.win.held.as_ref().is_some_and(|h| now >= h.release_at) {
            self.release_held(fx);
        }
        self.window_step(WindowMsg::Timer { now }, fx);
    }

    /// A focus change began: an activation waiting for it leads the focus utterance. A focus
    /// utterance still held for an earlier one is stale now.
    pub(crate) fn window_on_focus(&mut self, app: AppId) {
        self.win.held = None;
        let now = self.now;
        // The first activation of this app that its focus completes leads the utterance; the
        // others (other apps, or stale) are left to their own focus or timer.
        let mut lead = None;
        let mut keep: SmallVec<[WindowState; PENDING_MAX]> = SmallVec::new();
        for st in std::mem::take(&mut self.win.pending) {
            if lead.is_some() {
                keep.push(st);
                continue;
            }
            let (next, effect) = st.step(WindowMsg::FocusBegan { app, now });
            if let WindowEffect::Lead { window } = effect {
                lead = Some(window);
            }
            if next != WindowState::Idle {
                keep.push(next);
            }
        }
        self.win.pending = keep;
        if let Some(window) = lead {
            self.win.lead = Some((window, now));
        }
    }

    fn window_step(&mut self, msg: WindowMsg, fx: &mut ReaderEffects) {
        let interrupts = matches!(msg, WindowMsg::Timer { .. });
        let mut effects: SmallVec<[WindowEffect; PENDING_MAX]> = SmallVec::new();
        match msg {
            WindowMsg::Activated { .. } => {
                let (next, effect) = WindowState::Idle.step(msg);
                if let WindowState::Pending { window, .. } = next {
                    // One app keeps at most PENDING_PER_APP waiting activations (its own oldest
                    // goes first), so a flood from it cannot displace another app's.
                    let mine = |st: &WindowState| matches!(st, WindowState::Pending { window: w, .. } if w.app == window.app);
                    if self.win.pending.iter().filter(|st| mine(st)).count() >= PENDING_PER_APP
                        && let Some(i) = self.win.pending.iter().position(mine)
                    {
                        self.win.pending.remove(i);
                    }
                    if self.win.pending.len() >= PENDING_MAX {
                        // Never the focused app's own activation while another app's can go (N-3).
                        let focused = self.last_focus.map(|(n, _)| n.app);
                        let switched = self.win.user_app.map(|(a, _)| a);
                        let other = |st: &WindowState| {
                            !matches!(st, WindowState::Pending { window: w, .. }
                                if Some(w.app) == focused || Some(w.app) == switched)
                        };
                        let i = self.win.pending.iter().position(other).unwrap_or(0);
                        self.win.pending.remove(i);
                    }
                    self.win.pending.push(next);
                }
                effects.push(effect);
            }
            WindowMsg::Timer { .. } | WindowMsg::FocusBegan { .. } => {
                let mut keep: SmallVec<[WindowState; PENDING_MAX]> = SmallVec::new();
                for st in std::mem::take(&mut self.win.pending) {
                    let (next, effect) = st.step(msg);
                    if next != WindowState::Idle {
                        keep.push(next);
                    }
                    effects.push(effect);
                }
                self.win.pending = keep;
            }
        }
        for effect in effects {
            match effect {
                WindowEffect::None => {}
                WindowEffect::Arm(at) => fx.push(ReaderEffect::ArmTimer(at)),
                WindowEffect::Lead { window } => self.win.lead = Some((window, self.now)),
                WindowEffect::Alone { window } => self.speak_window_alone(window, interrupts, fx),
            }
        }
    }

    /// The snapshot of `window`: the mirror's, else what the application answered.
    fn window_snap(&self, window: NodeKey) -> Option<Arc<NodeSnapshot>> {
        self.dir.lookup(window).hit().or_else(|| {
            self.win
                .known
                .iter()
                .find(|k| k.window == window)
                .and_then(|k| k.snap.clone())
        })
    }

    /// The static text of a dialog: the mirror's walk, else what the application answered.
    fn window_texts(&self, window: NodeKey, skip: Option<NodeKey>) -> Vec<Arc<str>> {
        let mut texts = dialog_content(&*self.dir, window, skip).texts;
        if let Some(k) = self.win.known.iter().find(|k| k.window == window) {
            for t in &k.texts {
                if !texts.iter().any(|x| x.trim() == t.trim()) {
                    texts.push(Arc::clone(t));
                }
            }
        }
        texts
    }

    /// Does the installed application policy say the activation of this window is not worth
    /// speaking (the Shell's own "Main stage" window activates for every popup)?
    fn window_silenced(&self, snap: &NodeSnapshot) -> bool {
        use crate::builtin_ext::{PolicyFacts, PolicyMoment, PolicyVerdict};
        if !self.policy_active(snap.key.app) {
            return false;
        }
        let facts = PolicyFacts {
            moment: PolicyMoment::Activate,
            role: snap.role,
            states: snap.states,
            name: snap.name.as_deref(),
            description: snap.description.as_deref(),
            range: None,
        };
        self.decide_policy(snap.key.app, &facts) == PolicyVerdict::Silence
    }

    /// How a window's title goes out when no focus event followed it (`interrupts`) or when the
    /// focus came first and the title queues behind it.
    fn alone_mode(&mut self, window: NodeKey, interrupts: bool, fx: &mut ReaderEffects) -> LedMode {
        if !interrupts {
            return LedMode::Queued;
        }
        // Only the application the user is working in may cancel the user's speech; every other
        // application's window is a polite notice, however often it activates (H-1).
        let focused_app = self.last_focus.is_some_and(|(n, _)| n.app == window.app);
        let switched = self.win.user_app.is_some_and(|(a, at)| {
            a == window.app && self.now.saturating_duration_since(at) < USER_APP_VALID
        });
        if !focused_app && !switched {
            return LedMode::Background;
        }
        // The user's own focus speech, parked for a window read, goes out first and is never
        // dropped; the title then queues behind it instead of cancelling it.
        if self.win.held.is_some() {
            self.release_held(fx);
            return LedMode::Queued;
        }
        LedMode::Focused
    }

    /// Speak the window line `segs` of `led` according to its mode. The epoch advances here and
    /// only here, when an utterance really goes out and only for the focused app's own window.
    fn emit_window_line(
        &mut self,
        led: &mut Led,
        segs: impl IntoIterator<Item = Segment>,
        fx: &mut ReaderEffects,
    ) {
        let id = self.utt_id();
        let owner = self.dir.owner(led.window.app);
        match led.mode {
            LedMode::Focused if self.notice.fair.allow(owner, Kind::Speak, self.now) => {
                // A context change speaks at once and cancels the stale speech.
                led.epoch = self.epoch.advance();
                let mut u = follow_up(segs, id, led.epoch);
                u.prio = Prio::Message;
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
            LedMode::Focused | LedMode::Background => {
                let u = follow_up(segs, id, led.epoch);
                self.notice_speak(led.window.app, u, fx);
            }
            LedMode::Lead | LedMode::Queued => {
                let u = follow_up(segs, id, led.epoch);
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
        }
    }

    /// The title of a window that no focus event followed (`interrupts`) or that followed the
    /// focus (it queues behind it).
    fn speak_window_alone(&mut self, window: NodeKey, interrupts: bool, fx: &mut ReaderEffects) {
        let mode = self.alone_mode(window, interrupts, fx);
        let mut led = Led::new(window, self.epoch.current(), mode);
        let Some(snap) = self.window_snap(window) else {
            // Not read yet: the answer speaks it (`on_window_info`).
            self.win.led = Some(led);
            return;
        };
        if self.window_silenced(&snap) {
            return;
        }
        let lead = lead_segments(&snap);
        let texts = if reads_content(snap.role) {
            self.window_texts(window, None)
        } else {
            Vec::new()
        };
        led.title_done = true;
        led.spoken.clone_from(&texts);
        if !lead.is_empty() || !texts.is_empty() {
            self.emit_window_line(
                &mut led,
                lead.into_iter().chain(content_segments(&texts)),
                fx,
            );
        }
        self.win.led = Some(led);
    }

    /// Should the focus utterance wait for the window read? Only for a few milliseconds after the
    /// activation, and only while the read is in flight and has something to add: a window the
    /// mirror does not know yet, or a dialog whose text it lacks.
    fn window_read_pending(&self, window: NodeKey) -> Option<Instant> {
        let (_, since) = *self.win.inflight.iter().find(|(w, _)| *w == window)?;
        let due = since + WINDOW_HOLD;
        if self.now >= due {
            return None;
        }
        let wanted = match self.window_snap(window) {
            None => true,
            Some(s) => {
                reads_content(s.role) && !dialog_content(&*self.dir, window, None).misses.is_empty()
            }
        };
        wanted.then_some(due)
    }

    /// Put the activated window's title (and a dialog's text) in front of the focus utterance
    /// `u` for `snap`. Returns `true` when the utterance must wait for the window read (the
    /// caller hands it to [`Reader::window_hold`]); what the application answers later follows
    /// as a late utterance.
    pub(crate) fn window_lead_in(
        &mut self,
        snap: &NodeSnapshot,
        epoch: Epoch,
        u: &mut Utterance,
    ) -> bool {
        let Some((window, at)) = self.win.lead.take() else {
            return false;
        };
        if window == snap.key
            || window.app != snap.key.app
            || self.now.saturating_duration_since(at) > LEAD_VALID
        {
            return false;
        }
        if self.window_read_pending(window).is_some() {
            let mut led = Led::new(window, epoch, LedMode::Lead);
            led.focus_at = Some(self.now);
            self.win.led = Some(led);
            self.win.held = Some(Held {
                utt: u.clone(),
                window,
                focus: snap.key,
                focus_name: snap.name.clone(),
                release_at: self.now,
            });
            return true;
        }
        self.apply_lead(window, snap.key, snap.name.as_deref(), epoch, u);
        false
    }

    /// Insert the title and text of `window` in front of `u` as far as they are known, and record
    /// what was said.
    fn apply_lead(
        &mut self,
        window: NodeKey,
        focus: NodeKey,
        focus_name: Option<&str>,
        epoch: Epoch,
        u: &mut Utterance,
    ) {
        let mut led = Led::new(window, epoch, LedMode::Lead);
        led.focus_at = Some(self.now);
        led.tail = u.segments.iter().cloned().collect();
        let Some(win) = self.window_snap(window) else {
            self.win.led = Some(led);
            return;
        };
        if self.window_silenced(&win) {
            self.win.led = None;
            return;
        }
        let mut lead: SmallVec<[Segment; 6]> = lead_segments(&win).into_iter().collect();
        let mut texts = Vec::new();
        if reads_content(win.role) {
            texts = self.window_texts(window, Some(focus));
            // The focused control's own name is not read twice.
            texts.retain(|t| focus_name.is_none_or(|n| n.trim() != t.trim()));
            lead.extend(content_segments(&texts));
        }
        led.title_done = true;
        led.spoken = texts;
        self.win.led = Some(led);
        if !lead.is_empty() {
            u.segments.insert_many(0, lead);
        }
    }

    /// Park the focus utterance `u` until the window read answers or [`WINDOW_HOLD`] is over.
    pub(crate) fn window_hold(&mut self, u: Utterance, fx: &mut ReaderEffects) {
        let Some(h) = self.win.held.as_mut() else {
            fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            return;
        };
        let due = self
            .win
            .inflight
            .iter()
            .find(|(w, _)| *w == h.window)
            .map_or(self.now, |(_, since)| *since + WINDOW_HOLD);
        h.utt = u;
        h.release_at = due;
        fx.push(ReaderEffect::ArmTimer(due));
    }

    /// Speak the held focus utterance with whatever is known of its window by now.
    fn release_held(&mut self, fx: &mut ReaderEffects) {
        let Some(h) = self.win.held.take() else {
            return;
        };
        let mut u = h.utt;
        // The user moved on while it waited: the lane would drop it, and so do we.
        if u.epoch < self.epoch.current() {
            return;
        }
        let epoch = u.epoch;
        self.apply_lead(h.window, h.focus, h.focus_name.as_deref(), epoch, &mut u);
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// The application answered a window read.
    pub(crate) fn on_window_info(&mut self, info: &WindowInfo, fx: &mut ReaderEffects) {
        let window = info.window;
        self.win.inflight.retain(|(w, _)| *w != window);
        self.win.known.retain(|k| k.window != window);
        if self.win.known.len() >= PENDING_MAX {
            self.win.known.remove(0);
        }
        self.win.known.push(info.clone());
        // A focus utterance was waiting for exactly this: it leads with the answer.
        if self.win.held.as_ref().is_some_and(|h| h.window == window) {
            self.release_held(fx);
            return;
        }
        let Some(mut led) = self.win.led.clone() else {
            return;
        };
        // Only for the window just announced, and only while its speech is current.
        if led.window != window || led.epoch < self.epoch.current() {
            return;
        }
        let Some(snap) = info.snap.as_ref() else {
            return;
        };
        if self.window_silenced(snap) {
            return;
        }
        let fresh: Vec<Arc<str>> = info
            .texts
            .iter()
            .filter(|t| !led.spoken.iter().any(|s| s.trim() == t.trim()))
            .cloned()
            .collect();
        if led.title_done && fresh.is_empty() {
            return;
        }
        if led.mode == LedMode::Lead {
            let mut texts = std::mem::take(&mut led.spoken);
            let prior = texts.len();
            texts.extend(fresh);
            let near = led
                .focus_at
                .is_some_and(|at| self.now.saturating_duration_since(at) <= REPLACE_WITHIN);
            led.spoken = texts;
            if near {
                // The focus utterance is still playing: replace it by the complete line in
                // reading order (title, role, text, focus), so the focus is never heard twice.
                let segs: SmallVec<[Segment; 8]> = lead_segments(snap)
                    .into_iter()
                    .chain(content_segments(&led.spoken))
                    .chain(led.tail.iter().cloned())
                    .collect();
                led.title_done = true;
                led.mode = LedMode::Queued;
                let id = self.utt_id();
                let mut u = follow_up(segs, id, led.epoch);
                u.prio = Prio::Interrupt;
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            } else {
                // Too late to replace: only what the user has not heard, never the focus again.
                let lead = if led.title_done {
                    SmallVec::new()
                } else {
                    lead_segments(snap)
                };
                led.title_done = true;
                led.mode = LedMode::Queued;
                let new_texts = led.spoken.get(prior..).unwrap_or(&[]).to_vec();
                if !lead.is_empty() || !new_texts.is_empty() {
                    self.emit_window_line(
                        &mut led,
                        lead.into_iter().chain(content_segments(&new_texts)),
                        fx,
                    );
                }
            }
        } else {
            let lead = if led.title_done {
                SmallVec::new()
            } else {
                lead_segments(snap)
            };
            if lead.is_empty() && fresh.is_empty() {
                return;
            }
            led.title_done = true;
            led.spoken.extend(fresh.iter().cloned());
            self.emit_window_line(
                &mut led,
                lead.into_iter().chain(content_segments(&fresh)),
                fx,
            );
        }
        self.win.led = Some(led);
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_enum_match_arm)]
pub(crate) mod tests {
    use super::*;
    use crate::directory::MirrorDirectory;
    use crate::mode::ModeKeys;
    use crate::reader::{BackgroundMsg, FocusLaneMsg, ReaderMsg};
    use gorca_mirror::{ApplyOrigin, MirrorWriter, mirror};
    use gorca_model::{ConnectorId, Freshness, Ifaces, LocalId, SnapshotBatch, StateSet};
    use gorca_rt::EpochCell;
    use std::collections::HashMap;

    pub(crate) fn app(n: u32) -> AppId {
        AppId {
            connector: ConnectorId(1),
            serial: n,
        }
    }

    pub(crate) fn key(n: u64) -> NodeKey {
        NodeKey {
            app: app(1),
            local: LocalId(n),
        }
    }

    pub(crate) struct Spec {
        pub n: u64,
        pub role: Role,
        pub name: &'static str,
        pub kids: &'static [u64],
        pub states: StateSet,
    }

    pub(crate) fn node(n: u64, role: Role, name: &'static str, kids: &'static [u64]) -> Spec {
        Spec {
            n,
            role,
            name,
            kids,
            states: StateSet::SHOWING | StateSet::VISIBLE,
        }
    }

    pub(crate) fn snap(s: &Spec) -> Arc<NodeSnapshot> {
        Arc::new(NodeSnapshot {
            key: key(s.n),
            role: s.role,
            states: s.states,
            name: (!s.name.is_empty()).then(|| Arc::from(s.name)),
            description: None,
            value: None,
            parent: None,
            children: if s.kids.is_empty() {
                ChildList::Count(0)
            } else {
                ChildList::Known(s.kids.iter().map(|k| key(*k)).collect())
            },
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Fresh { at: Instant::now() },
        })
    }

    /// A map as a navigation source.
    struct Map(HashMap<NodeKey, Arc<NodeSnapshot>>);

    impl NavSource for Map {
        fn view(&self, k: NodeKey) -> Lookup<gorca_model::NodeView> {
            match self.0.get(&k) {
                Some(s) => Lookup::Hit(gorca_model::NodeView {
                    snap: Arc::clone(s),
                    stale: PropMask::empty(),
                    aged: false,
                }),
                None => Lookup::Miss,
            }
        }
    }

    fn map(specs: &[Spec]) -> Map {
        Map(specs.iter().map(|s| (key(s.n), snap(s))).collect())
    }

    fn texts(c: &Content) -> Vec<&str> {
        c.texts.iter().map(|t| &**t).collect()
    }

    // ---- the state machine ----

    fn t0() -> Instant {
        Instant::now()
    }

    #[test]
    fn an_activation_waits_for_its_focus_and_leads_it() {
        let t = t0();
        let (s, e) = WindowState::Idle.step(WindowMsg::Activated {
            window: key(1),
            now: t,
            after_focus: false,
        });
        assert_eq!(e, WindowEffect::Arm(t + ALONE_DELAY));
        let (s, e) = s.step(WindowMsg::FocusBegan {
            app: app(1),
            now: t + Duration::from_millis(5),
        });
        assert_eq!(e, WindowEffect::Lead { window: key(1) });
        assert_eq!(s, WindowState::Idle);
    }

    #[test]
    fn a_window_without_a_focus_is_spoken_alone_when_its_time_is_up() {
        let t = t0();
        let (s, _) = WindowState::Idle.step(WindowMsg::Activated {
            window: key(1),
            now: t,
            after_focus: false,
        });
        // A timer for another subsystem re-arms what is not due (N1).
        let (s, e) = s.step(WindowMsg::Timer {
            now: t + Duration::from_millis(10),
        });
        assert_eq!(e, WindowEffect::Arm(t + ALONE_DELAY));
        let (s, e) = s.step(WindowMsg::Timer {
            now: t + ALONE_DELAY,
        });
        assert_eq!(e, WindowEffect::Alone { window: key(1) });
        assert_eq!(s, WindowState::Idle);
    }

    #[test]
    fn a_focus_in_another_app_waits_and_one_far_later_is_not_the_windows_focus() {
        let t = t0();
        let pending = WindowState::Pending {
            window: key(1),
            since: t,
        };
        let (s, e) = pending.step(WindowMsg::FocusBegan {
            app: app(2),
            now: t,
        });
        assert_eq!(
            (s, e),
            (pending, WindowEffect::None),
            "another app's focus leaves it waiting"
        );
        let (s, e) = pending.step(WindowMsg::FocusBegan {
            app: app(1),
            now: t + PAIR_WINDOW + Duration::from_millis(1),
        });
        assert_eq!((s, e), (WindowState::Idle, WindowEffect::None));
    }

    #[test]
    fn an_activation_after_its_focus_speaks_at_once_without_waiting() {
        let (s, e) = WindowState::Idle.step(WindowMsg::Activated {
            window: key(1),
            now: t0(),
            after_focus: true,
        });
        assert_eq!(e, WindowEffect::Alone { window: key(1) });
        assert_eq!(s, WindowState::Idle);
    }

    // ---- what a dialog says ----

    #[test]
    fn a_dialog_reads_its_labels_in_tree_order_and_not_its_controls() {
        let src = map(&[
            node(1, Role::Dialog, "Delete file", &[2, 3, 4, 5]),
            node(2, Role::Label, "Delete file", &[]),
            node(3, Role::Panel, "", &[6, 7]),
            node(4, Role::Button, "Cancel", &[]),
            node(5, Role::Button, "Delete", &[]),
            node(6, Role::Label, "This cannot be undone.", &[]),
            node(7, Role::Label, "  The file will be lost.  ", &[]),
        ]);
        let c = dialog_content(&src, key(1), Some(key(5)));
        assert_eq!(
            texts(&c),
            ["This cannot be undone.", "The file will be lost."],
            "the title label repeats the title; buttons are not read"
        );
        assert!(c.misses.is_empty() && !c.truncated);
    }

    #[test]
    fn hidden_labels_and_the_focused_node_are_left_out() {
        let mut hidden = node(3, Role::Label, "Hidden", &[]);
        hidden.states = StateSet::empty();
        let src = map(&[
            node(1, Role::Alert, "Warning", &[2, 3, 4]),
            node(2, Role::Label, "Visible", &[]),
            hidden,
            node(4, Role::Label, "Focused label", &[]),
        ]);
        let c = dialog_content(&src, key(1), Some(key(4)));
        assert_eq!(texts(&c), ["Visible"]);
    }

    #[test]
    fn what_the_mirror_lacks_is_reported_as_nodes_to_expand() {
        // 2 is a child of 1 that the mirror does not hold; 3 has an unknown child list.
        let mut unknown = snap(&node(3, Role::Panel, "", &[]));
        Arc::make_mut(&mut unknown).children = ChildList::Unknown;
        let mut m = map(&[node(1, Role::Dialog, "D", &[2, 3])]);
        m.0.insert(key(3), unknown);
        let c = dialog_content(&m, key(1), None);
        assert_eq!(c.misses, [key(1), key(3)]);
        assert!(c.texts.is_empty());
        // The window itself missing: expand it.
        let c = dialog_content(&Map(HashMap::new()), key(9), None);
        assert_eq!(c.misses, [key(9)]);
    }

    #[test]
    fn a_dialog_read_is_bounded() {
        static KIDS: [u64; 12] = [2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];
        let mut specs = vec![node(1, Role::Dialog, "D", &KIDS)];
        let names = ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l"];
        for (i, n) in names.iter().enumerate() {
            specs.push(node(2 + i as u64, Role::Label, n, &[]));
        }
        let c = dialog_content(&map(&specs), key(1), None);
        assert_eq!(c.texts.len(), CONTENT_MAX_TEXTS);
        assert!(c.truncated);
    }

    #[test]
    fn only_dialogs_and_alerts_say_their_role_in_the_lead() {
        let d = snap(&node(1, Role::Dialog, "Save", &[]));
        let w = snap(&node(2, Role::Window, "Files", &[]));
        let n = snap(&node(3, Role::Window, "", &[]));
        let words = |s: &NodeSnapshot| -> Vec<String> {
            lead_segments(s)
                .iter()
                .map(|g| g.text.to_string())
                .collect()
        };
        assert_eq!(words(&d), ["Save", "dialog"]);
        assert_eq!(words(&w), ["Files"]);
        assert!(words(&n).is_empty(), "a nameless window says nothing");
    }

    // ---- the Reader ----

    pub(crate) struct Fx {
        pub reader: Reader,
        pub _writer: MirrorWriter,
        pub epoch: Arc<EpochCell>,
    }

    pub(crate) fn fixture(specs: &[Spec]) -> Fx {
        let epoch = Arc::new(EpochCell::new());
        let dir = Arc::new(MirrorDirectory::new());
        let (mut writer, handle) = mirror();
        dir.insert(app(1), &handle);
        // Every test application is its own process unless a test says otherwise.
        for a in 1..100 {
            dir.set_pid(app(a), Some(10_000 + a));
        }
        let mut nodes: Vec<Arc<NodeSnapshot>> = specs.iter().map(snap).collect();
        for s in specs {
            for k in s.kids {
                for n in &mut nodes {
                    if n.key == key(*k) {
                        Arc::make_mut(n).parent = Some(key(s.n));
                    }
                }
            }
        }
        writer.apply(
            SnapshotBatch {
                nodes,
                ..Default::default()
            },
            ApplyOrigin::Push,
        );
        let reader = Reader::new(Arc::clone(&epoch), dir, ModeKeys::default());
        Fx {
            reader,
            _writer: writer,
            epoch,
        }
    }

    pub(crate) fn said(fx: &ReaderEffects) -> Vec<String> {
        fx.iter()
            .filter_map(|e| match e {
                ReaderEffect::Speak(s) => Some(
                    s.utterance
                        .segments
                        .iter()
                        .map(|g| &*g.text)
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
                _ => None,
            })
            .collect()
    }

    pub(crate) fn prios(fx: &ReaderEffects) -> Vec<Prio> {
        fx.iter()
            .filter_map(|e| match e {
                ReaderEffect::Speak(s) => Some(s.utterance.prio),
                _ => None,
            })
            .collect()
    }

    fn activate(f: &mut Fx, window: u64, now: Instant) -> ReaderEffects {
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
                window: key(window),
            }),
            now,
        )
    }

    fn focus(f: &mut Fx, n: u64, now: Instant) -> ReaderEffects {
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged { node: key(n) }),
            now,
        )
    }

    fn dialog_specs() -> Vec<Spec> {
        vec![
            node(1, Role::Dialog, "Delete file", &[2, 3]),
            node(2, Role::Label, "This cannot be undone.", &[]),
            node(3, Role::Button, "Delete", &[]),
        ]
    }

    #[test]
    fn a_dialog_opening_speaks_title_text_and_focus_as_one_utterance() {
        let mut f = fixture(&dialog_specs());
        let t = Instant::now();
        let fx = activate(&mut f, 1, t);
        assert!(said(&fx).is_empty(), "the title waits for the focus");
        assert!(
            fx.iter().any(|e| matches!(e, ReaderEffect::ArmTimer(_))),
            "and a timer covers the case that no focus follows"
        );
        let fx = focus(&mut f, 3, t + Duration::from_millis(4));
        assert_eq!(
            said(&fx),
            ["Delete file, dialog, This cannot be undone., Delete, button"]
        );
        assert_eq!(prios(&fx), [Prio::Interrupt]);
    }

    #[test]
    fn a_plain_window_speaks_its_title_before_the_focus() {
        let mut f = fixture(&[
            node(1, Role::Window, "Files", &[2]),
            node(2, Role::Button, "Back", &[]),
        ]);
        let t = Instant::now();
        activate(&mut f, 1, t);
        let fx = focus(&mut f, 2, t + Duration::from_millis(3));
        assert_eq!(said(&fx), ["Files, Back, button"]);
    }

    #[test]
    fn a_window_with_no_focus_is_spoken_alone_and_interrupts() {
        let mut f = fixture(&dialog_specs());
        let t = Instant::now();
        // The user is working in this app: its next window is the user's own context change.
        focus(&mut f, 3, t);
        activate(&mut f, 1, t + Duration::from_millis(500));
        let before = f.epoch.current();
        let fx = f.reader.handle(
            ReaderMsg::Timer,
            t + Duration::from_millis(500) + ALONE_DELAY,
        );
        assert_eq!(said(&fx), ["Delete file, dialog, This cannot be undone."]);
        assert_eq!(prios(&fx), [Prio::Message]);
        assert!(f.epoch.current() > before, "it cancels stale speech");
    }

    #[test]
    fn a_focus_before_its_activation_is_not_cut_off_by_the_title() {
        let mut f = fixture(&[
            node(1, Role::Window, "Files", &[2]),
            node(2, Role::Button, "Back", &[]),
        ]);
        let t = Instant::now();
        let fx = focus(&mut f, 2, t);
        assert_eq!(said(&fx), ["Back, button"]);
        let epoch = f.epoch.current();
        let fx = activate(&mut f, 1, t + Duration::from_millis(10));
        assert_eq!(said(&fx), ["Files"]);
        assert_eq!(f.epoch.current(), epoch, "the focus utterance stays valid");
        assert_eq!(prios(&fx), [Prio::Notification], "and the title queues");
    }

    #[test]
    fn a_focus_in_the_previous_window_of_the_app_is_not_this_activations_focus() {
        let mut f = fixture(&[
            node(1, Role::Window, "Files", &[2]),
            node(2, Role::Button, "Back", &[]),
            node(5, Role::Window, "Other", &[6]),
            node(6, Role::Button, "Go", &[]),
        ]);
        let t = Instant::now();
        focus(&mut f, 6, t);
        // 30 ms later another window of the same app activates: its title leads its own focus.
        let fx = activate(&mut f, 1, t + Duration::from_millis(30));
        assert!(said(&fx).is_empty(), "it waits for its focus");
        let fx = focus(&mut f, 2, t + Duration::from_millis(40));
        assert_eq!(said(&fx), ["Files, Back, button"]);
    }

    fn dialog_info() -> WindowInfo {
        WindowInfo {
            window: key(1),
            snap: Some(snap(&node(1, Role::Dialog, "Delete file", &[]))),
            texts: vec![Arc::from("This cannot be undone.")],
        }
    }

    #[test]
    fn a_window_the_mirror_lacks_is_read_and_the_focus_waits_for_the_answer() {
        // The window is not in the mirror; only the focused button is.
        let mut f = fixture(&[node(3, Role::Button, "Delete", &[])]);
        let t = Instant::now();
        let fx = activate(&mut f, 1, t);
        assert!(
            fx.iter().any(|e| matches!(
                e,
                ReaderEffect::Desktop(DesktopEffect::Window { window }) if *window == key(1)
            )),
            "the application is asked for the window at once"
        );
        let fx = focus(&mut f, 3, t + Duration::from_millis(2));
        assert!(
            said(&fx).is_empty(),
            "the focus waits a few ms: {:?}",
            said(&fx)
        );
        assert!(
            fx.iter()
                .any(|e| matches!(e, ReaderEffect::ArmTimer(at) if *at == t + WINDOW_HOLD)),
            "and a timer bounds the wait"
        );
        // The answer arrives in time: title, text and focus are one utterance, in reading order.
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(dialog_info())),
            t + Duration::from_millis(5),
        );
        assert_eq!(
            said(&fx),
            ["Delete file, dialog, This cannot be undone., Delete, button"]
        );
        assert_eq!(prios(&fx), [Prio::Interrupt]);
    }

    #[test]
    fn a_slow_window_read_does_not_hold_the_focus_beyond_the_bound() {
        let mut f = fixture(&[node(3, Role::Button, "Delete", &[])]);
        let t = Instant::now();
        activate(&mut f, 1, t);
        let fx = focus(&mut f, 3, t + Duration::from_millis(2));
        assert!(said(&fx).is_empty());
        // No answer by the deadline: the focus is spoken alone ...
        let fx = f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD);
        assert_eq!(said(&fx), ["Delete, button"]);
        assert_eq!(prios(&fx), [Prio::Interrupt]);
        // ... and the answer (within 150 ms) replaces the still-playing focus by the whole line
        // in reading order, so the focus is never heard twice.
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(dialog_info())),
            t + Duration::from_millis(30),
        );
        assert_eq!(
            said(&fx),
            ["Delete file, dialog, This cannot be undone., Delete, button"]
        );
        assert_eq!(prios(&fx), [Prio::Interrupt]);
        // Once: the same answer again adds nothing.
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(dialog_info())),
            t + Duration::from_millis(31),
        );
        assert!(said(&fx).is_empty(), "{:?}", said(&fx));
    }

    #[test]
    fn a_newer_focus_drops_the_held_one() {
        let mut f = fixture(&[
            node(3, Role::Button, "Delete", &[]),
            node(4, Role::Button, "Cancel", &[]),
        ]);
        let t = Instant::now();
        activate(&mut f, 1, t);
        assert!(said(&focus(&mut f, 3, t + Duration::from_millis(2))).is_empty());
        // The user moved on before the answer came (a cold focus is spoken at once).
        let fx = focus(&mut f, 4, t + Duration::from_millis(60));
        assert_eq!(said(&fx), ["Cancel, button"], "the held focus is gone");
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(dialog_info())),
            t + Duration::from_millis(70),
        );
        assert!(
            said(&fx).is_empty(),
            "nothing stale is spoken late: {:?}",
            said(&fx)
        );
    }

    #[test]
    fn an_answer_that_arrives_first_leads_the_focus_utterance() {
        let mut f = fixture(&[node(3, Role::Button, "Delete", &[])]);
        let t = Instant::now();
        activate(&mut f, 1, t);
        let info = WindowInfo {
            window: key(1),
            snap: Some(snap(&node(1, Role::Dialog, "Delete file", &[]))),
            texts: vec![Arc::from("Sure?")],
        };
        let fx = f
            .reader
            .handle(ReaderMsg::Background(BackgroundMsg::Window(info)), t);
        assert!(said(&fx).is_empty());
        let fx = focus(&mut f, 3, t + Duration::from_millis(3));
        assert_eq!(said(&fx), ["Delete file, dialog, Sure?, Delete, button"]);
    }

    #[test]
    fn a_late_answer_is_dropped_once_the_user_moved_on() {
        let mut f = fixture(&[
            node(3, Role::Button, "Delete", &[]),
            node(4, Role::Button, "Cancel", &[]),
        ]);
        let t = Instant::now();
        activate(&mut f, 1, t);
        focus(&mut f, 3, t + Duration::from_millis(2));
        focus(&mut f, 4, t + Duration::from_millis(40));
        let info = WindowInfo {
            window: key(1),
            snap: Some(snap(&node(1, Role::Dialog, "Delete file", &[]))),
            texts: vec![Arc::from("Sure?")],
        };
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(info)),
            t + Duration::from_millis(60),
        );
        assert!(said(&fx).is_empty(), "stale dialog text is worse than none");
    }

    #[test]
    fn a_duplicate_activation_is_one_activation() {
        let mut f = fixture(&dialog_specs());
        let t = Instant::now();
        let a = activate(&mut f, 1, t);
        let b = activate(&mut f, 1, t + Duration::from_millis(5));
        assert!(!a.is_empty());
        assert!(b.is_empty());
    }

    #[test]
    fn a_focus_in_another_app_does_not_take_the_titles_lead() {
        let mut f = fixture(&dialog_specs());
        let t = Instant::now();
        activate(&mut f, 1, t);
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
                node: NodeKey {
                    app: app(2),
                    local: LocalId(3),
                },
            }),
            t + Duration::from_millis(2),
        );
        assert!(f.reader.win.lead.is_none());
    }

    #[derive(Debug)]
    struct Fake(crate::builtin_ext::PolicyVerdict);

    impl crate::builtin_ext::AppPolicy for Fake {
        fn decide(
            &self,
            _f: &crate::builtin_ext::PolicyFacts<'_>,
        ) -> crate::builtin_ext::PolicyVerdict {
            self.0.clone()
        }
    }

    fn policy_fixture(v: crate::builtin_ext::PolicyVerdict, on: bool) -> Fx {
        let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
        f.reader = f.reader.with_app_policy(Arc::new(Fake(v)));
        f.reader.dir.set_policy(app(1), on);
        f
    }

    #[test]
    fn a_policy_can_silence_or_reword_a_focus_but_only_for_apps_that_ask() {
        use crate::builtin_ext::PolicyVerdict as V;
        let t = Instant::now();
        let mut f = policy_fixture(V::Silence, true);
        assert!(said(&focus(&mut f, 2, t)).is_empty());
        assert!(
            matches!(
                f.reader.focus(),
                crate::focus::FocusState::Focused {
                    presented: true,
                    ..
                }
            ),
            "the focus is still tracked"
        );
        let mut f = policy_fixture(V::Silence, false);
        assert_eq!(
            said(&focus(&mut f, 2, t)),
            ["Back, button"],
            "no profile, no policy"
        );
        let mut f = policy_fixture(V::Value("50 percent".into()), true);
        assert_eq!(said(&focus(&mut f, 2, t)), ["Back, button, 50 percent"]);
        let seg = Segment {
            text: Arc::from("Custom"),
            kind: SegmentKind::Text,
        };
        let mut f = policy_fixture(V::Say(vec![seg]), true);
        assert_eq!(said(&focus(&mut f, 2, t)), ["Custom"]);
    }

    #[test]
    fn a_flooding_apps_activations_do_not_displace_a_quiet_apps_pending_window() {
        // The quiet app's dialog activates; the flooder (app 2) then activates many windows and
        // takes focus. The quiet app's title still leads its own focus.
        let mut f = fixture(&dialog_specs());
        let t = Instant::now();
        activate(&mut f, 1, t);
        for w in 0..30u64 {
            f.reader.handle(
                ReaderMsg::Focus(FocusLaneMsg::WindowActivated {
                    window: NodeKey {
                        app: app(2),
                        local: LocalId(500 + w),
                    },
                }),
                t + Duration::from_millis(1 + w),
            );
        }
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::FocusChanged {
                node: NodeKey {
                    app: app(2),
                    local: LocalId(9),
                },
            }),
            t + Duration::from_millis(40),
        );
        // The flooder's focus did not consume or cancel the quiet app's activation.
        let fx = focus(&mut f, 3, t + Duration::from_millis(60));
        assert_eq!(
            said(&fx),
            ["Delete file, dialog, This cannot be undone., Delete, button"]
        );
    }

    #[derive(Debug)]
    struct QuietStage;

    impl crate::builtin_ext::AppPolicy for QuietStage {
        fn decide(
            &self,
            f: &crate::builtin_ext::PolicyFacts<'_>,
        ) -> crate::builtin_ext::PolicyVerdict {
            use crate::builtin_ext::{PolicyMoment, PolicyVerdict};
            if f.moment == PolicyMoment::Activate && f.role == Role::Window {
                PolicyVerdict::Silence
            } else {
                PolicyVerdict::Default
            }
        }
    }

    #[test]
    fn a_policy_can_keep_a_windows_activation_unannounced() {
        let t = Instant::now();
        let mut f = fixture(&[
            node(1, Role::Window, "Main stage", &[2]),
            node(2, Role::Button, "Back", &[]),
        ]);
        f.reader = f.reader.with_app_policy(Arc::new(QuietStage));
        f.reader.dir.set_policy(app(1), true);
        activate(&mut f, 1, t);
        let fx = focus(&mut f, 2, t + Duration::from_millis(3));
        assert_eq!(said(&fx), ["Back, button"], "no stage title");
        // Alone, after its time, it is silent too.
        activate(&mut f, 1, t + Duration::from_secs(2));
        let fx = f
            .reader
            .handle(ReaderMsg::Timer, t + Duration::from_secs(3));
        assert!(said(&fx).is_empty());
    }

    // ---- H-1, L-2: fairness regression tests (reviewer probes P1 to P3) ----

    fn other(n: u64) -> NodeKey {
        NodeKey {
            app: app(2),
            local: gorca_model::LocalId(n),
        }
    }

    fn all_said(v: &[ReaderEffects]) -> Vec<String> {
        v.iter().flat_map(said).collect()
    }

    fn win_activated(f: &mut Fx, window: NodeKey, now: Instant) -> ReaderEffects {
        f.reader.handle(
            ReaderMsg::Focus(FocusLaneMsg::WindowActivated { window }),
            now,
        )
    }

    #[test]
    fn a_background_activation_that_says_nothing_never_touches_the_epoch() {
        // P1: another app activates a window it never answers for.
        let mut f = fixture(&[
            node(1, Role::Window, "Files", &[2]),
            node(2, Role::Button, "Back", &[]),
        ]);
        let t = Instant::now();
        assert_eq!(said(&focus(&mut f, 2, t)), ["Back, button"]);
        let e0 = f.epoch.current();
        let fx1 = win_activated(&mut f, other(500), t + Duration::from_millis(5));
        let fx2 = f
            .reader
            .handle(ReaderMsg::Timer, t + Duration::from_millis(5) + ALONE_DELAY);
        assert!(all_said(&[fx1, fx2]).is_empty());
        assert_eq!(f.epoch.current(), e0, "the user's speech is not cancelled");
    }

    #[test]
    fn a_silent_window_of_the_focused_app_does_not_advance_the_epoch_either() {
        // The window is unknown: nothing to say, so nothing may be cancelled.
        let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
        let t = Instant::now();
        focus(&mut f, 2, t);
        let e0 = f.epoch.current();
        activate(&mut f, 77, t + Duration::from_millis(5));
        f.reader
            .handle(ReaderMsg::Timer, t + Duration::from_millis(5) + ALONE_DELAY);
        assert_eq!(f.epoch.current(), e0);
    }

    #[test]
    fn a_background_windows_title_is_a_polite_notice_not_a_context_change() {
        let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
        let t = Instant::now();
        focus(&mut f, 2, t);
        let e0 = f.epoch.current();
        let at = t + Duration::from_millis(5);
        win_activated(&mut f, other(500), at);
        let fx = f.reader.handle(
            ReaderMsg::Background(BackgroundMsg::Window(WindowInfo {
                window: other(500),
                snap: Some(Arc::new(NodeSnapshot {
                    key: other(500),
                    ..(*snap(&node(500, Role::Window, "Chat", &[]))).clone()
                })),
                texts: Vec::new(),
            })),
            at + Duration::from_millis(3),
        );
        let _ = fx;
        let fx = f.reader.handle(ReaderMsg::Timer, at + ALONE_DELAY);
        assert_eq!(said(&fx), ["Chat"]);
        assert_eq!(prios(&fx), [Prio::Notification]);
        assert_eq!(f.epoch.current(), e0);
    }

    #[test]
    fn a_held_focus_survives_another_apps_alone_timer() {
        // P2: app 2's alone timer is due inside app 1's hold; the user's focus is spoken.
        let mut f = fixture(&[node(3, Role::Button, "Delete", &[])]);
        let base = Instant::now();
        let t = base + Duration::from_millis(115);
        let mut out = vec![win_activated(&mut f, other(500), base)];
        out.push(win_activated(&mut f, key(1), t));
        let fx = focus(&mut f, 3, t + Duration::from_millis(2));
        assert!(said(&fx).is_empty(), "held: {:?}", said(&fx));
        out.push(fx);
        out.push(
            f.reader
                .handle(ReaderMsg::Timer, t + Duration::from_millis(6)),
        );
        out.push(f.reader.handle(ReaderMsg::Timer, t + WINDOW_HOLD));
        let spoken = all_said(&out);
        assert!(
            spoken.iter().any(|s| s.contains("Delete, button")),
            "the user's focus was never spoken: {spoken:?}"
        );
    }

    #[test]
    fn the_last_alt_tab_title_is_spoken_even_over_the_activation_budget() {
        // P3, 9 switches 125 ms apart between two windows of one process.
        let mut f = fixture(&[
            node(1, Role::Window, "Files", &[2]),
            node(2, Role::Button, "Back", &[]),
            node(5, Role::Window, "Other", &[6]),
            node(6, Role::Button, "Go", &[]),
        ]);
        let t = Instant::now();
        let mut all: Vec<Vec<String>> = Vec::new();
        for i in 0..9u64 {
            let (w, n) = if i % 2 == 0 { (1, 2) } else { (5, 6) };
            let at = t + Duration::from_millis(125 * i);
            activate(&mut f, w, at);
            all.push(said(&focus(&mut f, n, at + Duration::from_millis(3))));
        }
        let last = all.last().cloned().unwrap_or_default();
        assert_eq!(last, ["Files, Back, button"], "{all:?}");
        assert!(
            all.iter()
                .all(|s| s[0].starts_with("Files") || s[0].starts_with("Other")),
            "titles dropped: {all:?}"
        );
    }

    #[test]
    fn many_apps_activating_windows_never_advance_the_epoch_or_exceed_the_speech_ceiling() {
        let mut f = fixture(&[node(2, Role::Button, "Back", &[])]);
        let t = Instant::now();
        focus(&mut f, 2, t);
        let e0 = f.epoch.current();
        let mut spoken = 0;
        for a in 2..60u32 {
            let w = NodeKey {
                app: app(a),
                local: gorca_model::LocalId(1),
            };
            win_activated(&mut f, w, t + Duration::from_millis(10));
            f.reader.handle(
                ReaderMsg::Background(BackgroundMsg::Window(WindowInfo {
                    window: w,
                    snap: Some(Arc::new(NodeSnapshot {
                        key: w,
                        ..(*snap(&node(1, Role::Window, "Spam", &[]))).clone()
                    })),
                    texts: Vec::new(),
                })),
                t + Duration::from_millis(12),
            );
        }
        for ms in (130..1000).step_by(10) {
            spoken += said(
                &f.reader
                    .handle(ReaderMsg::Timer, t + Duration::from_millis(ms)),
            )
            .len();
        }
        assert_eq!(f.epoch.current(), e0);
        assert!(spoken <= 20, "{spoken} window notices in one second");
    }
}

#[cfg(test)]
mod m4b_rereview_probes;

#[cfg(test)]
mod m4b_review_probes;
