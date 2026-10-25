// SPDX-License-Identifier: GPL-3.0-or-later
//! The strike / backoff / disable state machine (ADR-0026 §2.5.4 with the §2.4.6 delta).
//!
//! The writer owns the [`Slot`]; the plugin actor owns the instance. Calls, results and faults
//! reach the slot as events (replies processed by the writer). Every `(state, event)` cell is an
//! explicit arm: there are no wildcard arms, so a new state or event fails to compile until every
//! cell is decided.

// Each (state, event) cell is its own arm on purpose, even where two cells do the same thing.
#![allow(clippy::match_same_arms)]

use std::time::{Duration, Instant};

/// Job id (monotonic per writer).
pub type JobId = u64;

/// Strikes allowed in the window before escalation.
pub const STRIKE_LIMIT: usize = 3;
/// Window length for strikes and overruns.
pub const WINDOW: Duration = Duration::from_secs(60);
/// Demotion to background-only after a wall overrun.
pub const DEMOTE_FOR: Duration = Duration::from_secs(60);
/// `D(1)`.
pub const BACKOFF_1: Duration = Duration::from_secs(30);
/// `D(2)`.
pub const BACKOFF_2: Duration = Duration::from_secs(300);
/// Highest escalation level; a full window at this level disables the slot.
pub const MAX_LEVEL: u8 = 2;

/// What went wrong in a call. Not a fault: calls to imports without permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// Guest trap (`unreachable`, panic, out-of-bounds, stack).
    Trap,
    /// The fuel grant ran out.
    OutOfFuel,
    /// The memory limiter denied growth.
    MemoryDenied,
    /// Malformed effects: forged handles beyond tolerance, cap exceeded, B-6 overflow, oversize
    /// lift refused before the copy.
    Malformed,
    /// Instantiation failed.
    Instantiate,
    /// The host watchdog inside imports cut the call.
    Watchdog,
    /// The actor was lost (host panic, closed channel, wedged thread).
    ActorLost,
}

/// A sliding window of the last `N` events within [`WINDOW`].
#[derive(Debug, Clone, Default)]
pub struct Window {
    at: Vec<Instant>,
}

impl Window {
    /// Record an event; true when the window is now full (`STRIKE_LIMIT` within `WINDOW`).
    pub fn record(&mut self, now: Instant) -> bool {
        self.at
            .retain(|t| now.saturating_duration_since(*t) < WINDOW);
        self.at.push(now);
        self.at.len() >= STRIKE_LIMIT
    }

    /// Events currently inside the window.
    #[must_use]
    pub fn len(&self) -> usize {
        self.at.len()
    }

    /// True when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.at.is_empty()
    }

    fn clear(&mut self) {
        self.at.clear();
    }
}

/// Slot states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotState {
    /// No pre-loaded component yet.
    Unloaded,
    /// Component ready, no live instance.
    Cold,
    /// Live instance; `busy` is the job in flight.
    Ready {
        /// The job running on the actor, if any.
        busy: Option<JobId>,
    },
    /// Facts masked until `until`.
    Backoff {
        /// Expiry.
        until: Instant,
    },
    /// Off for this scope and session.
    Disabled,
}

/// Events of the table (columns of §2.5.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Pre-load finished.
    PreloadDone,
    /// Pre-load failed.
    PreloadFailed,
    /// A call is needed for the given job.
    CallNeeded(JobId),
    /// The call returned normally.
    CallOk,
    /// The call was cancelled or displaced before it ran: not a fault, frees the slot.
    Cancelled,
    /// The call faulted (or the instantiate failed).
    Fault(FaultKind),
    /// Wall overrun, window not necessarily full.
    WallOverrun,
    /// Backoff timer expired.
    BackoffExpired,
    /// The user re-enabled the plugin.
    UserReEnable,
    /// The package was swapped.
    PackageSwapped,
    /// The scope closed.
    ScopeCloses,
}

/// What the caller must do after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing.
    Nothing,
    /// Start the off-lane pre-load.
    Preload,
    /// Send the job to the actor.
    Send,
    /// The actor is busy: merge the nodes into the pending set.
    Coalesce,
    /// Fail open: no call, publish without this plugin's facts.
    Skip,
    /// Drop the instance (the next call re-instantiates it).
    DropInstance,
    /// Drop the instance and speak the one notice per plugin per session.
    DropInstanceAndNotice,
    /// The slot is dropped (scope closed); `level` is kept by the owner of the plugin.
    DropSlot,
    /// Impossible cell: state unchanged, the caller logs it.
    Impossible,
}

/// One slot per (plugin, scope, lane).
#[derive(Debug, Clone)]
pub struct Slot {
    state: SlotState,
    level: u8,
    strikes: Window,
    overruns: Window,
    interactive_ok_after: Option<Instant>,
    epoch: u64,
    stale_replies: u64,
}

impl Default for Slot {
    fn default() -> Self {
        Self::new()
    }
}

impl Slot {
    /// A fresh slot in `Unloaded`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: SlotState::Unloaded,
            level: 0,
            strikes: Window::default(),
            overruns: Window::default(),
            interactive_ok_after: None,
            epoch: 0,
            stale_replies: 0,
        }
    }

    /// Current state.
    #[must_use]
    pub fn state(&self) -> SlotState {
        self.state
    }

    /// Escalation level (0..=2).
    #[must_use]
    pub fn level(&self) -> u8 {
        self.level
    }

    /// Strikes inside the window.
    #[must_use]
    pub fn strikes(&self) -> usize {
        self.strikes.len()
    }

    /// Incarnation counter: replies from another incarnation are stale.
    #[must_use]
    pub fn slot_epoch(&self) -> u64 {
        self.epoch
    }

    /// Replies dropped for a wrong `slot_epoch`.
    #[must_use]
    pub fn stale_replies(&self) -> u64 {
        self.stale_replies
    }

    /// May an interactive call be made now (not demoted)?
    #[must_use]
    pub fn interactive_allowed(&self, now: Instant) -> bool {
        self.interactive_ok_after.is_none_or(|t| now >= t)
    }

    /// A reply from the actor, tagged with the epoch it was sent under. A stale reply is
    /// dropped and counted: it cannot revive a `Backoff` or `Disabled` slot.
    pub fn on_reply(&mut self, epoch: u64, ev: Event, now: Instant) -> Action {
        if epoch != self.epoch {
            self.stale_replies += 1;
            return Action::Nothing;
        }
        self.event(ev, now)
    }

    fn escalate(&mut self, now: Instant) -> Action {
        // A fault: record a strike; not full -> Cold; full and level < MAX -> Backoff; else off.
        self.epoch += 1;
        self.overruns.clear();
        if !self.strikes.record(now) {
            self.state = SlotState::Cold;
            return Action::DropInstance;
        }
        if self.level < MAX_LEVEL {
            self.level += 1;
            let d = if self.level == 1 {
                BACKOFF_1
            } else {
                BACKOFF_2
            };
            self.state = SlotState::Backoff { until: now + d };
            Action::DropInstance
        } else {
            self.state = SlotState::Disabled;
            Action::DropInstanceAndNotice
        }
    }

    fn overrun(&mut self, now: Instant) -> Action {
        if self.overruns.record(now) {
            // Three wall overruns in 60 s are one fault.
            return self.escalate(now);
        }
        self.interactive_ok_after = Some(now + DEMOTE_FOR);
        Action::Nothing
    }

    fn clear_windows(&mut self) {
        self.strikes.clear();
        self.overruns.clear();
        self.interactive_ok_after = None;
    }

    /// Apply one event to the table.
    pub fn event(&mut self, ev: Event, now: Instant) -> Action {
        match self.state {
            SlotState::Unloaded => match ev {
                Event::PreloadDone => {
                    self.state = SlotState::Cold;
                    Action::Nothing
                }
                Event::PreloadFailed => {
                    self.state = SlotState::Disabled;
                    Action::DropInstanceAndNotice
                }
                Event::CallNeeded(_) => Action::Skip,
                Event::CallOk
                | Event::Cancelled
                | Event::Fault(_)
                | Event::WallOverrun
                | Event::BackoffExpired => Action::Impossible,
                Event::UserReEnable => Action::Nothing,
                Event::PackageSwapped => Action::Preload,
                Event::ScopeCloses => {
                    self.epoch += 1;
                    Action::DropSlot
                }
            },
            SlotState::Cold => match ev {
                Event::PreloadDone => Action::Nothing,
                Event::PreloadFailed => Action::Impossible,
                Event::CallNeeded(job) => {
                    self.state = SlotState::Ready { busy: Some(job) };
                    Action::Send
                }
                Event::CallOk | Event::Cancelled | Event::WallOverrun | Event::BackoffExpired => {
                    Action::Impossible
                }
                // A fault from the instantiate itself.
                Event::Fault(_) => self.escalate(now),
                Event::UserReEnable => Action::Nothing,
                Event::PackageSwapped => {
                    self.epoch += 1;
                    self.state = SlotState::Unloaded;
                    Action::Preload
                }
                Event::ScopeCloses => {
                    self.epoch += 1;
                    Action::DropSlot
                }
            },
            SlotState::Ready { busy } => match ev {
                Event::PreloadDone => Action::Nothing,
                Event::PreloadFailed | Event::BackoffExpired => Action::Impossible,
                Event::CallNeeded(job) => {
                    if busy.is_some() {
                        Action::Coalesce
                    } else {
                        self.state = SlotState::Ready { busy: Some(job) };
                        Action::Send
                    }
                }
                Event::CallOk | Event::Cancelled => {
                    self.state = SlotState::Ready { busy: None };
                    Action::Nothing
                }
                Event::Fault(_) => self.escalate(now),
                Event::WallOverrun => self.overrun(now),
                Event::UserReEnable => Action::Nothing,
                Event::PackageSwapped => {
                    self.epoch += 1;
                    self.state = SlotState::Unloaded;
                    Action::Preload
                }
                Event::ScopeCloses => {
                    self.epoch += 1;
                    Action::DropSlot
                }
            },
            SlotState::Backoff { until } => match ev {
                Event::PreloadDone => Action::Nothing,
                Event::PreloadFailed
                | Event::CallOk
                | Event::Cancelled
                | Event::Fault(_)
                | Event::WallOverrun => Action::Impossible,
                Event::CallNeeded(_) => {
                    if now >= until {
                        self.clear_windows();
                        self.epoch += 1;
                        self.state = SlotState::Cold;
                        // The expiry and the call arrive together: take the call.
                        return self.event(ev, now);
                    }
                    Action::Skip
                }
                Event::BackoffExpired => {
                    self.clear_windows();
                    self.epoch += 1;
                    self.state = SlotState::Cold;
                    Action::Nothing
                }
                Event::UserReEnable => {
                    self.clear_windows();
                    self.level = 0;
                    self.epoch += 1;
                    self.state = SlotState::Cold;
                    Action::Nothing
                }
                Event::PackageSwapped => {
                    self.epoch += 1;
                    self.state = SlotState::Unloaded;
                    Action::Preload
                }
                Event::ScopeCloses => {
                    self.epoch += 1;
                    Action::DropSlot
                }
            },
            SlotState::Disabled => match ev {
                Event::PreloadDone => Action::Nothing,
                Event::PreloadFailed
                | Event::CallOk
                | Event::Cancelled
                | Event::Fault(_)
                | Event::WallOverrun
                | Event::BackoffExpired => Action::Impossible,
                Event::CallNeeded(_) => Action::Skip,
                Event::UserReEnable => {
                    self.clear_windows();
                    self.level = 0;
                    self.epoch += 1;
                    self.state = SlotState::Cold;
                    Action::Nothing
                }
                Event::PackageSwapped => Action::Nothing,
                Event::ScopeCloses => {
                    self.epoch += 1;
                    Action::DropSlot
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn ready() -> (Slot, Instant) {
        let now = t0();
        let mut s = Slot::new();
        s.event(Event::PreloadDone, now);
        (s, now)
    }

    #[test]
    fn call_cycle_and_coalesce() {
        let (mut s, now) = ready();
        assert_eq!(s.event(Event::CallNeeded(1), now), Action::Send);
        assert_eq!(s.state(), SlotState::Ready { busy: Some(1) });
        assert_eq!(s.event(Event::CallNeeded(2), now), Action::Coalesce);
        assert_eq!(s.event(Event::CallOk, now), Action::Nothing);
        assert_eq!(s.state(), SlotState::Ready { busy: None });
    }

    #[test]
    fn first_faults_go_cold_third_backs_off() {
        let (mut s, now) = ready();
        for i in 0..2 {
            s.event(Event::CallNeeded(i), now);
            assert_eq!(
                s.event(Event::Fault(FaultKind::Trap), now),
                Action::DropInstance
            );
            assert_eq!(s.state(), SlotState::Cold);
        }
        s.event(Event::CallNeeded(9), now);
        assert_eq!(
            s.event(Event::Fault(FaultKind::OutOfFuel), now),
            Action::DropInstance
        );
        assert_eq!(
            s.state(),
            SlotState::Backoff {
                until: now + BACKOFF_1
            }
        );
        assert_eq!(s.level(), 1);
        assert_eq!(s.event(Event::CallNeeded(10), now), Action::Skip);
    }

    #[test]
    fn nine_faults_disable() {
        let (mut s, mut now) = ready();
        let mut notice = false;
        for round in 0..3 {
            for i in 0..3 {
                s.event(Event::CallNeeded(i), now);
                let a = s.event(Event::Fault(FaultKind::Trap), now);
                notice |= a == Action::DropInstanceAndNotice;
            }
            if round < 2 {
                now += if round == 0 { BACKOFF_1 } else { BACKOFF_2 };
                assert_eq!(s.event(Event::BackoffExpired, now), Action::Nothing);
                assert_eq!(s.state(), SlotState::Cold);
                assert_eq!(s.strikes(), 0, "expiry clears the windows");
            }
        }
        assert_eq!(s.state(), SlotState::Disabled);
        assert!(notice);
        assert_eq!(s.event(Event::CallNeeded(1), now), Action::Skip);
        assert_eq!(s.event(Event::UserReEnable, now), Action::Nothing);
        assert_eq!((s.state(), s.level()), (SlotState::Cold, 0));
    }

    #[test]
    fn strikes_outside_the_window_do_not_accumulate() {
        let (mut s, now) = ready();
        s.event(Event::CallNeeded(1), now);
        s.event(Event::Fault(FaultKind::Trap), now);
        s.event(Event::CallNeeded(2), now);
        s.event(
            Event::Fault(FaultKind::Trap),
            now + WINDOW + Duration::from_secs(1),
        );
        assert_eq!(s.state(), SlotState::Cold);
        assert_eq!(s.strikes(), 1);
    }

    #[test]
    fn overruns_demote_then_three_make_a_fault() {
        let (mut s, now) = ready();
        s.event(Event::CallNeeded(1), now);
        assert_eq!(s.event(Event::WallOverrun, now), Action::Nothing);
        assert!(!s.interactive_allowed(now));
        assert!(s.interactive_allowed(now + DEMOTE_FOR));
        s.event(Event::WallOverrun, now);
        assert_eq!(s.event(Event::WallOverrun, now), Action::DropInstance);
        assert_eq!(s.state(), SlotState::Cold);
        assert_eq!(s.strikes(), 1);
    }

    #[test]
    fn cancelled_reply_frees_the_slot() {
        let (mut s, now) = ready();
        assert_eq!(s.event(Event::CallNeeded(1), now), Action::Send);
        assert_eq!(
            s.on_reply(s.slot_epoch(), Event::Cancelled, now),
            Action::Nothing
        );
        assert_eq!(s.state(), SlotState::Ready { busy: None });
        assert_eq!(s.event(Event::CallNeeded(2), now), Action::Send);
    }

    #[test]
    fn stale_reply_is_dropped_and_cannot_revive() {
        let (mut s, now) = ready();
        s.event(Event::CallNeeded(1), now);
        let old = s.slot_epoch();
        for i in 0..3 {
            s.event(Event::CallNeeded(i), now);
            s.event(Event::Fault(FaultKind::Trap), now);
        }
        assert!(matches!(s.state(), SlotState::Backoff { .. }));
        assert_eq!(s.on_reply(old, Event::CallOk, now), Action::Nothing);
        assert!(matches!(s.state(), SlotState::Backoff { .. }));
        assert_eq!(s.stale_replies(), 1);
    }

    #[test]
    fn preload_failure_disables_and_swap_restarts() {
        let now = t0();
        let mut s = Slot::new();
        assert_eq!(s.event(Event::CallNeeded(1), now), Action::Skip);
        assert_eq!(
            s.event(Event::PreloadFailed, now),
            Action::DropInstanceAndNotice
        );
        assert_eq!(s.state(), SlotState::Disabled);
        assert_eq!(s.event(Event::PackageSwapped, now), Action::Nothing);
        let (mut r, now) = ready();
        assert_eq!(r.event(Event::PackageSwapped, now), Action::Preload);
        assert_eq!(r.state(), SlotState::Unloaded);
        assert_eq!(r.event(Event::ScopeCloses, now), Action::DropSlot);
    }

    #[test]
    fn backoff_expiry_is_taken_by_the_next_call() {
        let (mut s, now) = ready();
        for i in 0..3 {
            s.event(Event::CallNeeded(i), now);
            s.event(Event::Fault(FaultKind::Trap), now);
        }
        assert_eq!(s.event(Event::CallNeeded(7), now + BACKOFF_1), Action::Send);
        assert_eq!(s.state(), SlotState::Ready { busy: Some(7) });
        assert_eq!(s.level(), 1, "level is kept across expiry");
    }
}
