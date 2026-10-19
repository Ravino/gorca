// SPDX-License-Identifier: GPL-3.0-or-later
//! Drag lock: one pure machine per lockable button (ADR-0025 section 2.6, review C-7c). A button
//! pressed by `mouse.*_lock_toggle` stays down until the user toggles it, the 120 s limit passes
//! (gorca releases at 120 s, the helper's backstop is 125 s), or pointer emulation goes away.

use std::time::{Duration, Instant};

use super::pointer_req::PtrReqId;

/// gorca's auto-release (the owner's fixed value, MI-6).
pub const HOLD_LIMIT: Duration = Duration::from_secs(120);

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ReleaseWhy {
    /// The user toggled the lock or clicked that button.
    User,
    /// The 120 s limit passed.
    Expired,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HoldState {
    Up,
    Pressing {
        req: PtrReqId,
    },
    Held {
        since: Instant,
    },
    Releasing {
        req: PtrReqId,
        why: ReleaseWhy,
    },
    /// A request failed or timed out: nobody knows whether the button is down. The next toggle
    /// sends `ReleaseAll` first.
    Unknown,
}

/// What the Reader must send for a transition.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HoldCmd {
    Press,
    Release,
    ReleaseAll,
    /// A request for this button is already in flight: the key is answered "busy".
    Busy,
}

/// What to say after a reply.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum HoldEvent {
    Locked,
    Unlocked(ReleaseWhy),
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Hold {
    state: HoldState,
}

impl Default for Hold {
    fn default() -> Self {
        Self::new()
    }
}

impl Hold {
    pub const fn new() -> Self {
        Self {
            state: HoldState::Up,
        }
    }

    pub const fn is_held(&self) -> bool {
        matches!(self.state, HoldState::Held { .. })
    }

    /// Anything that needs a release when pointer emulation goes away.
    pub const fn needs_release(&self) -> bool {
        match self.state {
            HoldState::Pressing { .. }
            | HoldState::Held { .. }
            | HoldState::Releasing { .. }
            | HoldState::Unknown => true,
            HoldState::Up => false,
        }
    }

    /// The lock key (or a click of the locked button).
    pub fn toggle(&mut self, req: PtrReqId) -> HoldCmd {
        match self.state {
            HoldState::Up => {
                self.state = HoldState::Pressing { req };
                HoldCmd::Press
            }
            HoldState::Held { .. } => {
                self.state = HoldState::Releasing {
                    req,
                    why: ReleaseWhy::User,
                };
                HoldCmd::Release
            }
            HoldState::Unknown => {
                self.state = HoldState::Releasing {
                    req,
                    why: ReleaseWhy::User,
                };
                HoldCmd::ReleaseAll
            }
            HoldState::Pressing { .. } | HoldState::Releasing { .. } => HoldCmd::Busy,
        }
    }

    /// The 120 s limit: release a button held that long. `None` if nothing is due.
    pub fn expire(&mut self, now: Instant, req: PtrReqId) -> Option<HoldCmd> {
        match self.state {
            HoldState::Held { since } if now >= since + HOLD_LIMIT => {
                self.state = HoldState::Releasing {
                    req,
                    why: ReleaseWhy::Expired,
                };
                Some(HoldCmd::Release)
            }
            HoldState::Held { .. }
            | HoldState::Up
            | HoldState::Pressing { .. }
            | HoldState::Releasing { .. }
            | HoldState::Unknown => None,
        }
    }

    /// When the held button is due for release.
    pub fn deadline(&self) -> Option<Instant> {
        match self.state {
            HoldState::Held { since } => Some(since + HOLD_LIMIT),
            HoldState::Up
            | HoldState::Pressing { .. }
            | HoldState::Releasing { .. }
            | HoldState::Unknown => None,
        }
    }

    /// A successful reply to request `req`. `held` says whether the helper reports the button
    /// as held by us.
    pub fn reply_ok(&mut self, req: PtrReqId, held: bool, now: Instant) -> Option<HoldEvent> {
        match self.state {
            HoldState::Pressing { req: r } if r == req => {
                if held {
                    self.state = HoldState::Held { since: now };
                    Some(HoldEvent::Locked)
                } else {
                    self.state = HoldState::Unknown;
                    None
                }
            }
            HoldState::Releasing { req: r, why } if r == req => {
                self.state = HoldState::Up;
                Some(HoldEvent::Unlocked(why))
            }
            HoldState::Up
            | HoldState::Pressing { .. }
            | HoldState::Held { .. }
            | HoldState::Releasing { .. }
            | HoldState::Unknown => None,
        }
    }

    /// A failed or timed-out request: the state of the button is unknown. True if it was ours.
    pub fn reply_err(&mut self, req: PtrReqId) -> bool {
        match self.state {
            HoldState::Pressing { req: r } | HoldState::Releasing { req: r, .. } if r == req => {
                self.state = HoldState::Unknown;
                true
            }
            HoldState::Up
            | HoldState::Pressing { .. }
            | HoldState::Held { .. }
            | HoldState::Releasing { .. }
            | HoldState::Unknown => false,
        }
    }

    /// Pointer emulation went away (helper gone, `mouse.enabled` off): the helper releases on its
    /// own; here the button is simply not ours any more.
    pub fn lost(&mut self) {
        self.state = HoldState::Up;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const R1: PtrReqId = PtrReqId(1);
    const R2: PtrReqId = PtrReqId(2);

    #[test]
    fn lock_then_unlock() {
        let t0 = Instant::now();
        let mut h = Hold::new();
        assert_eq!(h.toggle(R1), HoldCmd::Press);
        assert_eq!(h.reply_ok(R1, true, t0), Some(HoldEvent::Locked));
        assert!(h.is_held());
        assert_eq!(h.deadline(), Some(t0 + HOLD_LIMIT));
        assert_eq!(h.toggle(R2), HoldCmd::Release);
        assert_eq!(
            h.reply_ok(R2, false, t0),
            Some(HoldEvent::Unlocked(ReleaseWhy::User))
        );
        assert_eq!(h.state, HoldState::Up);
    }

    #[test]
    fn a_toggle_while_a_request_is_in_flight_is_busy() {
        let mut h = Hold::new();
        h.toggle(R1);
        assert_eq!(h.toggle(R2), HoldCmd::Busy);
    }

    #[test]
    fn a_failed_press_goes_unknown_and_the_next_toggle_releases_everything_first() {
        let mut h = Hold::new();
        h.toggle(R1);
        assert!(h.reply_err(R1));
        assert_eq!(h.state, HoldState::Unknown);
        assert!(h.needs_release());
        assert_eq!(h.toggle(R2), HoldCmd::ReleaseAll);
        assert_eq!(
            h.reply_ok(R2, false, Instant::now()),
            Some(HoldEvent::Unlocked(ReleaseWhy::User))
        );
        assert_eq!(h.state, HoldState::Up);
    }

    #[test]
    fn a_reply_that_does_not_show_the_button_held_is_unknown() {
        let mut h = Hold::new();
        h.toggle(R1);
        assert_eq!(h.reply_ok(R1, false, Instant::now()), None);
        assert_eq!(h.state, HoldState::Unknown);
    }

    #[test]
    fn the_limit_releases_at_120_seconds_and_not_before() {
        let t0 = Instant::now();
        let mut h = Hold::new();
        h.toggle(R1);
        h.reply_ok(R1, true, t0);
        assert_eq!(h.expire(t0 + Duration::from_secs(119), R2), None);
        assert_eq!(h.expire(t0 + HOLD_LIMIT, R2), Some(HoldCmd::Release));
        assert_eq!(
            h.reply_ok(R2, false, t0),
            Some(HoldEvent::Unlocked(ReleaseWhy::Expired))
        );
    }

    #[test]
    fn replies_for_other_requests_are_ignored() {
        let mut h = Hold::new();
        h.toggle(R1);
        assert_eq!(h.reply_ok(R2, true, Instant::now()), None);
        assert!(!h.reply_err(R2));
        assert_eq!(h.state, HoldState::Pressing { req: R1 });
    }

    #[test]
    fn losing_pointer_emulation_drops_every_state() {
        let mut h = Hold::new();
        h.toggle(R1);
        h.reply_ok(R1, true, Instant::now());
        h.lost();
        assert_eq!(h.state, HoldState::Up);
        assert!(!h.needs_release());
    }
}
