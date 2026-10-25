// SPDX-License-Identifier: GPL-3.0-or-later
//! Review cursor, object navigation and flat review (ADR-0006, milestone M4a).
//!
//! The Reader arm: [`ReviewMsg`] handling, [`ReviewState`], and the command layer in
//! [`commands`]. Everything pure lives in the sibling modules ([`objnav`], [`cluster`],
//! [`flat`], [`validity`], [`segment`], [`collect`]); everything that talks to an application
//! lives in [`build`] and runs in tasks the runtime owns. The Reader only reads the mirror and
//! the immutable line model, and sends [`crate::ReaderEffect`]s (R3).

pub mod build;
pub mod cluster;
pub mod collect;
mod commands;
pub mod cursor;
pub mod flat;
#[doc(hidden)]
pub mod gate;
#[cfg(test)]
mod latency_tests;
pub mod model;
pub mod objnav;
pub mod segment;
pub mod terminal;
pub mod validity;
pub mod zone;

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::NodeKey;
use smallvec::SmallVec;

pub use build::{BuildError, NavFetchError, ProbeItem, ReviewIo, RrVerdict};
pub use cursor::{
    BuildId, FlatPos, ModelId, NavReqId, ReviewCursor, ReviewFrom, ReviewKind, ReviewOrigin,
    ReviewPos, ZoneAnchor,
};
pub use model::{Completeness, LineModel, ReviewLimits};
pub use objnav::{NavOp, NavSource};

use crate::reader::{Reader, ReaderEffects};
use commands::Pending;

/// Messages for the review arm. Replies are matched by request id, not by epoch: a reply to a
/// request that was superseded by a newer review command still finishes that command, which then
/// speaks under the newest epoch (ADR-0006 4.3: only the last command speaks).
#[derive(Debug)]
pub enum ReviewMsg {
    /// A model build or refresh finished.
    Built {
        build: BuildId,
        result: Result<Arc<LineModel>, BuildError>,
    },
    /// A navigation fetch was applied to the mirror (or failed).
    NavFetched {
        req: NavReqId,
        result: Result<(), NavFetchError>,
    },
    /// RR-1 revalidation result for `model`.
    Revalidated { model: ModelId, verdict: RrVerdict },
    /// The status-bar `Collection` reply.
    StatusBarFound {
        req: NavReqId,
        result: Result<Option<NodeKey>, NavFetchError>,
    },
    /// An action on the navigator finished.
    ActionDone {
        req: NavReqId,
        result: Result<(), NavFetchError>,
    },
}

/// What a model build asks the app side to do.
#[derive(Clone, Debug)]
pub struct BuildRequest {
    pub build: BuildId,
    pub surface: NodeKey,
    pub prev: Option<Arc<LineModel>>,
    /// Nodes whose text lines must be read again; `None` = all of them.
    pub dirty: Option<Arc<[NodeKey]>>,
    /// The key press waits for this build (Interactive); otherwise it refreshes in the background.
    pub interactive: bool,
}

/// Counters of ADR-0006 (5.3, 4.3, 13).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ReviewCounters {
    pub objnav_visit_cap: u64,
    pub objnav_degraded: u64,
    pub review_step_timeout: u64,
    pub review_stale_spoken: u64,
    pub review_queue_overflow: u64,
    pub review_builds: u64,
    pub review_nav_gone: u64,
    pub review_surface_gone: u64,
    pub review_move_focus_unsupported: u64,
    pub review_build_failed: u64,
    pub review_superseded: u64,
    pub review_corrections: u64,
}

/// Reader-side state of the review arm.
#[derive(Debug)]
pub struct ReviewState {
    pub(crate) op: OpState,
    /// The model of the current cursor, and the one before it (RI-4: at most two).
    pub(crate) cur: Option<Arc<LineModel>>,
    pub(crate) prev: Option<Arc<LineModel>>,
    /// The mirror revision at which `cur` was last found valid.
    pub(crate) checked_rev: u64,
    pub(crate) validated_at: Option<Instant>,
    pub(crate) simple: bool,
    pub(crate) focus_follows: bool,
    pub(crate) default_kind: ReviewKind,
    pub(crate) limits: ReviewLimits,
    pub(crate) geometry_ttl: Duration,
    pub(crate) next_id: u64,
    pub(crate) bg_build: Option<(BuildId, Instant)>,
    /// Surfaces the user was told have no usable layout.
    pub(crate) told_fallback: SmallVec<[NodeKey; 4]>,
    pub(crate) counters: ReviewCounters,
    /// When the key press that started the running step (and its queue) arrived.
    pub(crate) step_started: Option<Instant>,
    /// The line last spoken from the current model, for the RR-1 correction.
    pub(crate) rr_check: Option<RrCheck>,
    /// The screen text the terminal model `cur` was built from (model id, hash): a new screen
    /// generation with the same text (a caret move) does not rebuild it.
    pub(crate) term_text: Option<(ModelId, u64)>,
    /// The line a terminal cursor is on after it scrolled off the screen, as a one-line model
    /// (ADR-0018): word and character moves go on reading it.
    pub(crate) held: Option<Arc<LineModel>>,
    /// Rows the terminal screen scrolled since `cur` was built, from its edit events.
    pub(crate) scroll: ScrollHint,
}

/// How far the terminal screen scrolled up since the current model was built, counted from the
/// edit events (VTE reports the rows that leave the top as a delete at offset 0, so the count is
/// exact). `exact` drops when an event was lost, the screen was replaced as a whole or the
/// tracking changed: the scroll is then estimated from the content (ADR-0018).
#[derive(Copy, Clone, Debug)]
pub(crate) struct ScrollHint {
    pub rows: u32,
    pub exact: bool,
    /// The tracking (`generation >> 32`) the count belongs to.
    pub serial: u64,
}

impl ScrollHint {
    pub(crate) fn fresh(serial: u64) -> Self {
        Self {
            rows: 0,
            exact: true,
            serial,
        }
    }

    /// The exact scroll for a model of tracking `built_rev`, if it is known.
    pub(crate) fn rows_for(&self, built_rev: u64) -> Option<u32> {
        (self.exact && self.serial == built_rev >> 32).then_some(self.rows)
    }
}

/// The line a revalidation may have to correct (ADR-0006 RR-1, F-4).
#[derive(Clone, Debug)]
pub(crate) struct RrCheck {
    pub model: ModelId,
    pub anchor: ZoneAnchor,
    pub text: Arc<str>,
    pub epoch: gorca_model::Epoch,
}

/// ADR-0006 4.3.
#[derive(Debug)]
pub(crate) enum OpState {
    Idle,
    Pending(Box<Pending>),
}

/// `review.geometry_ttl` default (ADR-0006 6.6).
pub const GEOMETRY_TTL: Duration = Duration::from_secs(2);
/// Deadline of an Interactive review operation (ADR-0006 4.3).
pub const INTERACTIVE: Duration = Duration::from_millis(120);

impl ReviewState {
    pub fn new() -> Self {
        Self {
            op: OpState::Idle,
            cur: None,
            prev: None,
            checked_rev: 0,
            validated_at: None,
            simple: true,
            focus_follows: true,
            default_kind: ReviewKind::Screen,
            limits: ReviewLimits::default(),
            geometry_ttl: GEOMETRY_TTL,
            next_id: 0,
            bg_build: None,
            told_fallback: SmallVec::new(),
            counters: ReviewCounters::default(),
            step_started: None,
            rr_check: None,
            term_text: None,
            held: None,
            scroll: ScrollHint::fresh(0),
        }
    }

    pub(crate) fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// Drop every cached model (the app went away).
    pub(crate) fn forget_app(&mut self, app: gorca_model::AppId) {
        if self.cur.as_ref().is_some_and(|m| m.app == app) {
            self.cur = None;
        }
        if self.prev.as_ref().is_some_and(|m| m.app == app) {
            self.prev = None;
        }
    }

    /// Models held (RI-4 counter).
    pub fn models_held(&self) -> usize {
        usize::from(self.cur.is_some()) + usize::from(self.prev.is_some())
    }

    /// Bytes of the held models.
    pub fn model_bytes(&self) -> usize {
        self.cur.as_ref().map_or(0, |m| m.bytes()) + self.prev.as_ref().map_or(0, |m| m.bytes())
    }
}

impl Default for ReviewState {
    fn default() -> Self {
        Self::new()
    }
}

impl Reader {
    pub(crate) fn on_review(&mut self, m: ReviewMsg, fx: &mut ReaderEffects) {
        self.review_message(m, fx);
    }

    pub fn review_counters(&self) -> ReviewCounters {
        self.review.counters
    }

    pub fn review_state(&self) -> &ReviewState {
        &self.review
    }
}

#[cfg(test)]
mod build_tests;
#[cfg(test)]
mod tests;
