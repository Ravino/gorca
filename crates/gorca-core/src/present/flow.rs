// SPDX-License-Identifier: GPL-3.0-or-later
//! The `Focus`, `Reply` and `Timer` arms: focus settling, the mirror read of B-1 5.2, fetch on
//! miss, RV-1 revalidation (5.3), the removed-focus proof of life (5.5), the state-change
//! presenter S1 (5.6), presentation and the focused-application protection published once a
//! focus is validated.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_mirror::fill::{DemandOutcome, DemandWhy};
use gorca_model::{
    Epoch, Ifaces, NodeKey, NodeSnapshot, NodeView, PropMask, Role, StateSet, UttId, Utterance,
};
use gorca_rt::BudgetClass;

/// Focus changes closer together than this are a storm (held key, scripted walk, a list
/// scrolling under the pointer): hits are then held for [`STORM_HOLD`], so the last focus wins
/// and the speech server is not flooded with fragments. An isolated focus speaks at once.
pub const RAPID_FOCUS: Duration = Duration::from_millis(20);
/// How long a hit in a storm waits for a newer focus to supersede it.
pub const STORM_HOLD: Duration = Duration::from_millis(4);

use super::log::{PresentRecord, PresentSource};
use super::{Changed, PresentContext, changed, message, present_change};
use crate::SpeechIntent;
use crate::focus::{FocusEffect, FocusMsg, FocusState, settle_delay};
use crate::reader::{
    DUPLICATE_FOCUS_WINDOW, FetchKind, FocusLaneMsg, QueryReply, RECONFIRM_KEY_INTERVAL, Reader,
    ReaderEffect, ReaderEffects, Reconfirm,
};

/// How long a focus whose fetch was refused as `Busy` waits before it is asked again, per
/// attempt. The immediate retry (N2) meets the same full queue of a flooded application (the
/// Shell opening the Alt+Tab switcher); a pause lets the queue drain. At most 3 further tries,
/// about 0.6 s in all, then the usual fallback.
const FOCUS_BACKOFF: [Duration; 3] = [
    Duration::from_millis(60),
    Duration::from_millis(150),
    Duration::from_millis(400),
];

/// A `Busy` focus fetch waiting for its next try (see [`FOCUS_BACKOFF`]).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct FocusBackoff {
    pub node: NodeKey,
    pub epoch: Epoch,
    /// Tries made so far (index into [`FOCUS_BACKOFF`] of the next delay).
    pub attempt: usize,
    /// When the next try is due; `None` while one is in flight.
    pub due: Option<Instant>,
}

/// How long after presenting a nameless node its one late name re-read is made: a relation or
/// label child usually arrives within a few frames.
pub const NAME_RETRY_DELAY: Duration = Duration::from_millis(120);

/// How long after a focus a change of the focused node's name is still spoken: a page that sets
/// its label from a script after the focus event (a framework state update) is heard once the name
/// is there; later than that the focus is long over and a changing name is not news.
pub const NAME_FOLLOW_WINDOW: Duration = Duration::from_millis(2000);

/// A name that is nothing to say: empty, blank, or made only of private-use glyphs (an icon font
/// draws its symbol with a code point that speaks as nothing).
pub(crate) fn name_is_blank(name: Option<&str>) -> bool {
    name.is_none_or(|n| {
        n.chars().all(|c| {
            c.is_whitespace()
                || c.is_control()
                || gorca_model::text::is_object_marker(c)
                || gorca_model::text::is_icon_glyph(c)
        })
    })
}

/// A presented node without a name, waiting for its one late re-read.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct NameRetry {
    pub node: NodeKey,
    pub epoch: Epoch,
    /// When the re-read is due; `None` once it is in flight.
    pub due: Option<Instant>,
}

/// Roles that are only meaningful with a name: speaking one bare ("button") is the symptom of a
/// late label.
pub(crate) fn needs_name(role: Role) -> bool {
    matches!(
        role,
        Role::Button
            | Role::ToggleButton
            | Role::CheckBox
            | Role::RadioButton
            | Role::MenuItem
            | Role::Link
            | Role::Tab
            | Role::ListItem
            | Role::ComboBox
    )
}

/// A correction replaces the earlier utterance only if less than this much time has passed
/// since it was submitted (ADR-0001 8.1: 150 ms of audio); otherwise it is a follow-up.
pub const REFINE_WINDOW: Duration = Duration::from_millis(150);

/// What the Reader remembers of its latest node presentation: the base of RV-1 comparisons, of
/// refines and of S1 speech.
#[derive(Clone, Debug)]
pub struct Presented {
    pub node: NodeKey,
    pub snap: Arc<NodeSnapshot>,
    pub utt: UttId,
    pub epoch: Epoch,
    pub at: Instant,
    /// How it was presented: a corrected utterance is built the same way.
    pub ctx: PresentContext,
}

/// The fields a focus presentation reads and a Reader fetch asks for: `PRESENT` without
/// `DESCRIPTION`, which no presenter speaks yet, plus `ATTRS` (M22a C1a: the semantic model's
/// attribute facts, one more call pipelined in the same batch, guarded by M-6b). Asking for
/// `DESCRIPTION` would add a call to the cold path for nothing.
///
/// The M-6b fallback switch is [`focus_fields_for`].
pub const FOCUS_FIELDS: PropMask = PropMask::PRESENT
    .difference(PropMask::DESCRIPTION)
    .union(PropMask::ATTRS);

/// [`FOCUS_FIELDS`] without `ATTRS`: what the cold focus fetch asked for before M22a.
pub const FOCUS_FIELDS_LAZY: PropMask = PropMask::PRESENT.difference(PropMask::DESCRIPTION);

/// The focus fetch mask for a switch position: [`FOCUS_FIELDS`], or [`FOCUS_FIELDS_LAZY`] when
/// the M-6b fallback is on. Pure; the Reader holds the position (`CoreConfig::lazy_attrs`).
pub const fn focus_fields_for(lazy: bool) -> PropMask {
    if lazy {
        FOCUS_FIELDS_LAZY
    } else {
        FOCUS_FIELDS
    }
}

/// What a focus presentation needs from a snapshot (B-1 5.2): `FOCUS_FIELDS`, minus `VALUE` when the
/// node is known to have neither a Value nor a Text interface, plus `RELATIONS` when it has no
/// name (a label may name it).
pub fn focus_need(n: &NodeSnapshot, lazy_attrs: bool) -> PropMask {
    let mut need = focus_fields_for(lazy_attrs);
    if !n.interfaces.is_empty() && !n.interfaces.intersects(Ifaces::VALUE | Ifaces::TEXT) {
        need.remove(PropMask::VALUE);
    }
    if n.name.as_deref().is_none_or(str::is_empty) {
        need |= PropMask::RELATIONS;
    }
    // A field's help or error text is spoken after its value: asked for only once the role is
    // known to be a field (the cold path stays as measured, B-1).
    if matches!(
        n.role,
        Role::TextEntry | Role::PasswordEntry | Role::ComboBox | Role::SpinButton
    ) {
        need |= PropMask::DESCRIPTION;
    }
    need
}

/// The browse mode word in front of, and the line behind, a focus utterance (BM-5, W3).
fn apply_web_extra(
    u: &mut gorca_model::Utterance,
    word: Option<&'static str>,
    tail: &[crate::browse::present::Item],
    o: gorca_speech::VerbalizeOpts,
) {
    use crate::browse::present::Item;
    let (lead, rest): (Vec<Item>, Vec<Item>) = tail
        .iter()
        .cloned()
        .partition(|i| matches!(i, Item::Lead(_)));
    let mut front: Vec<gorca_model::Segment> = Vec::new();
    if let Some(w) = word {
        front.push(gorca_model::Segment::new(
            Arc::from(w),
            gorca_model::SegmentKind::Text,
        ));
    }
    front.extend(crate::browse::present::segments(&lead, o));
    for (at, seg) in front.into_iter().enumerate() {
        u.segments.insert(at, seg);
    }
    u.segments
        .extend(crate::browse::present::segments(&rest, o));
}

impl Reader {
    pub(crate) fn on_focus(&mut self, f: FocusLaneMsg, now: Instant, fx: &mut ReaderEffects) {
        tracing::debug!(msg = ?f, "focus lane");
        match f {
            FocusLaneMsg::FocusChanged { node } => {
                self.selection_node = None;
                self.focus_retried = None;
                self.focus_backoff = None;
                self.revalidate_retried = None;
                self.web_on_focus(node, fx);
                self.window_on_focus(node.app);
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::ItemSelected { node } => {
                self.selection_node = Some(node);
                self.focus_backoff = None;
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::NodeRemoved(n) => {
                self.text_node_gone(Some(n), None);
                self.apply_focus(FocusMsg::NodeRemoved(n), fx);
            }
            FocusLaneMsg::AppGone(a) => {
                self.text_node_gone(None, Some(a));
                self.review_app_gone(a, fx);
                if let Some(f) = &self.focused_app {
                    f.forget(a);
                }
                self.apply_focus(FocusMsg::AppGone(a), fx);
            }
            FocusLaneMsg::AppHung(a) => self.apply_focus(FocusMsg::AppHung(a), fx),
            FocusLaneMsg::WindowActivated { window } => self.on_window_activated(window, fx),
            FocusLaneMsg::StateChanged { node, mask } => self.on_state_changed(node, mask, fx),
        }
    }

    pub(crate) fn begin_focus(&mut self, node: NodeKey, now: Instant, fx: &mut ReaderEffects) {
        let previous = self.last_focus;
        if let Some((last, at)) = previous
            && last == node
            && now.saturating_duration_since(at) < DUPLICATE_FOCUS_WINDOW
        {
            self.duplicate_focus_dropped += 1;
            self.reconfirm_focus(node, fx);
            return;
        }
        self.last_focus = Some((node, now));
        self.reconfirm = Reconfirm::Idle;
        self.timeout_charged = false;
        self.text_untrack("focus event");
        // Review follows the focus (ADR-0006 4.1) unless the user turned that off.
        self.review_focus_changed(fx);
        // The trigger gets its own sequence point: only a request issued after it can have
        // confirmed the mirror's content for this focus (RV-1, 5.3).
        self.focus_seq = self.dir.bump_seq(node.app);
        // The epoch advances first, so stale speech cancels now, not after δ (E-2).
        let epoch = self.epoch.advance();
        let cached = self.dir.lookup(node).hit();
        let role = cached.as_ref().map(|n| n.role);
        let rapid = previous.is_some_and(|(_, at)| now.saturating_duration_since(at) < RAPID_FOCUS);
        // Hits do not bypass the coalescer: in a storm they wait like a flapping role does.
        let hold = if cached.is_some() && rapid {
            STORM_HOLD
        } else {
            Duration::ZERO
        };
        let until = now + settle_delay(role).max(hold);
        self.apply_focus(FocusMsg::FocusChanged { node, epoch, until }, fx);
        if let FocusState::Settling { until, .. } = self.focus {
            if until <= now {
                self.apply_focus(FocusMsg::TimerElapsed { now }, fx);
            } else {
                fx.push(ReaderEffect::ArmTimer(until));
            }
        }
    }

    /// Present the focused `node` again (its first presentation never finished): the same path a
    /// focus event takes after the settle, under the current epoch.
    pub(crate) fn represent_focus(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        self.focus_retried = None;
        self.focus_backoff = None;
        let epoch = self.epoch.current();
        self.begin_present(node, epoch, fx);
    }

    /// Whether the application of `node` has not earned the protection yet (and `node` is a real
    /// focus, not a switcher selection). The grace of the previous application does not count:
    /// it ends, and only publishing makes the application current.
    pub(crate) fn protection_unearned(&self, node: NodeKey) -> bool {
        self.selection_node != Some(node)
            && self
                .focused_app
                .as_ref()
                .is_some_and(|f| f.get() != Some(node.app))
    }

    /// A repeated focus event for the node in focus. It is not presented again, but a toolkit
    /// that announces the focus before the widget says `FOCUSED` (the real VTE) repeats it once
    /// the state is true: read the state once, and `on_state_reply` earns the protection.
    fn reconfirm_focus(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        tracing::debug!(?node, state = ?self.reconfirm, unearned = self.protection_unearned(node), presented = ?self.presented.as_ref().map(|p| p.node), "repeated focus event");
        if self.reconfirm.node() == Some(node) || !self.protection_unearned(node) {
            return;
        }
        if self.presented.as_ref().is_some_and(|p| p.node == node) {
            self.ask_state(node, fx);
        } else {
            // The first presentation is still on its way; `present_with` asks when it is done.
            self.reconfirm = Reconfirm::Pending(node);
        }
    }

    /// A key press in the focused terminal whose application has not earned the protection: the
    /// reconfirm read failed or never ran, and nothing else restores it (the key publish is
    /// gone), so read the focus state again. Never a blind publish. Throttled by
    /// `RECONFIRM_KEY_INTERVAL` so a slow app is charged at most one timeout per interval.
    pub(crate) fn reconfirm_on_key(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        if !self.protection_unearned(node) {
            return;
        }
        let may_ask = match self.reconfirm {
            Reconfirm::Idle => true,
            Reconfirm::Failed(n, at) => {
                n != node || self.now.saturating_duration_since(at) >= RECONFIRM_KEY_INTERVAL
            }
            Reconfirm::Pending(_) | Reconfirm::Asked(_) | Reconfirm::Retried(_) => false,
        };
        if may_ask && self.presented.as_ref().is_some_and(|p| p.node == node) {
            self.counters.focus_reconfirm_key_reads += 1;
            self.ask_state(node, fx);
        }
    }

    /// The reply of a reconfirm read. True when it was one (the caller handles nothing more for
    /// a failure). A `Timeout`/`Busy` is retried once with a fresh deadline, but a `Timeout` only
    /// when no other timeout was charged to this focus (B-1 RR-1: at most two per focus, below
    /// the Hung threshold of three); the second failure leaves the state `Failed`, which a key
    /// press can reopen.
    fn reconfirm_failed(&mut self, r: &QueryReply, fx: &mut ReaderEffects) -> bool {
        use gorca_connector_api::BackendError as E;
        let Err(e) = &r.result else {
            if self.reconfirm.node() == Some(r.node) {
                // Answered. A reply that shows the node FOCUSED earns the protection (the
                // caller); one that does not leaves the read open for the terminal watch
                // (`term_protection_watch`), which asks again on a backoff.
                let focused = r
                    .result
                    .as_ref()
                    .is_ok_and(|n| n.states.contains(StateSet::FOCUSED));
                self.reconfirm = if focused {
                    Reconfirm::Asked(r.node)
                } else {
                    Reconfirm::Failed(r.node, self.now)
                };
            }
            return false;
        };
        let prior_timeout = self.timeout_charged;
        let retryable = match e {
            E::Timeout => {
                self.timeout_charged = true;
                !prior_timeout
            }
            E::Busy => true,
            E::Gone | E::NotSupported | E::Protocol(_) | E::Unresolved => false,
        };
        match self.reconfirm {
            Reconfirm::Asked(n) if n == r.node && retryable => {
                self.counters.focus_reconfirm_retries += 1;
                self.ask_state(r.node, fx);
                self.reconfirm = Reconfirm::Retried(r.node);
                true
            }
            Reconfirm::Asked(n) | Reconfirm::Retried(n) if n == r.node => {
                self.reconfirm = Reconfirm::Failed(n, self.now);
                true
            }
            Reconfirm::Idle
            | Reconfirm::Pending(_)
            | Reconfirm::Asked(_)
            | Reconfirm::Retried(_)
            | Reconfirm::Failed(..) => false,
        }
    }

    pub(crate) fn ask_state(&mut self, node: NodeKey, fx: &mut ReaderEffects) {
        tracing::debug!(?node, "focus reconfirm: reading the state");
        self.reconfirm = Reconfirm::Asked(node);
        self.counters.focus_reconfirms += 1;
        fx.push(Self::fetch_effect(
            node,
            PropMask::STATES | PropMask::VALUE | PropMask::NAME,
            self.epoch.current(),
            FetchKind::StateChange,
        ));
    }

    pub(crate) fn apply_focus(&mut self, msg: FocusMsg, fx: &mut ReaderEffects) {
        let (next, effect) = self.focus.step(msg);
        self.focus = next;
        let Some(effect) = effect else { return };
        match effect {
            FocusEffect::BeginPresent { node, epoch } => self.begin_present(node, epoch, fx),
            FocusEffect::Speak { node, epoch } => {
                // Reached when `Presented` was stepped by `present`; the snapshot was already
                // turned into an utterance there, so nothing is left to do for `node`/`epoch`.
                let _ = (node, epoch);
            }
            FocusEffect::QueryActiveWindowFocus => fx.push(ReaderEffect::QueryActiveWindowFocus),
            FocusEffect::AnnounceNotResponding => {
                let id = self.utt_id();
                let u = message(
                    gorca_i18n::t(gorca_i18n::MsgId::AppNotResponding),
                    id,
                    self.epoch.current(),
                );
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
        }
    }

    /// Ask the installed application policy (ADR-0008 W3) how to present `snap`; returns `true`
    /// when it says to stay silent. Only applications whose profile asks for it are asked.
    fn apply_policy(&self, snap: &NodeSnapshot, u: &mut Utterance) -> bool {
        use crate::builtin_ext::{PolicyFacts, PolicyMoment, PolicyVerdict};
        if !self.policy_active(snap.key.app) {
            return false;
        }
        let range = match &snap.value {
            Some(gorca_model::Value::Range { current, min, max }) => Some((*current, *min, *max)),
            Some(gorca_model::Value::Text(_)) | None => None,
        };
        let facts = PolicyFacts {
            moment: PolicyMoment::Focus,
            role: snap.role,
            states: snap.states,
            name: snap.name.as_deref(),
            description: snap.description.as_deref(),
            range,
        };
        match self.decide_policy(snap.key.app, &facts) {
            PolicyVerdict::Default | PolicyVerdict::Notice => false,
            PolicyVerdict::Silence => true,
            PolicyVerdict::Value(text) => {
                let seg =
                    gorca_model::Segment::new(Arc::from(text), gorca_model::SegmentKind::Value);
                match u
                    .segments
                    .iter_mut()
                    .find(|g| g.kind == gorca_model::SegmentKind::Value)
                {
                    Some(g) => *g = seg,
                    None => u.segments.push(seg),
                }
                false
            }
            PolicyVerdict::Say(segs) => {
                u.segments = segs.into_iter().collect();
                false
            }
        }
    }

    /// N7: Gecko reports no Sensitive on static content, which the connector maps to DISABLED.
    /// In a web application the "unavailable" of a static role is dropped; native applications
    /// (a GTK row or cell that really is insensitive) keep it.
    fn drop_web_unavailable(&self, u: &mut Utterance, snap: &NodeSnapshot) {
        let web = self
            .web
            .as_ref()
            .is_some_and(|b| b.web_apps.contains(&snap.key.app));
        if web && super::utterance::never_disabled(snap.role) {
            u.segments.retain(|s| {
                !(s.kind == gorca_model::SegmentKind::State
                    && &*s.text == gorca_i18n::t(gorca_i18n::MsgId::StateUnavailable))
            });
        }
    }

    fn fetch_effect(node: NodeKey, mask: PropMask, epoch: Epoch, kind: FetchKind) -> ReaderEffect {
        ReaderEffect::Fetch {
            node,
            mask,
            class: BudgetClass::Interactive,
            epoch,
            kind,
        }
    }

    /// B-1 5.2: read the mirror, lock-free. A hit for the purpose is presented from memory; a
    /// partial or stale read is presented too (tagged `may_refine`) unless NAME or ROLE is the
    /// problem, then the node is fetched and the reply presented. A removed key is not final
    /// (5.5): one Interactive proof-of-life fetch decides.
    fn begin_present(&mut self, node: NodeKey, epoch: Epoch, fx: &mut ReaderEffects) {
        match self.dir.view(node) {
            gorca_model::Lookup::Hit(v) => {
                let need = focus_need(&v.snap, self.lazy_attrs);
                let missing = need & !v.snap.have;
                let stale = v.stale & need;
                let outcome = if missing.is_empty() && stale.is_empty() {
                    DemandOutcome::Hit
                } else if stale.is_empty() {
                    DemandOutcome::Partial { missing }
                } else {
                    DemandOutcome::Stale { mask: stale }
                };
                if (missing | stale).intersects(PropMask::NAME | PropMask::ROLE) {
                    fx.push(Self::fetch_effect(
                        node,
                        focus_fields_for(self.lazy_attrs),
                        epoch,
                        FetchKind::Focus,
                    ));
                } else {
                    let source = match outcome {
                        DemandOutcome::Hit => PresentSource::Hit,
                        DemandOutcome::Partial { .. } => PresentSource::Partial,
                        DemandOutcome::Stale { .. } | DemandOutcome::Miss | DemandOutcome::Gone => {
                            PresentSource::Stale
                        }
                    };
                    // Speech first; the bookkeeping effects follow.
                    self.present_cached(&v, source, true, epoch, fx);
                }
                fx.push(ReaderEffect::Demand {
                    node,
                    why: DemandWhy::Focus,
                    outcome,
                });
            }
            gorca_model::Lookup::Miss => {
                // A node of a mirrored web document: speak it from the mirror now, then confirm
                // (RV-1); the cold fetch would cost a round trip into a possibly busy browser.
                if let Some(snap) = self.web_focus_snapshot(node) {
                    let v = NodeView {
                        snap: Arc::new(snap),
                        stale: PropMask::VALUE,
                        aged: true,
                    };
                    self.present_cached(&v, PresentSource::Stale, true, epoch, fx);
                    fx.push(ReaderEffect::Demand {
                        node,
                        why: DemandWhy::Focus,
                        outcome: DemandOutcome::Miss,
                    });
                    return;
                }
                fx.push(Self::fetch_effect(
                    node,
                    focus_fields_for(self.lazy_attrs),
                    epoch,
                    FetchKind::Focus,
                ));
                fx.push(ReaderEffect::Demand {
                    node,
                    why: DemandWhy::Focus,
                    outcome: DemandOutcome::Miss,
                });
            }
            gorca_model::Lookup::Gone => {
                // The key may come back (a reorder re-adds the same path): ask once (5.5).
                fx.push(Self::fetch_effect(
                    node,
                    focus_fields_for(self.lazy_attrs),
                    epoch,
                    FetchKind::Focus,
                ));
                fx.push(ReaderEffect::Demand {
                    node,
                    why: DemandWhy::Focus,
                    outcome: DemandOutcome::Gone,
                });
            }
        }
    }

    /// Present a node served from the mirror, tagged `may_refine`, and (RV-1) revalidate it
    /// unless the snapshot was confirmed by a request issued after the focus began.
    fn present_cached(
        &mut self,
        v: &NodeView,
        source: PresentSource,
        revalidate: bool,
        _epoch: Epoch,
        fx: &mut ReaderEffects,
    ) {
        self.counters.cached_presentations += 1;
        self.present_with(&v.snap, source, true, fx);
        // Only a request issued strictly after the focus began can have confirmed the content.
        let confirmed_after_trigger =
            v.snap.confirmed_seq > self.focus_seq && v.stale.is_empty() && !v.aged;
        // A fresh snapshot that did not earn the protection (a prefetched window carries no
        // `FOCUSED` state) is still re-read: only the re-read can earn it (review F-1, path 4).
        let unearned = self.selection_node != Some(v.snap.key)
            && self
                .focused_app
                .as_ref()
                .is_some_and(|f| !f.protects(v.snap.key.app));
        if revalidate && (!confirmed_after_trigger || unearned) {
            self.counters.revalidations += 1;
            if matches!(v.snap.role, Role::ListItem | Role::Row | Role::TreeItem) {
                self.counters.revalidations_list_row += 1;
            }
            fx.push(Self::fetch_effect(
                v.snap.key,
                focus_need(&v.snap, self.lazy_attrs),
                self.epoch.current(),
                FetchKind::Revalidate,
            ));
        }
    }

    /// M22b1 shadow mode: offer the legacy presenter's focus tokens and the fused entry to
    /// `gorca-present`. Only a cheap clone of the tokens and one `try_send`; never waits.
    pub(crate) fn shadow_focus(
        &self,
        ctx: PresentContext,
        snap: &NodeSnapshot,
        u: &gorca_model::Utterance,
        from_cache: bool,
    ) {
        use gorca_present::{FocusSample, LegacyToken, SkipWhy};
        let Some(h) = &self.present_shadow else {
            return;
        };
        if ctx != PresentContext::Focus {
            h.skipped(SkipWhy::Context);
            return;
        }
        let web = crate::aria::Node {
            role: snap.role,
            states: snap.states,
            attrs: &snap.attrs,
        }
        .is_web();
        if web {
            h.skipped(SkipWhy::Web);
            return;
        }
        let Some(entry) = &snap.sem.entry else {
            h.skipped(SkipWhy::NoEntry);
            return;
        };
        h.offer(FocusSample {
            node: snap.key,
            entry: Arc::clone(entry),
            legacy: u
                .segments
                .iter()
                .map(|s| LegacyToken {
                    kind: s.kind,
                    text: Arc::clone(&s.text),
                })
                .collect(),
            may_refine: from_cache || u.may_refine,
            secret: snap.role == Role::PasswordEntry,
            description_pause: self.cmd.effective.browse.description_pause,
        });
    }

    /// Step `Presented` for the snapshot's node and, if that produced the `Speak` effect, build
    /// the utterance from the snapshot at hand.
    pub(crate) fn present(&mut self, snap: &Arc<NodeSnapshot>, fx: &mut ReaderEffects) {
        self.present_with(snap, PresentSource::MissReply, false, fx);
    }

    fn present_with(
        &mut self,
        snap: &Arc<NodeSnapshot>,
        source: PresentSource,
        from_cache: bool,
        fx: &mut ReaderEffects,
    ) {
        // M8b: a web node is spoken with what the document mirror knows about it (ARIA role,
        // current, haspopup, description, error text), which a focus fetch does not carry.
        let overlaid = self.spoken_snapshot(snap);
        let snap = &overlaid;
        tracing::debug!(node = ?snap.key, states = ?snap.states, ?source, from_cache, "focus presented");
        let (next, effect) = self.focus.step(FocusMsg::Presented { node: snap.key });
        self.focus = next;
        match effect {
            Some(FocusEffect::Speak { epoch, .. }) => {
                let id = self.utt_id();
                let ctx = if self.selection_node == Some(snap.key) {
                    PresentContext::SelectedItem
                } else {
                    PresentContext::Focus
                };
                let mut u = self.presenters.present(ctx, snap, id, epoch);
                self.shadow_focus(ctx, snap, &u, from_cache);
                if !self.cmd.effective.browse.description_pause {
                    crate::aria::strip_pause(&mut u);
                }
                self.drop_web_unavailable(&mut u, snap);
                let silent = ctx == PresentContext::Focus && self.apply_policy(snap, &mut u);
                let hold = ctx == PresentContext::Focus
                    && !silent
                    && self.window_lead_in(snap, epoch, &mut u);
                if from_cache {
                    // Every cache-served presentation may be refined by RV-1 (ADR-0001 8.1).
                    u.may_refine = true;
                }
                // Browse mode: the mode word rides on the focus utterance (BM-5), and the line
                // at the cursor follows a document's own focus speech (W3).
                self.web_learn_focus(snap, fx);
                let word = self.web_take_word();
                let tail = self.web_take_tail();
                apply_web_extra(&mut u, word, &tail, self.verbalize_opts());
                self.web_extra = (word.is_some() || !tail.is_empty()).then_some((id, word, tail));
                // The context asserts `selected`, which the spoken text omits, so the log must
                // not carry a cached snapshot from before the event.
                let logged = if ctx == PresentContext::SelectedItem
                    && !snap.states.contains(StateSet::SELECTED)
                {
                    let mut c = (**snap).clone();
                    c.states |= StateSet::SELECTED;
                    Arc::new(c)
                } else {
                    Arc::clone(snap)
                };
                self.log_presentation(&u, &logged, source, None);
                self.presented = Some(Presented {
                    node: snap.key,
                    snap: Arc::clone(snap),
                    utt: id,
                    epoch,
                    at: self.now,
                    ctx,
                });
                if silent {
                    // The policy says nothing for this node; the bookkeeping above stands.
                } else if self.web_suppress_focus(snap.key) {
                } else if hold {
                    self.window_hold(u, fx);
                } else {
                    fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                    self.web_label_arm(snap, fx);
                }
                fx.push(ReaderEffect::FocusSettled { node: snap.key });
                self.name_retry_arm(snap, epoch, fx);
                if self.selection_node != Some(snap.key) {
                    self.text_track(snap, fx);
                }
                // Protection from eviction is earned only here: the focus passed the state
                // machine, was presented, and the fetched node itself shows `FOCUSED`. A focus
                // event alone, from any application, proves nothing.
                if self.selection_node == Some(snap.key) {
                    // A selected item in a switcher: not real focus, so the protection of the
                    // application that does hold focus stays as it is.
                } else if let Some(f) = &self.focused_app {
                    if snap.states.contains(StateSet::FOCUSED)
                        && !snap.states.contains(StateSet::DEFUNCT)
                    {
                        f.publish(snap.key.app);
                    } else if from_cache && f.protects(snap.key.app) {
                        // Served from the cache inside the application that already owns the
                        // protection: the re-read that follows decides (a prefetched sibling
                        // never shows `FOCUSED`, and demoting for one round trip drops the
                        // app's text events).
                    } else {
                        // The Reader's focus is here now and could not be validated: protection
                        // must not stay with whoever held it before (fail closed).
                        f.demote();
                    }
                }
                if self.reconfirm == Reconfirm::Pending(snap.key)
                    && self.protection_unearned(snap.key)
                {
                    self.ask_state(snap.key, fx);
                }
            }
            Some(
                FocusEffect::BeginPresent { .. }
                | FocusEffect::QueryActiveWindowFocus
                | FocusEffect::AnnounceNotResponding,
            )
            | None => {}
        }
    }

    pub(crate) fn log_presentation(
        &self,
        u: &Utterance,
        snap: &Arc<NodeSnapshot>,
        source: PresentSource,
        refines: Option<UttId>,
    ) {
        if let Some(log) = &self.present_log {
            log.record(PresentRecord {
                t_ns: gorca_rt::monotonic_ns(),
                utt_id: u.id.0,
                epoch: u.epoch.0,
                source,
                may_refine: u.may_refine,
                refines: refines.map(|r| r.0),
                snap: Arc::clone(snap),
            });
        }
    }

    pub(crate) fn on_reply(&mut self, r: QueryReply, fx: &mut ReaderEffects) {
        // E-3: results from a worker with an older epoch are dropped by whoever receives them.
        if r.epoch < self.epoch.current() {
            self.stale_replies_dropped += 1;
            // A key press advances the shared epoch while a reconfirm read is in flight: its
            // reply is dropped, and the read must not stay `Asked` for good (neither the terminal
            // watch nor a key would ever ask again). The node's state is simply still unknown.
            if r.kind == FetchKind::StateChange
                && matches!(self.reconfirm, Reconfirm::Asked(n) | Reconfirm::Retried(n) if n == r.node)
            {
                self.reconfirm = Reconfirm::Failed(r.node, self.now);
            }
            return;
        }
        if matches!(r.kind, FetchKind::Focus | FetchKind::Revalidate)
            && matches!(r.result, Err(gorca_connector_api::BackendError::Timeout))
        {
            self.timeout_charged = true;
        }
        match r.kind {
            FetchKind::Focus => self.on_focus_reply(r, fx),
            FetchKind::Revalidate => self.on_revalidated(&r, fx),
            FetchKind::StateChange => self.on_state_reply(&r, fx),
            FetchKind::Name => self.on_name_reply(&r, fx),
            // An answer to `info.where_am_i` and friends (CR-3), not a focus presentation.
            FetchKind::Info => {
                self.take_pending_reply(r.node, r.epoch, r.result.as_ref().ok().cloned(), fx);
            }
        }
    }

    fn on_focus_reply(&mut self, r: QueryReply, fx: &mut ReaderEffects) {
        match r.result {
            // A reply that says `defunct` is a removal: nothing is spoken from a dead key.
            Ok(snap) if snap.states.contains(StateSet::DEFUNCT) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
            }
            Ok(snap) => self.present(&snap, fx),
            // The object is gone for good (5.5): nothing from the cache may be spoken.
            Err(gorca_connector_api::BackendError::Gone) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
            }
            // Timeout, Busy, Unresolved, ...: speak from the cache if there is anything
            // (`may_refine`); `Unresolved` is a miss, never a removal.
            Err(
                gorca_connector_api::BackendError::Timeout
                | gorca_connector_api::BackendError::NotSupported
                | gorca_connector_api::BackendError::Busy
                | gorca_connector_api::BackendError::Protocol(_)
                | gorca_connector_api::BackendError::Unresolved,
            ) => {
                if let gorca_model::Lookup::Hit(v) = self.dir.view(r.node) {
                    let src = if v.stale.is_empty() {
                        PresentSource::Partial
                    } else {
                        PresentSource::Stale
                    };
                    // The cache is all there is, so the re-read is what can still confirm it.
                    // After a Timeout the app is slow: a second request on the same deadline
                    // would double the timeouts charged to it (review RR-1), so none is asked;
                    // a later FOCUSED state re-read earns the protection (RR-2).
                    let ask_rv1 =
                        !matches!(r.result, Err(gorca_connector_api::BackendError::Timeout));
                    self.present_cached(&v, src, ask_rv1, r.epoch, fx);
                } else if self.web_focus_fallback(r.node, fx) {
                    // N2: a web node is spoken from the document mirror (name and role).
                } else if self.focus_retried != Some(r.node) {
                    // One more try with a fresh deadline.
                    self.focus_retried = Some(r.node);
                    fx.push(Self::fetch_effect(
                        r.node,
                        focus_fields_for(self.lazy_attrs),
                        r.epoch,
                        FetchKind::Focus,
                    ));
                } else if matches!(r.result, Err(gorca_connector_api::BackendError::Busy))
                    && self.focus_backoff_wait(r.node, r.epoch, fx)
                {
                    // Refused twice in a row: the application is flooded (the Shell opening
                    // the Alt+Tab switcher). Ask again after a pause, not at once.
                } else if self.selection_node == Some(r.node) {
                    // A switcher item that cannot be read says nothing: "unknown" for an
                    // item the user is only passing over is noise, and the next move speaks.
                    tracing::debug!(node = ?r.node, "selected item unreadable: silent");
                } else {
                    // Never stay silent on focus: say that nothing is known.
                    self.say(gorca_i18n::t(gorca_i18n::MsgId::FocusUnknown), fx);
                }
            }
        }
    }

    /// The snapshot as it is spoken: with what the document's fused entry adds (M8b, M22a-C3: the
    /// name of a control the engine left nameless, the quirk-stripped name, description and the
    /// read-only and popup states), and a name that is only an icon glyph counted as none.
    fn spoken_snapshot(&self, snap: &Arc<NodeSnapshot>) -> Arc<NodeSnapshot> {
        let overlaid = self.web_overlay(snap);
        if overlaid.name.is_some() && name_is_blank(overlaid.name.as_deref()) {
            let mut c = (*overlaid).clone();
            c.name = None;
            return Arc::new(c);
        }
        overlaid
    }

    /// A node presented without a name gets one late re-read (once per node, only for roles
    /// that need a name and only if the speech was not silenced by policy).
    fn name_retry_arm(&mut self, snap: &Arc<NodeSnapshot>, epoch: Epoch, fx: &mut ReaderEffects) {
        let nameless = name_is_blank(snap.name.as_deref());
        if !nameless || !needs_name(snap.role) || self.name_retried == Some(snap.key) {
            return;
        }
        self.name_retried = Some(snap.key);
        let due = self.now + NAME_RETRY_DELAY;
        self.name_retry = Some(NameRetry {
            node: snap.key,
            epoch,
            due: Some(due),
        });
        fx.push(ReaderEffect::ArmTimer(due));
    }

    /// The timer of [`Self::name_retry_arm`].
    pub(crate) fn name_retry_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some(r) = self.name_retry else { return };
        let Some(due) = r.due else { return };
        if r.epoch != self.epoch.current() {
            self.name_retry = None;
        } else if now < due {
            fx.push(ReaderEffect::ArmTimer(due));
        } else {
            self.name_retry = Some(NameRetry { due: None, ..r });
            fx.push(Self::fetch_effect(
                r.node,
                PropMask::NAME | PropMask::STATES,
                r.epoch,
                FetchKind::Name,
            ));
        }
    }

    /// The late re-read: speak the name alone when it has appeared (no role again).
    fn on_name_reply(&mut self, r: &QueryReply, fx: &mut ReaderEffects) {
        if self.name_retry.is_some_and(|n| n.node == r.node) {
            self.name_retry = None;
        }
        let Some(p) = self.presented.clone() else {
            return;
        };
        if p.node != r.node || p.epoch != r.epoch {
            return;
        }
        let Ok(new) = &r.result else { return };
        if new.states.contains(StateSet::DEFUNCT) {
            return;
        }
        let Some(name) = new.name.as_deref().filter(|n| !name_is_blank(Some(n))) else {
            return;
        };
        if !name_is_blank(p.snap.name.as_deref()) {
            return;
        }
        let ch = Changed {
            name: true,
            role: false,
            states: StateSet::empty(),
            value: false,
            description: false,
        };
        let mut merged = (*p.snap).clone();
        merged.name = Some(name.into());
        let merged = Arc::new(merged);
        let id = self.utt_id();
        let u = present_change(&merged, ch, id, self.epoch.current(), None);
        self.log_presentation(&u, &merged, PresentSource::StateChange, None);
        self.presented = Some(Presented {
            node: p.node,
            snap: merged,
            utt: id,
            epoch: p.epoch,
            at: self.now,
            ctx: p.ctx,
        });
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// A web control spoken without a name, of a kind Orca infers a label for, asks its document
    /// for one (C3b): the text next to it, picked by layout, with a deadline. The answer
    /// ([`Self::web_label_done`]) refines or follows the utterance; no answer leaves it as it was.
    fn web_label_arm(&mut self, snap: &Arc<NodeSnapshot>, fx: &mut ReaderEffects) {
        if !name_is_blank(snap.name.as_deref()) || !gorca_doc::web::label::infers_for(snap.role) {
            return;
        }
        let Some(b) = self.web.as_mut() else { return };
        let Some((doc, node)) = b.join.get(snap.key) else {
            return;
        };
        b.label_req = b.label_req.wrapping_add(1);
        let req = b.label_req;
        b.label_pending = Some((req, snap.key));
        fx.push(ReaderEffect::Web(crate::WebEffect::Label {
            doc,
            node,
            role: snap.role,
            req,
        }));
    }

    /// The inferred label of the control in focus arrived. Spoken plainly, as Orca speaks an
    /// inferred label: within the refine window it replaces the utterance (label first, then the
    /// role), later it follows as the name alone. Dropped when the focus has moved on or the
    /// control got a name meanwhile.
    pub(crate) fn web_label_done(&mut self, req: u32, text: &str, fx: &mut ReaderEffects) {
        let Some(b) = self.web.as_mut() else { return };
        let Some((r, key)) = b.label_pending else {
            return;
        };
        if r != req {
            return;
        }
        b.label_pending = None;
        let Some(p) = self.presented.clone() else {
            return;
        };
        if p.node != key
            || p.epoch != self.epoch.current()
            || !name_is_blank(p.snap.name.as_deref())
        {
            return;
        }
        let Some(label) = crate::browse::commands::one_line_label(text) else {
            return;
        };
        let mut merged = (*p.snap).clone();
        merged.name = Some(Arc::from(label.as_str()));
        // The words are not said twice: a description that is the label stays unsaid.
        if crate::aria::speakable_description(merged.description.as_deref(), Some(&label)).is_none()
        {
            merged.description = None;
        }
        let merged = Arc::new(merged);
        let id = self.utt_id();
        let epoch = self.epoch.current();
        let refine = self.now.saturating_duration_since(p.at) < REFINE_WINDOW;
        let (mut u, source) = if refine {
            let mut u = self.presenters.present(p.ctx, &merged, id, epoch);
            self.drop_web_unavailable(&mut u, &merged);
            u.refines = Some(p.utt);
            if let Some((_, word, tail)) = self.web_extra.as_ref().filter(|e| e.0 == p.utt) {
                apply_web_extra(&mut u, *word, tail, self.verbalize_opts());
            }
            self.counters.refined += 1;
            (u, PresentSource::Refine)
        } else {
            let ch = Changed {
                name: true,
                role: false,
                states: StateSet::empty(),
                value: false,
                description: false,
            };
            self.counters.followups += 1;
            (
                present_change(&merged, ch, id, epoch, Some(p.utt)),
                PresentSource::Followup,
            )
        };
        u.may_refine = false;
        self.log_presentation(&u, &merged, source, Some(p.utt));
        self.presented = Some(Presented {
            node: p.node,
            snap: merged,
            utt: id,
            epoch,
            at: self.now,
            ctx: p.ctx,
        });
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }

    /// Schedule the next delayed try of a `Busy` focus fetch. False when the tries are used up.
    fn focus_backoff_wait(&mut self, node: NodeKey, epoch: Epoch, fx: &mut ReaderEffects) -> bool {
        let attempt = match self.focus_backoff {
            Some(b) if b.node == node && b.epoch == epoch => b.attempt,
            Some(_) | None => 0,
        };
        let Some(delay) = FOCUS_BACKOFF.get(attempt) else {
            self.focus_backoff = None;
            return false;
        };
        let due = self.now + *delay;
        self.focus_backoff = Some(FocusBackoff {
            node,
            epoch,
            attempt: attempt + 1,
            due: Some(due),
        });
        fx.push(ReaderEffect::ArmTimer(due));
        true
    }

    /// The timer of [`Self::focus_backoff_wait`]: ask again when due, drop it when the focus has
    /// moved on, and arm again when the timer fired for another subsystem (N1).
    pub(crate) fn focus_backoff_timer(&mut self, now: Instant, fx: &mut ReaderEffects) {
        let Some(b) = self.focus_backoff else {
            return;
        };
        let Some(due) = b.due else {
            return;
        };
        if b.epoch != self.epoch.current() {
            self.focus_backoff = None;
        } else if now < due {
            fx.push(ReaderEffect::ArmTimer(due));
        } else {
            self.focus_backoff = Some(FocusBackoff { due: None, ..b });
            fx.push(Self::fetch_effect(
                b.node,
                focus_fields_for(self.lazy_attrs),
                b.epoch,
                FetchKind::Focus,
            ));
        }
    }

    /// RV-1: compare the fresh node with what was presented. A difference is a correction.
    fn on_revalidated(&mut self, r: &QueryReply, fx: &mut ReaderEffects) {
        let Some(p) = self.presented.clone() else {
            return;
        };
        if p.node != r.node || p.epoch != r.epoch {
            return;
        }
        // A focus served from the cache (a prefetched sibling, an earlier visit) shows
        // the states of its fetch, not `FOCUSED`, and was not trusted at presentation
        // (fail closed). The re-read is the proof: earn the protection now, or the
        // text events of the focused application would never reach the Reader.
        if self.selection_node != Some(r.node)
            && let Ok(new) = &r.result
            && let Some(f) = &self.focused_app
        {
            if new.states.contains(StateSet::FOCUSED) && !new.states.contains(StateSet::DEFUNCT) {
                f.publish(r.node.app);
            } else if f.protects(r.node.app) {
                // The re-read answered: the node is not focused. What a cache-served
                // presentation left standing is withdrawn (fail closed).
                f.demote();
            }
        }
        // A web document's speech belongs to browse mode: the cursor line follows the focus
        // utterance and keys move it at once, so only a changed title is worth correcting (the
        // cache served the previous page). Other differences are counted, not spoken (M5).
        if p.snap.role == Role::DocumentWeb
            && let Ok(new) = &r.result
            && !new.states.contains(StateSet::DEFUNCT)
            && p.snap.name == new.name
        {
            if changed(&p.snap, new, skip_states(p.ctx)).is_empty() {
                self.counters.revalidations_agreed += 1;
            } else {
                self.counters.revalidations_suppressed += 1;
            }
            return;
        }
        match &r.result {
            Ok(new) if new.states.contains(StateSet::DEFUNCT) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
            }
            Ok(new) => {
                let new = &self.spoken_snapshot(new);
                let ch = changed(&p.snap, new, skip_states(p.ctx));
                if ch.is_empty() {
                    self.counters.revalidations_agreed += 1;
                    return;
                }
                let id = self.utt_id();
                let refine = self.now.saturating_duration_since(p.at) < REFINE_WINDOW
                    && r.epoch == self.epoch.current();
                let (mut u, source) = if refine {
                    // Replace: the whole node again, interrupting the earlier utterance.
                    let mut u = self.presenters.present(p.ctx, new, id, r.epoch);
                    self.drop_web_unavailable(&mut u, new);
                    u.refines = Some(p.utt);
                    if let Some((_, word, tail)) = self.web_extra.as_ref().filter(|e| e.0 == p.utt)
                    {
                        apply_web_extra(&mut u, *word, tail, self.verbalize_opts());
                    }
                    self.counters.refined += 1;
                    (u, PresentSource::Refine)
                } else {
                    self.counters.followups += 1;
                    (
                        present_change(new, ch, id, r.epoch, Some(p.utt)),
                        PresentSource::Followup,
                    )
                };
                u.may_refine = false;
                self.log_presentation(&u, new, source, Some(p.utt));
                self.presented = Some(Presented {
                    node: p.node,
                    snap: Arc::clone(new),
                    utt: id,
                    epoch: r.epoch,
                    at: self.now,
                    ctx: p.ctx,
                });
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
            // The node vanished between presenting and checking: the focus machine decides.
            Err(gorca_connector_api::BackendError::Gone) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
            }
            // A timed-out re-read is not repeated: the app is slow, and a retry would double the
            // timeouts charged to it (review RR-1). A later FOCUSED state re-read earns the
            // protection (RR-2).
            Err(gorca_connector_api::BackendError::Timeout) => {}
            // Any other failure of the re-read: it is the only thing that can confirm a
            // cache-served focus (protection, text events), so it is asked once more, and only
            // while the protection is still unearned.
            Err(
                gorca_connector_api::BackendError::NotSupported
                | gorca_connector_api::BackendError::Busy
                | gorca_connector_api::BackendError::Protocol(_)
                | gorca_connector_api::BackendError::Unresolved,
            ) => {
                let unearned = self
                    .focused_app
                    .as_ref()
                    .is_some_and(|f| !f.protects(r.node.app));
                if unearned && self.revalidate_retried != Some(r.node) {
                    self.revalidate_retried = Some(r.node);
                    fx.push(Self::fetch_effect(
                        r.node,
                        focus_fields_for(self.lazy_attrs),
                        r.epoch,
                        FetchKind::Revalidate,
                    ));
                }
            }
        }
    }

    /// S1: the focused node's state or value changed.
    fn on_state_changed(&mut self, node: NodeKey, mask: PropMask, fx: &mut ReaderEffects) {
        let Some(p) = &self.presented else { return };
        if p.node != node {
            return;
        }
        // A name-only change is news only right after the focus (see `NAME_FOLLOW_WINDOW`).
        // Outside web documents only a node spoken without a name follows: a title-like name
        // that changes after commands (a terminal) must not be echoed.
        if !mask.intersects(PropMask::STATES | PropMask::VALUE) {
            let soon = self.last_focus.is_some_and(|(n, at)| {
                n == node && self.now.saturating_duration_since(at) < NAME_FOLLOW_WINDOW
            });
            let eligible = name_is_blank(p.snap.name.as_deref())
                || self
                    .web
                    .as_ref()
                    .is_some_and(|b| b.web_apps.contains(&node.app));
            if !soon || !eligible {
                return;
            }
        }
        fx.push(ReaderEffect::Demand {
            node,
            why: DemandWhy::StateChange,
            outcome: DemandOutcome::Stale { mask },
        });
        fx.push(Self::fetch_effect(
            node,
            PropMask::STATES | PropMask::VALUE | PropMask::NAME,
            self.epoch.current(),
            FetchKind::StateChange,
        ));
    }

    fn on_state_reply(&mut self, r: &QueryReply, fx: &mut ReaderEffects) {
        tracing::debug!(node = ?r.node, states = ?r.result.as_ref().map(|n| n.states), presented = ?self.presented.as_ref().map(|p| p.node), "state reply");
        let Some(p) = self.presented.clone() else {
            return;
        };
        if p.node != r.node {
            return;
        }
        let new = match &r.result {
            Ok(n) if n.states.contains(StateSet::DEFUNCT) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
                return;
            }
            Ok(n) => n,
            Err(gorca_connector_api::BackendError::Gone) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
                return;
            }
            Err(_) => {
                self.reconfirm_failed(r, fx);
                return;
            }
        };
        self.reconfirm_failed(r, fx);
        // A re-read that shows the node FOCUSED is the proof a failed RV-1 never delivered
        // (review RR-2): earn the protection now.
        if self.selection_node != Some(r.node)
            && new.states.contains(StateSet::FOCUSED)
            && let Some(f) = &self.focused_app
        {
            f.publish(r.node.app);
        }
        // A terminal that is not (yet) focused says nothing of its own title-like name changing
        // (`on_state_changed` keeps that silent too): the watch reads it only for `FOCUSED`.
        if p.snap.role == Role::Terminal && !new.states.contains(StateSet::FOCUSED) {
            return;
        }
        let new = &self.spoken_snapshot(new);
        let ch = changed(&p.snap, new, skip_states(p.ctx));
        if ch.is_empty() {
            return;
        }
        let id = self.utt_id();
        let u = present_change(new, ch, id, self.epoch.current(), None);
        self.counters.state_changes_spoken += 1;
        self.log_presentation(&u, new, PresentSource::StateChange, None);
        self.presented = Some(Presented {
            node: p.node,
            snap: Arc::clone(new),
            utt: id,
            epoch: p.epoch,
            at: self.now,
            ctx: p.ctx,
        });
        fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
    }
}

/// States a context does not speak, so their change is not a correction.
fn skip_states(ctx: PresentContext) -> StateSet {
    match ctx {
        PresentContext::Focus => StateSet::empty(),
        PresentContext::SelectedItem => StateSet::SELECTED,
    }
}
