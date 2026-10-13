//! The `Focus`, `Reply` and `Timer` arms: focus settling, the mirror read of B-1 5.2, fetch on
//! miss, RV-1 revalidation (5.3), the removed-focus proof of life (5.5), the state-change
//! presenter S1 (5.6), presentation and the focused-application protection published once a
//! focus is validated.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_mirror::fill::{DemandOutcome, DemandWhy};
use gorca_model::{
    Epoch, Ifaces, NodeKey, NodeSnapshot, NodeView, PropMask, StateSet, UttId, Utterance,
};
use gorca_rt::BudgetClass;

/// Focus changes closer together than this are a storm (held key, scripted walk, a list
/// scrolling under the pointer): hits are then held for [`STORM_HOLD`], so the last focus wins
/// and the speech server is not flooded with fragments. An isolated focus speaks at once.
pub const RAPID_FOCUS: Duration = Duration::from_millis(20);
/// How long a hit in a storm waits for a newer focus to supersede it.
pub const STORM_HOLD: Duration = Duration::from_millis(4);

use super::log::{PresentRecord, PresentSource};
use super::{PresentContext, changed, message, present_change};
use crate::SpeechIntent;
use crate::focus::{FocusEffect, FocusMsg, FocusState, settle_delay};
use crate::reader::{
    DUPLICATE_FOCUS_WINDOW, FetchKind, FocusLaneMsg, QueryReply, Reader, ReaderEffect,
    ReaderEffects,
};

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
/// `DESCRIPTION`, which no presenter speaks yet. Asking for it would add a call to the cold
/// path for nothing (B-1: the cold path stays what Phase 1 measured).
pub const FOCUS_FIELDS: PropMask = PropMask::PRESENT.difference(PropMask::DESCRIPTION);

/// What a focus presentation needs from a snapshot (B-1 5.2): `FOCUS_FIELDS`, minus `VALUE` when the
/// node is known to have neither a Value nor a Text interface, plus `RELATIONS` when it has no
/// name (a label may name it).
pub fn focus_need(n: &NodeSnapshot) -> PropMask {
    let mut need = FOCUS_FIELDS;
    if !n.interfaces.is_empty() && !n.interfaces.intersects(Ifaces::VALUE | Ifaces::TEXT) {
        need.remove(PropMask::VALUE);
    }
    if n.name.as_deref().is_none_or(str::is_empty) {
        need |= PropMask::RELATIONS;
    }
    need
}

impl Reader {
    pub(crate) fn on_focus(&mut self, f: FocusLaneMsg, now: Instant, fx: &mut ReaderEffects) {
        match f {
            FocusLaneMsg::FocusChanged { node } => {
                self.selection_node = None;
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::ItemSelected { node } => {
                self.selection_node = Some(node);
                self.begin_focus(node, now, fx);
            }
            FocusLaneMsg::NodeRemoved(n) => self.apply_focus(FocusMsg::NodeRemoved(n), fx),
            FocusLaneMsg::AppGone(a) => {
                if let Some(f) = &self.focused_app {
                    f.forget(a);
                }
                self.apply_focus(FocusMsg::AppGone(a), fx);
            }
            FocusLaneMsg::AppHung(a) => self.apply_focus(FocusMsg::AppHung(a), fx),
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
            return;
        }
        self.last_focus = Some((node, now));
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
                let u = message("application not responding", id, self.epoch.current());
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
            }
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
                let need = focus_need(&v.snap);
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
                        FOCUS_FIELDS,
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
                fx.push(Self::fetch_effect(
                    node,
                    FOCUS_FIELDS,
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
                    FOCUS_FIELDS,
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
        if revalidate && !confirmed_after_trigger {
            self.counters.revalidations += 1;
            fx.push(Self::fetch_effect(
                v.snap.key,
                FOCUS_FIELDS,
                self.epoch.current(),
                FetchKind::Revalidate,
            ));
        }
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
                if from_cache {
                    // Every cache-served presentation may be refined by RV-1 (ADR-0001 8.1).
                    u.may_refine = true;
                }
                self.log_presentation(&u, snap, source, None);
                self.presented = Some(Presented {
                    node: snap.key,
                    snap: Arc::clone(snap),
                    utt: id,
                    epoch,
                    at: self.now,
                    ctx,
                });
                fx.push(ReaderEffect::Speak(SpeechIntent { utterance: u }));
                fx.push(ReaderEffect::FocusSettled { node: snap.key });
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
                    } else {
                        // The Reader's focus is here now and could not be validated: protection
                        // must not stay with whoever held it before (fail closed).
                        f.demote();
                    }
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

    fn log_presentation(
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
            return;
        }
        match r.kind {
            FetchKind::Focus => self.on_focus_reply(r, fx),
            FetchKind::Revalidate => self.on_revalidated(&r, fx),
            FetchKind::StateChange => self.on_state_reply(&r, fx),
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
                    self.present_cached(&v, src, false, r.epoch, fx);
                }
            }
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
        match &r.result {
            Ok(new) if new.states.contains(StateSet::DEFUNCT) => {
                self.apply_focus(FocusMsg::NodeRemoved(r.node), fx);
            }
            Ok(new) => {
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
                    u.refines = Some(p.utt);
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
            Err(
                gorca_connector_api::BackendError::Timeout
                | gorca_connector_api::BackendError::NotSupported
                | gorca_connector_api::BackendError::Busy
                | gorca_connector_api::BackendError::Protocol(_)
                | gorca_connector_api::BackendError::Unresolved,
            ) => {}
        }
    }

    /// S1: the focused node's state or value changed.
    fn on_state_changed(&mut self, node: NodeKey, mask: PropMask, fx: &mut ReaderEffects) {
        let Some(p) = &self.presented else { return };
        if p.node != node {
            return;
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
            Err(_) => return,
        };
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
