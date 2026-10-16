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

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use gorca_model::{AppId, DocId, DocNodeId, NodeKey};
use smallvec::SmallVec;

use crate::doc_actor::{DocDirectory, WebJoin};
use crate::mode::WebCursor;

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
            ModeWord::Browse => "browse mode",
            ModeWord::Focus => "focus mode",
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

/// Everything the Reader keeps for browse mode.
#[derive(Debug)]
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
    pub settings: BrowseSettings,
    pub counters: BrowseCounters,
    /// Offset of the cursor in the flat text at the last move, to restore after an edit.
    pub last_off: u32,
    /// Latest scroll request: sent after a pause so held keys do not flood the page (LP-6).
    pub scroll_due: Option<(Instant, DocId, DocNodeId)>,
    /// When the browser last reported `load-complete`, by document root (page auto-read): the
    /// load of another tab or of an iframe of the same browser is not this page's.
    pub loaded: HashMap<gorca_model::NodeKey, Instant>,
    /// A freshly published document that reads itself once its page has loaded.
    pub auto_read: Option<sayall::AutoRead>,
    /// The field in focus mode has a popup open (a drop-down, an auto-complete list): Escape
    /// belongs to the page, which closes it; the next Escape leaves focus mode (modes.md 2).
    pub popup_open: bool,
    /// The web dialog (`dialog`, `alertdialog`) the last focus was in, and the name of one that
    /// focus has just entered, for the next focus utterance. A browser that sends no window
    /// event for a page's dialog (Chromium) is announced from the mirror, as NVDA announces a
    /// context change.
    pub focus_dialog: Option<DocNodeId>,
    pub dialog_lead: Option<(Arc<str>, Instant)>,
}

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
            settings: BrowseSettings::default(),
            counters: BrowseCounters::default(),
            last_off: 0,
            scroll_due: None,
            loaded: HashMap::new(),
            auto_read: None,
            popup_open: false,
            focus_dialog: None,
            dialog_lead: None,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod latency;
