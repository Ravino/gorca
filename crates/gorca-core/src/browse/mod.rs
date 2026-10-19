// SPDX-License-Identifier: GPL-3.0-or-later
//! Browse mode (ADR-0003 section 11, browse-mode design section 7): the Reader's virtual cursor
//! over a web document mirror and the browse/focus mode machine.
//!
//! * [`WebMode`] is a pure state machine ([`step`]): an enum, an exhaustive `match`, unit-tested
//!   before it is wired (BI-4). It never reads a mirror and never performs IO; it returns
//!   [`WebFx`] effects the Reader interprets.
//! * [`forms`] is the pass-through predicate (when focus or activation switches to focus mode).
//! * [`cursor`] and [`present`] are pure functions of a published `WebDoc`: cursor moves through
//!   `gorca_text` and the line presenter. No browse key waits for IPC (BI-1).
//! * [`commands`] is the Reader arm: key commands, document notices, focus handling.

pub mod commands;
pub mod cursor;
pub mod forms;
pub mod present;
pub mod sayall;
pub mod selmirror;
pub mod tablenav;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gorca_model::{AppId, DocId, DocNodeId, NodeKey};
use smallvec::SmallVec;

use crate::doc_actor::{DocDirectory, WebJoin};
use crate::mode::WebCursor;

/// Live-region utterances admitted per second, over every document of the reader.
pub const LIVE_RATE: f32 = 5.0;
/// Burst of the live-region admission bucket.
pub const LIVE_BURST: f32 = 6.0;

/// What a cursor key may roam over (M7b: modal dialogs).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Scope {
    Document,
    Modal(DocNodeId),
}

/// Why the user is in focus mode.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FocusCause {
    /// Focus landed on an editable control: Escape returns to browse mode.
    Auto,
    /// The user toggled it: only the toggle leaves.
    Manual,
    /// `role=application`: only the toggle leaves, Escape reaches the page.
    Application,
}

/// See [`Browse::grace_fallback`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct GraceFallback {
    /// The focus that waited for the mirror.
    pub focus: NodeKey,
    pub doc: DocId,
    /// Where the fallback put the cursor (the document root).
    pub root: DocNodeId,
    /// When the fallback started: a rejoin later than [`GRACE_REJOIN_WINDOW`] is not wanted.
    pub at: Instant,
}

/// How long after the loading grace a repair may still move the cursor to the focus (review F-7).
pub const GRACE_REJOIN_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// The browse/focus state machine (ADR-0003 `WebMode`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WebMode {
    Inactive,
    /// Focus is in a web application but the document is not mirrored yet.
    Loading {
        doc: Option<DocId>,
        since: Instant,
        focus: NodeKey,
        /// The focus is an editable control: focus mode at once, no waiting.
        editable: bool,
        /// `editable` was read from the focus snapshot. A cold focus (no snapshot yet) might be
        /// a field: until it is known the grabs stay off and the grace timer must not flip to
        /// browse mode (H2: typing would be swallowed).
        known: bool,
    },
    Browse {
        doc: DocId,
        cur: WebCursor,
        scope: Scope,
        sticky: bool,
    },
    Focus {
        doc: DocId,
        node: DocNodeId,
        cause: FocusCause,
        resume: WebCursor,
        scope: Scope,
    },
}

/// The spoken mode indication (BM-5). Until earcons exist it is a word.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ModeWord {
    Browse,
    Focus,
}

impl ModeWord {
    pub fn words(self) -> &'static str {
        match self {
            ModeWord::Browse => gorca_i18n::t(gorca_i18n::MsgId::WebModeBrowse),
            ModeWord::Focus => gorca_i18n::t(gorca_i18n::MsgId::WebModeFocus),
        }
    }
}

impl WebMode {
    /// The document this mode is in, once it is known.
    pub fn doc(self) -> Option<DocId> {
        match self {
            WebMode::Inactive => None,
            WebMode::Loading { doc, .. } => doc,
            WebMode::Browse { doc, .. } | WebMode::Focus { doc, .. } => Some(doc),
        }
    }

    /// What the user hears named: browse, focus or nothing.
    fn kind(self) -> Option<ModeWord> {
        match self {
            WebMode::Inactive => None,
            WebMode::Loading { editable, .. } => editable.then_some(ModeWord::Focus),
            WebMode::Browse { .. } => Some(ModeWord::Browse),
            WebMode::Focus { .. } => Some(ModeWord::Focus),
        }
    }

    /// Escape is grabbed only here (modes.md section 2).
    pub fn escape_grabbed(self) -> bool {
        matches!(
            self,
            WebMode::Focus {
                cause: FocusCause::Auto,
                ..
            }
        )
    }

    pub fn is_active(self) -> bool {
        !matches!(self, WebMode::Inactive)
    }
}

/// Inputs of the machine.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WebEv {
    /// Focus landed on a mirrored node of a built document.
    FocusIn {
        doc: DocId,
        node: DocNodeId,
        /// `P(node, Focus)`.
        passes: bool,
        application: bool,
    },
    /// Focus is in a web application and the join missed: the document is not mirrored yet.
    FocusPending {
        focus: NodeKey,
        editable: bool,
        /// See [`WebMode::Loading::known`].
        known: bool,
        now: Instant,
    },
    /// Focus left web content (browser chrome, another application).
    FocusAway,
    /// The document was published. `focus` is the node focus was on when we started loading.
    FirstUseful {
        doc: DocId,
        focus: Option<(DocNodeId, bool)>,
        top: DocNodeId,
    },
    NoDocument {
        focus: NodeKey,
    },
    /// `browse.toggle`.
    Toggle {
        focusable: Option<DocNodeId>,
        root: DocNodeId,
    },
    /// `browse.activate` on the node under the cursor.
    Activate {
        node: DocNodeId,
        passes: bool,
    },
    Escape,
    /// The node focus mode was on is gone.
    FocusRemoved {
        fallback: WebCursor,
    },
    DocGone {
        doc: DocId,
    },
}

/// Effects the Reader interprets.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WebFx {
    /// Speak this mode word with the next utterance.
    Announce(ModeWord),
    /// Find or build the document around this node.
    Ensure(NodeKey),
    /// Give the page's focus to this node (`Action::Focus`).
    FocusNode(DocId, DocNodeId),
}

pub type WebFxs = SmallVec<[WebFx; 2]>;

fn at(node: DocNodeId) -> WebCursor {
    WebCursor { node, local: 0 }
}

/// The pure transition function (browse-mode design 7.3).
pub fn step(mode: WebMode, ev: WebEv) -> (WebMode, WebFxs) {
    let mut fx = WebFxs::new();
    let next = next(mode, ev, &mut fx);
    if mode.kind() != next.kind()
        && let Some(w) = next.kind()
    {
        fx.insert(0, WebFx::Announce(w));
    }
    (next, fx)
}

fn next(mode: WebMode, ev: WebEv, fx: &mut WebFxs) -> WebMode {
    match ev {
        WebEv::FocusIn {
            doc,
            node,
            passes,
            application,
        } => match mode {
            WebMode::Focus {
                doc: d,
                cause: FocusCause::Manual,
                resume,
                scope,
                ..
            } if d == doc => WebMode::Focus {
                doc,
                node,
                cause: FocusCause::Manual,
                resume,
                scope,
            },
            WebMode::Focus {
                doc: d,
                cause: FocusCause::Application,
                resume,
                scope,
                ..
            } if d == doc => WebMode::Focus {
                doc,
                node,
                cause: FocusCause::Application,
                resume,
                scope,
            },
            WebMode::Inactive
            | WebMode::Loading { .. }
            | WebMode::Browse { .. }
            | WebMode::Focus { .. } => {
                let scope = match mode {
                    WebMode::Browse { doc: d, scope, .. }
                    | WebMode::Focus { doc: d, scope, .. }
                        if d == doc =>
                    {
                        scope
                    }
                    WebMode::Inactive
                    | WebMode::Loading { .. }
                    | WebMode::Browse { .. }
                    | WebMode::Focus { .. } => Scope::Document,
                };
                if application {
                    WebMode::Focus {
                        doc,
                        node,
                        cause: FocusCause::Application,
                        resume: at(node),
                        scope,
                    }
                } else if passes {
                    WebMode::Focus {
                        doc,
                        node,
                        cause: FocusCause::Auto,
                        resume: at(node),
                        scope,
                    }
                } else {
                    WebMode::Browse {
                        doc,
                        cur: at(node),
                        scope,
                        sticky: false,
                    }
                }
            }
        },
        WebEv::FocusPending {
            focus,
            editable,
            known,
            now,
        } => {
            // Already waiting for this very node: a better reading of it still counts.
            if let WebMode::Loading {
                focus: f,
                doc,
                since,
                editable: e,
                known: k,
            } = mode
                && f == focus
            {
                return if known && !k {
                    WebMode::Loading {
                        doc,
                        since,
                        focus: f,
                        editable,
                        known,
                    }
                } else {
                    let _ = e;
                    mode
                };
            }
            fx.push(WebFx::Ensure(focus));
            WebMode::Loading {
                doc: None,
                since: now,
                focus,
                editable,
                known,
            }
        }
        WebEv::FocusAway => WebMode::Inactive,
        WebEv::FirstUseful { doc, focus, top } => match mode {
            WebMode::Loading { .. } => match focus {
                Some((n, true)) => WebMode::Focus {
                    doc,
                    node: n,
                    cause: FocusCause::Auto,
                    resume: at(n),
                    scope: Scope::Document,
                },
                Some((n, false)) => WebMode::Browse {
                    doc,
                    cur: at(n),
                    scope: Scope::Document,
                    sticky: false,
                },
                None => WebMode::Browse {
                    doc,
                    cur: at(top),
                    scope: Scope::Document,
                    sticky: false,
                },
            },
            WebMode::Inactive | WebMode::Browse { .. } | WebMode::Focus { .. } => mode,
        },
        WebEv::NoDocument { focus } => match mode {
            WebMode::Loading { focus: f, .. } if f == focus => WebMode::Inactive,
            WebMode::Inactive
            | WebMode::Loading { .. }
            | WebMode::Browse { .. }
            | WebMode::Focus { .. } => mode,
        },
        WebEv::Toggle { focusable, root } => match mode {
            WebMode::Browse {
                doc, cur, scope, ..
            } => {
                let node = focusable.unwrap_or(root);
                if let Some(f) = focusable {
                    fx.push(WebFx::FocusNode(doc, f));
                }
                WebMode::Focus {
                    doc,
                    node,
                    cause: FocusCause::Manual,
                    resume: cur,
                    scope,
                }
            }
            WebMode::Focus {
                doc,
                node,
                resume,
                scope,
                ..
            } => WebMode::Browse {
                doc,
                cur: if resume.node == node {
                    at(node)
                } else {
                    resume
                },
                scope,
                sticky: false,
            },
            WebMode::Inactive | WebMode::Loading { .. } => mode,
        },
        WebEv::Activate { node, passes } => match mode {
            WebMode::Browse {
                doc, cur, scope, ..
            } if passes => {
                fx.push(WebFx::FocusNode(doc, node));
                WebMode::Focus {
                    doc,
                    node,
                    cause: FocusCause::Auto,
                    resume: cur,
                    scope,
                }
            }
            WebMode::Inactive
            | WebMode::Loading { .. }
            | WebMode::Browse { .. }
            | WebMode::Focus { .. } => mode,
        },
        WebEv::Escape => match mode {
            WebMode::Focus {
                doc,
                node,
                cause: FocusCause::Auto,
                scope,
                ..
            } => WebMode::Browse {
                doc,
                cur: at(node),
                scope,
                sticky: false,
            },
            WebMode::Inactive
            | WebMode::Loading { .. }
            | WebMode::Browse { .. }
            | WebMode::Focus { .. } => mode,
        },
        WebEv::FocusRemoved { fallback } => match mode {
            WebMode::Focus { doc, scope, .. } => WebMode::Browse {
                doc,
                cur: fallback,
                scope,
                sticky: false,
            },
            WebMode::Inactive | WebMode::Loading { .. } | WebMode::Browse { .. } => mode,
        },
        WebEv::DocGone { doc } => {
            if mode.doc() == Some(doc) {
                WebMode::Inactive
            } else {
                mode
            }
        }
    }
}

/// Browse state a dormant document keeps (browse design 7.1 `saved`).
#[derive(Copy, Clone, Debug)]
pub struct SavedWeb {
    pub cur: WebCursor,
    pub off: u32,
}

/// Counters (M-13 and the browse metrics).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowseCounters {
    pub join_hits: u64,
    pub join_misses: u64,
    pub moves: u64,
    pub edges: u64,
    pub still_loading: u64,
    /// Live-region messages spoken, dropped by the admission bucket, and muted (not in front).
    pub live_spoken: u64,
    pub live_dropped: u64,
    pub live_muted: u64,
    /// Focus resolutions restarted by a key press in a web application that was not browsing.
    pub key_resyncs: u64,
}

/// Settings the browse arm reads (NVDA defaults).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrowseSettings {
    /// Soft line length (`browse.max_line_length`, NVDA 100).
    pub max_line: u16,
    /// Lines per page key (NVDA 25).
    pub lines_per_page: u32,
}

impl Default for BrowseSettings {
    fn default() -> Self {
        Self {
            max_line: 100,
            lines_per_page: cursor::LINES_PER_PAGE,
        }
    }
}

/// See [`BrowseState::activated_node`].
#[derive(Copy, Clone, Debug)]
pub struct ActivatedNode {
    pub cursor: gorca_model::DocNodeId,
    pub target: gorca_model::DocNodeId,
    pub role: gorca_model::Role,
    /// Its pressed, checked and expanded states when we clicked it.
    pub toggles: gorca_model::StateSet,
    pub at: Instant,
}

/// What last put the browse cursor where it is.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum CursorFrom {
    /// A reading or navigation command of ours.
    Own,
    /// A focus change onto a control.
    Focus,
}

/// Everything the Reader keeps for browse mode.
#[derive(Debug)]
#[allow(clippy::struct_excessive_bools)] // independent one-shot flags of the web layer
pub struct BrowseState {
    pub mode: WebMode,
    pub docs: Arc<DocDirectory>,
    pub join: Arc<WebJoin>,
    /// Applications whose registration said they can host web documents.
    pub web_apps: HashSet<AppId>,
    pub saved: HashMap<DocId, SavedWeb>,
    /// The document that last held focus (DR-3): awake; others sleep.
    pub active: Option<DocId>,
    /// A mode word waiting for the next utterance (BM-5).
    pub pending_word: Option<(ModeWord, Instant)>,
    /// The line at the cursor, to follow the focus utterance of a document root (W3, load).
    pub pending_tail: Option<(Vec<present::Item>, Instant)>,
    /// Focus landed on a document root whose mirror had no line yet: the first `Changed` that
    /// brings one speaks it, unless a key or another focus came first (M4).
    pub entry_wait: Option<(DocId, Instant)>,
    /// The last focus resolution restarted by a key press.
    pub resync_at: Option<Instant>,
    pub settings: BrowseSettings,
    pub counters: BrowseCounters,
    /// Offset of the cursor in the flat text at the last move, to restore after an edit.
    pub last_off: u32,
    /// The cursor's node vanished: until this instant (and while the offset is unchanged) a
    /// re-render re-resolves the cursor at the saved offset.
    /// Bare quick-navigation keys are grabbed (gorca+Shift+Space turns them off).
    pub single_letter: bool,
    pub restore_until: Option<(Instant, u32)>,
    /// Latest scroll request: sent after a pause so held keys do not flood the page (LP-6).
    pub scroll_due: Option<(Instant, DocId, DocNodeId, u32)>,
    /// When the browser last reported `load-complete`, by document root (page auto-read): the
    /// load of another tab or of an iframe of the same browser is not this page's.
    pub loaded: HashMap<gorca_model::NodeKey, Instant>,
    /// A freshly published document that reads itself once its page has loaded.
    pub auto_read: Option<sayall::AutoRead>,
    /// The field in focus mode has a popup open (a drop-down, an auto-complete list): Escape
    /// belongs to the page, which closes it; the next Escape leaves focus mode (modes.md 2).
    pub popup_open: bool,
    /// Live-region speech the Reader admits: a token bucket over all documents (fairness: a
    /// flooding page cannot make the reader talk without end, whatever its documents send).
    pub live_tokens: (f32, Option<Instant>),
    /// The focus event of this node is the browser re-focusing the document we already browse
    /// (a same-page link, a click): nothing is spoken for it and the cursor stays.
    pub suppress_focus: Option<NodeKey>,
    /// A nested document took the focus in browse mode and its speech was swallowed on the
    /// assumption that the focus moves on into the frame (Chromium); when nothing newer
    /// happened by the deadline, the stop was real (Firefox Tab onto a frame) and the frame is
    /// announced after all.
    pub frame_focus: Option<(NodeKey, Instant)>,
    /// When we last clicked a link or button for the user (`DoDefault`): a caret event within
    /// [`CARET_FOLLOW`] of it is the page answering our action (anchor jumps), not the user.
    pub activated: Option<Instant>,
    /// The check box, radio button or toggle that browse mode just clicked.
    /// The dialog whose name was last announced on focus entry.
    pub lead_dialog: Option<DocNodeId>,
    /// The virtual selection `(anchor, active)` in flat offsets (D-3).
    pub sel: Option<(u32, u32)>,
    /// Which Tab directions the last built grab set offered (next, previous): D-2 grabs Tab
    /// only while the mirror has a focusable target in that direction. Written by
    /// `current_grabs`, the one place every pushed set is built, so it cannot go stale (R-2).
    pub tab_grab: std::cell::Cell<(bool, bool)>,
    /// The web node that has system focus, and whether the browse cursor was last put there by a
    /// focus change (NVDA `_lastCaretMoveWasFocus`, #5227). While the cursor is on or inside the
    /// focused node, or came from a focus change, Tab belongs to the browser (R-5).
    pub focus_node: Option<(DocId, DocNodeId)>,
    pub cursor_from: CursorFrom,
    /// Where position restore put the cursor after the node the user was on vanished: the
    /// next Enter on exactly this node announces the new item instead of clicking it.
    pub displaced: Option<gorca_model::DocNodeId>,
    /// The loading grace ran out with the focus still unmirrored and browse mode started at the
    /// top of the page (7.3). When a repair or an update later mirrors the focused node and
    /// neither the focus nor the cursor moved, the focus is joined after all (CHR-0 B-2).
    pub grace_fallback: Option<GraceFallback>,
    /// The Reader just said what a refused click's object says now: the refresh that follows is
    /// not a second displacement (the next Enter acts on the name that was spoken).
    pub announced_change: bool,
    /// The node we last clicked: the cursor node it was clicked from, the control itself, its
    /// role, and when. Only a relabel of exactly this control right after the click is the
    /// click's answer and not a swap.
    pub activated_node: Option<ActivatedNode>,
    /// Content signature of the node under the cursor when the user last left it there: a
    /// change under an unmoved cursor (the page re-used the node for another item) displaces it.
    pub cursor_sig: Option<(gorca_model::DocNodeId, u64)>,
    /// Which selection Ctrl+C copies (gorca+Shift+F10, ADR-0021): `Native` mirrors the virtual
    /// selection into the page, `Virtual` leaves the page alone. `Native` is the default.
    pub model: SelectionModel,
    /// Where the page selection stands (one request in flight at most).
    pub sync: NativeSync,
    /// Counter of selection requests; a result for an older one is ignored.
    pub sel_req: u32,
    /// A "page selection not set / incomplete" notice was spoken for the current selection:
    /// once per burst, not once per Shift+arrow (ADR-0021).
    pub sel_notified: bool,
    /// When the page selection is next brought up to date with `sel` (debounced).
    pub sel_due: Option<Instant>,
    /// The text node whose page selection we set last: it is cleared when the virtual range
    /// moves elsewhere or goes away, so the page never keeps a stale selection.
    pub native_sel_node: Option<(DocId, DocNodeId)>,
    pub click_target: Option<(DocNodeId, Instant)>,
}

/// Which selection Ctrl+C copies in browse mode (ADR-0021).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SelectionModel {
    /// The page's own selection follows the virtual one; the browser copies.
    Native,
    /// The virtual selection stays gorca's; the page is never written.
    Virtual,
}

/// The state of the request that sets (or probes) the page selection.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum NativeSync {
    Idle,
    /// A selection was sent; its result decides whether "page selection not set" is said.
    Pending {
        req: u32,
    },
    /// The switch to `Native` was asked and the document is being probed for support.
    Probing {
        req: u32,
    },
}

/// How long polite live-region news may wait in the speech queue (ADR-0003 13.2).
pub const LIVE_TTL_POLITE: u8 = 5;
/// How long assertive live-region news may wait in the speech queue (ADR-0003 13.2).
pub const LIVE_TTL_ASSERTIVE: u8 = 10;

/// How long after our own activation a caret event moves the browse cursor.
pub const CARET_FOLLOW: std::time::Duration = std::time::Duration::from_millis(700);

impl BrowseState {
    pub fn new(docs: Arc<DocDirectory>, join: Arc<WebJoin>) -> Self {
        Self {
            mode: WebMode::Inactive,
            docs,
            join,
            web_apps: HashSet::new(),
            saved: HashMap::new(),
            active: None,
            pending_word: None,
            pending_tail: None,
            entry_wait: None,
            resync_at: None,
            settings: BrowseSettings::default(),
            counters: BrowseCounters::default(),
            last_off: 0,
            restore_until: None,
            displaced: None,
            grace_fallback: None,
            announced_change: false,
            activated_node: None,
            cursor_sig: None,
            single_letter: true,
            scroll_due: None,
            loaded: HashMap::new(),
            auto_read: None,
            popup_open: false,
            live_tokens: (LIVE_BURST, None),
            suppress_focus: None,
            frame_focus: None,
            activated: None,
            click_target: None,
            sel: None,
            tab_grab: std::cell::Cell::new((true, true)),
            focus_node: None,
            cursor_from: CursorFrom::Own,
            model: SelectionModel::Native,
            sync: NativeSync::Idle,
            sel_req: 0,
            sel_notified: false,
            sel_due: None,
            native_sel_node: None,
            lead_dialog: None,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod latency;
