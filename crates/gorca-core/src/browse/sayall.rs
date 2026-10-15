//! Say-all in browse mode (browse design 9, ADR-0007 section 13): the page is read from the
//! virtual cursor in sentence chunks by the same engine as a native text field
//! ([`crate::text::say_all`]), with the document's flat text as the source. Nothing here waits
//! for IPC: the text is the Reader's own published `WebDoc`, a chunk is spoken as the line
//! presenter would speak it (objects by role and name), and the virtual cursor follows the
//! chunk the server is speaking, so any key that stops the reading acts from where it is.
//!
//! Page auto-read on load (Q-W2, NVDA `autoSayAllOnPageLoad`): after a page has loaded and its
//! title was spoken, the reading starts by itself. Two events decide it, in either order: the
//! document's first publish (`FirstUseful`) and the browser's `load-complete` (`Loaded`).

use std::time::{Duration, Instant};

use gorca_model::{DocId, Epoch, Prio, UttId, Utterance};

use super::WebMode;
use super::commands::SCROLL_DEBOUNCE;
use super::cursor::{self, Mv};
use super::present;
use crate::present::message;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};
use crate::text::say_all::{FETCH_RADIUS, Run, SayAll};

/// A `Loaded` notice this recent (before the document's first publish) means the page has
/// finished loading.
const LOADED_FRESH: Duration = Duration::from_secs(4);
/// A first publish waits this long for the `load-complete` that has not come yet; then the
/// page reads itself anyway.
const LOAD_WAIT: Duration = Duration::from_millis(1500);
/// A wait that outlives this is forgotten.
const WAIT_LIMIT: Duration = Duration::from_secs(15);

/// A document waiting for its page to finish loading before it reads itself.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AutoRead {
    pub doc: DocId,
    /// The epoch of the entry speech: anything newer (a key, another focus) cancels the wait.
    pub epoch: Epoch,
    /// Read at this time even if `load-complete` has not come.
    pub fallback: Instant,
    pub until: Instant,
}

impl Reader {
    /// `text.say_all` in browse mode: read from the virtual cursor. False when browse mode has
    /// no document to read.
    pub(crate) fn web_say_all_start(&mut self, epoch: Epoch, fx: &mut ReaderEffects) -> bool {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return false;
        };
        let Some(b) = self.web.as_ref() else {
            return false;
        };
        let Some(h) = b.docs.get(doc) else {
            return false;
        };
        let last = b.last_off;
        let from = h.read.with(|d| cursor::offset_of(d, cur).unwrap_or(last));
        self.web_read_from(doc, from, epoch, false, fx);
        true
    }

    /// Start a reading of `doc` at flat offset `from`. `queue_first` puts the first chunk
    /// behind the utterance being spoken (a title) instead of interrupting it.
    pub(crate) fn web_read_from(
        &mut self,
        doc: DocId,
        from: u32,
        epoch: Epoch,
        queue_first: bool,
        fx: &mut ReaderEffects,
    ) {
        let mut run = Run::over_doc(doc, epoch, from, self.now);
        run.queue_first = queue_first;
        self.text.say_all = SayAll::Running(run);
        self.say_all_pump(fx);
    }

    /// The flat text from `around` on, for a reading's buffer: `(base, text, length)`.
    pub(crate) fn web_flat_window(&self, doc: DocId, around: u32) -> Option<(u32, String, u32)> {
        let h = self.web.as_ref()?.docs.get(doc)?;
        Some(h.read.with(|d| {
            let len = d.text_len();
            let base = around.min(len);
            let end = base.saturating_add(2 * FETCH_RADIUS).min(len);
            (base, d.text_in(base, end), len)
        }))
    }

    /// One chunk of the page as the line presenter speaks it, or `None` when nothing in it is
    /// speakable.
    pub(crate) fn web_chunk_utterance(
        &self,
        doc: DocId,
        range: &std::ops::Range<u32>,
        id: UttId,
        epoch: Epoch,
    ) -> Option<Utterance> {
        let h = self.web.as_ref()?.docs.get(doc)?;
        let items = h
            .read
            .with(|d| present::describe(d, range.start, range.end));
        if items.is_empty() {
            return None;
        }
        let mut u = message("", id, epoch);
        u.prio = Prio::Text;
        u.segments.clear();
        u.segments.extend(present::segments(&items));
        Some(u)
    }

    /// The virtual cursor goes to the chunk being spoken (and the page scrolls to it, debounced).
    pub(crate) fn web_follow(&mut self, doc: DocId, off: u32, fx: &mut ReaderEffects) {
        let now = self.now;
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let Some(cur) = h.read.with(|d| cursor::cursor_at(d, off)) else {
            return;
        };
        if !matches!(b.mode, WebMode::Browse { doc: d, .. } if d == doc) {
            return;
        }
        self.web_set_cursor(doc, cur, off, now);
        fx.push(ReaderEffect::ArmTimer(now + SCROLL_DEBOUNCE));
    }

    // ---- page auto-read -------------------------------------------------------------------

    /// A document was published for the first time and its entry speech (`spoke`) was made.
    /// Read it from its first line now when the page has finished loading, else wait for the
    /// browser's `load-complete`.
    pub(crate) fn web_auto_read(
        &mut self,
        doc: DocId,
        app: gorca_model::AppId,
        spoke: bool,
        fx: &mut ReaderEffects,
    ) {
        if !self.cmd.effective.browse.say_all_on_load {
            return;
        }
        let now = self.now;
        let epoch = self.epoch.current();
        let Some(b) = self.web.as_mut() else { return };
        if !matches!(b.mode, WebMode::Browse { doc: d, .. } if d == doc) {
            return;
        }
        let loaded = b
            .loaded
            .get(&app)
            .is_some_and(|t| now.saturating_duration_since(*t) < LOADED_FRESH);
        if loaded {
            self.web_auto_read_now(doc, spoke, fx);
        } else {
            b.auto_read = Some(AutoRead {
                doc,
                epoch,
                fallback: now + LOAD_WAIT,
                until: now + WAIT_LIMIT,
            });
            fx.push(ReaderEffect::ArmTimer(now + LOAD_WAIT));
        }
    }

    /// Timers of the wait for `load-complete`.
    pub(crate) fn web_auto_read_timer(&mut self, fx: &mut ReaderEffects) {
        let now = self.now;
        let epoch = self.epoch.current();
        let Some(b) = self.web.as_mut() else { return };
        let Some(w) = b.auto_read else { return };
        if w.epoch != epoch || now > w.until {
            b.auto_read = None;
        } else if now >= w.fallback {
            self.web_auto_read_now(w.doc, true, fx);
        } else {
            fx.push(ReaderEffect::ArmTimer(w.fallback));
        }
    }

    /// The first line of `doc` is where a page starts reading.
    fn web_auto_read_now(&mut self, doc: DocId, queue_first: bool, fx: &mut ReaderEffects) {
        let epoch = self.epoch.current();
        let Some(b) = self.web.as_mut() else { return };
        b.auto_read = None;
        let max_line = b.settings.max_line;
        let Some(h) = b.docs.get(doc) else { return };
        let from = h.read.with(|d| match cursor::first_line(d, max_line) {
            Mv::Landed { off, .. } => Some(off),
            Mv::Edge(_) => None,
        });
        if let Some(from) = from {
            self.web_read_from(doc, from, epoch, queue_first, fx);
        }
    }

    /// The browser reported `load-complete` for the document rooted at `node`.
    pub(crate) fn web_loaded(&mut self, node: gorca_model::NodeKey, fx: &mut ReaderEffects) {
        let now = self.now;
        let epoch = self.epoch.current();
        let Some(b) = self.web.as_mut() else { return };
        b.loaded.insert(node.app, now);
        let Some(wait) = b.auto_read else { return };
        // Only the document that is waiting, only if nothing happened since its entry speech.
        let same_doc = b.join.get(node).is_some_and(|(d, _)| d == wait.doc);
        if wait.until < now || wait.epoch != epoch {
            b.auto_read = None;
            return;
        }
        if same_doc && self.cmd.effective.browse.say_all_on_load {
            self.web_auto_read_now(wait.doc, true, fx);
        }
    }
}
