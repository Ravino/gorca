// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-(source, scope) state machine (ADR-0024 section 2.6, r2 table). `step` is pure and
//! exhaustive; `SourceBook` adds the stable-identity memory (M-7) and the source-wide
//! quarantine (3 scopes within 10 minutes) around it.

use std::collections::HashMap;

use crate::admit::Scope;
use crate::descriptor::StableIdentity;

/// Window of the strike counter: 3 strikes within this many milliseconds quarantine a scope.
pub const STRIKE_WINDOW_MS: u64 = 60_000;
/// Strikes that quarantine a (source, scope).
pub const STRIKE_LIMIT: usize = 3;
/// Quarantined scopes within `WIDE_WINDOW_MS` that quarantine the whole source.
pub const WIDE_LIMIT: usize = 3;
pub const WIDE_WINDOW_MS: u64 = 600_000;

/// Timestamps (ms) of the strikes inside the window, oldest first.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Strikes {
    at: [u64; STRIKE_LIMIT],
    n: u8,
}

impl Strikes {
    pub const NONE: Strikes = Strikes {
        at: [0; STRIKE_LIMIT],
        n: 0,
    };

    pub const fn count(self) -> usize {
        self.n as usize
    }

    /// Record a strike at `now_ms`; the bool is true when the limit is reached.
    #[must_use]
    pub fn record(self, now_ms: u64) -> (Strikes, bool) {
        let mut out = Strikes::NONE;
        for t in self.at.iter().take(self.count()) {
            if now_ms.saturating_sub(*t) <= STRIKE_WINDOW_MS {
                out.push(*t);
            }
        }
        out.push(now_ms);
        let hit = out.count() >= STRIKE_LIMIT;
        (out, hit)
    }

    fn push(&mut self, t: u64) {
        if let Some(slot) = self.at.get_mut(self.n as usize) {
            *slot = t;
            self.n += 1;
        }
    }
}

/// State of one source in one scope.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourceState {
    Registered,
    Active(Strikes),
    Throttled(Strikes),
    /// Facts masked, port closed.
    Paused,
    /// Facts stale; purged after 30 s unless re-attached with the same identity.
    Detached {
        purged: bool,
    },
    /// Facts masked, one notice.
    Quarantined,
    /// Terminal for this identity this session.
    Revoked,
}

impl SourceState {
    /// True when this source's facts may win (the writer's mask for the epoch snapshot).
    pub const fn facts_win(self) -> bool {
        match self {
            SourceState::Active(_) | SourceState::Throttled(_) => true,
            SourceState::Registered
            | SourceState::Paused
            | SourceState::Detached { .. }
            | SourceState::Quarantined
            | SourceState::Revoked => false,
        }
    }
}

/// What the core remembers per stable identity (and scope) for the whole session.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Remembered {
    Paused,
    Quarantined,
    Revoked,
}

impl Remembered {
    const fn rank(self) -> u8 {
        match self {
            Remembered::Paused => 0,
            Remembered::Quarantined => 1,
            Remembered::Revoked => 2,
        }
    }

    /// The more restrictive of two memories.
    #[must_use]
    pub const fn stronger(self, other: Remembered) -> Remembered {
        if self.rank() >= other.rank() {
            self
        } else {
            other
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourceEvent {
    /// The transport connected and identity verified. `remembered` comes from the book.
    Hello {
        remembered: Option<Remembered>,
    },
    /// A rejected fact with `Severity::Violation` (carries the time for the strike window).
    Violation {
        at_ms: u64,
    },
    /// `Severity::Overload`, or the port cap.
    RateExceeded,
    /// No overload for 10 s.
    Quiet,
    TransportClosed,
    UserRevoke,
    PrivacyPause,
    Resume {
        by_user: bool,
    },
    /// The 30 s detach grace ran out.
    DetachExpired,
    /// The source-wide quarantine fired (3 scopes quarantined within 10 minutes).
    WideQuarantine,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Notice {
    Quarantined,
    Throttled,
    Revoked,
}

/// Effects the runtime executes, in order. `Mask`, `Unmask` and `PurgeRegisters` bump the
/// scope's `SourceEpoch` and travel to the writer as one timeline message; `ReFuseFocus` asks
/// for the synchronous focus-chain re-fuse of the same turn (M-10).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourceEffect {
    Mask,
    Unmask,
    PurgeRegisters,
    ClosePort,
    OpenPort,
    ReFuseFocus,
    Notice(Notice),
    Remember(Remembered),
    Forget,
}

type Out = (SourceState, Vec<SourceEffect>);

fn stay(s: SourceState) -> Out {
    (s, Vec::new())
}

fn silence(r: Remembered, notice: Option<Notice>) -> Vec<SourceEffect> {
    let mut v = vec![
        SourceEffect::Remember(r),
        SourceEffect::Mask,
        SourceEffect::ReFuseFocus,
        SourceEffect::ClosePort,
    ];
    if let Some(n) = notice {
        v.push(SourceEffect::Notice(n));
    }
    v
}

fn on_hello(remembered: Option<Remembered>, unmask: bool) -> Out {
    match remembered {
        None => {
            let mut v = Vec::new();
            if unmask {
                v.push(SourceEffect::Unmask);
            }
            v.push(SourceEffect::OpenPort);
            (SourceState::Active(Strikes::NONE), v)
        }
        Some(Remembered::Paused) => (SourceState::Paused, vec![SourceEffect::ClosePort]),
        Some(Remembered::Quarantined) => (SourceState::Quarantined, vec![SourceEffect::ClosePort]),
        Some(Remembered::Revoked) => (SourceState::Revoked, vec![SourceEffect::ClosePort]),
    }
}

fn revoke(purge: bool) -> Out {
    let mut v = silence(Remembered::Revoked, Some(Notice::Revoked));
    if purge {
        v.push(SourceEffect::PurgeRegisters);
    }
    (SourceState::Revoked, v)
}

fn resume() -> Out {
    (
        SourceState::Active(Strikes::NONE),
        vec![
            SourceEffect::Forget,
            SourceEffect::Unmask,
            SourceEffect::OpenPort,
            SourceEffect::ReFuseFocus,
        ],
    )
}

fn detach(from_live: bool) -> Out {
    let v = if from_live {
        vec![SourceEffect::Mask, SourceEffect::ReFuseFocus]
    } else {
        Vec::new()
    };
    (SourceState::Detached { purged: false }, v)
}

fn strike(s: SourceState, st: Strikes, at_ms: u64, throttled: bool) -> Out {
    let (st, hit) = st.record(at_ms);
    if hit {
        (
            SourceState::Quarantined,
            silence(Remembered::Quarantined, Some(Notice::Quarantined)),
        )
    } else if throttled {
        (SourceState::Throttled(st), Vec::new())
    } else {
        let _ = s;
        (SourceState::Active(st), Vec::new())
    }
}

/// The transition function of the r2 table. Pure; every (state, event) pair is handled.
pub fn step(s: SourceState, e: SourceEvent) -> (SourceState, Vec<SourceEffect>) {
    match s {
        SourceState::Registered => match e {
            SourceEvent::Hello { remembered } => on_hello(remembered, false),
            SourceEvent::Violation { .. } | SourceEvent::UserRevoke => revoke(false),
            SourceEvent::PrivacyPause => (
                SourceState::Paused,
                vec![
                    SourceEffect::Remember(Remembered::Paused),
                    SourceEffect::ClosePort,
                ],
            ),
            SourceEvent::TransportClosed => detach(false),
            SourceEvent::WideQuarantine => (
                SourceState::Quarantined,
                silence(Remembered::Quarantined, None),
            ),
            SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::Resume { .. }
            | SourceEvent::DetachExpired => stay(s),
        },
        SourceState::Active(st) | SourceState::Throttled(st) => {
            let throttled = matches!(s, SourceState::Throttled(_));
            match e {
                SourceEvent::Violation { at_ms } => strike(s, st, at_ms, throttled),
                SourceEvent::RateExceeded => {
                    if throttled {
                        stay(s)
                    } else {
                        (
                            SourceState::Throttled(st),
                            vec![SourceEffect::Notice(Notice::Throttled)],
                        )
                    }
                }
                SourceEvent::Quiet => {
                    if throttled {
                        (SourceState::Active(st), Vec::new())
                    } else {
                        stay(s)
                    }
                }
                SourceEvent::TransportClosed => detach(true),
                SourceEvent::UserRevoke => revoke(true),
                SourceEvent::PrivacyPause => {
                    (SourceState::Paused, silence(Remembered::Paused, None))
                }
                SourceEvent::WideQuarantine => (
                    SourceState::Quarantined,
                    silence(Remembered::Quarantined, Some(Notice::Quarantined)),
                ),
                SourceEvent::Hello { .. }
                | SourceEvent::Resume { .. }
                | SourceEvent::DetachExpired => stay(s),
            }
        }
        SourceState::Paused => match e {
            SourceEvent::TransportClosed => (s, vec![SourceEffect::PurgeRegisters]),
            SourceEvent::UserRevoke => revoke(false),
            SourceEvent::Resume { .. } => resume(),
            SourceEvent::WideQuarantine => (
                SourceState::Quarantined,
                vec![SourceEffect::Remember(Remembered::Quarantined)],
            ),
            SourceEvent::Hello { .. }
            | SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::PrivacyPause
            | SourceEvent::DetachExpired => stay(s),
        },
        SourceState::Detached { purged } => match e {
            SourceEvent::Hello { remembered } => on_hello(remembered, true),
            SourceEvent::UserRevoke => {
                let mut v = silence(Remembered::Revoked, Some(Notice::Revoked));
                v.push(SourceEffect::PurgeRegisters);
                (SourceState::Revoked, v)
            }
            SourceEvent::PrivacyPause => (s, vec![SourceEffect::Remember(Remembered::Paused)]),
            SourceEvent::WideQuarantine => (
                SourceState::Quarantined,
                vec![
                    SourceEffect::Remember(Remembered::Quarantined),
                    SourceEffect::PurgeRegisters,
                ],
            ),
            SourceEvent::DetachExpired => {
                if purged {
                    stay(s)
                } else {
                    (
                        SourceState::Detached { purged: true },
                        vec![SourceEffect::PurgeRegisters],
                    )
                }
            }
            SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::TransportClosed
            | SourceEvent::Resume { .. } => stay(s),
        },
        SourceState::Quarantined => match e {
            SourceEvent::TransportClosed => (s, vec![SourceEffect::PurgeRegisters]),
            SourceEvent::UserRevoke => revoke(false),
            SourceEvent::Resume { by_user: true } => resume(),
            SourceEvent::Resume { by_user: false }
            | SourceEvent::Hello { .. }
            | SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::PrivacyPause
            | SourceEvent::DetachExpired
            | SourceEvent::WideQuarantine => stay(s),
        },
        SourceState::Revoked => stay(s),
    }
}

/// An effect tagged with the scope it applies to (a wide quarantine touches several scopes).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ScopedEffect {
    pub scope: Scope,
    pub effect: SourceEffect,
}

/// All machines of the session, keyed by (stable identity, scope), plus the identity memory.
/// Pure: timestamps are inputs. Quarantine, pause and revocation survive reconnecting (M-7);
/// a quarantine in one scope never affects another scope until the source-wide limit is hit.
#[derive(Debug, Default)]
pub struct SourceBook {
    states: HashMap<(StableIdentity, Scope), SourceState>,
    scope_memory: HashMap<(StableIdentity, Scope), Remembered>,
    wide_memory: HashMap<StableIdentity, Remembered>,
    quarantines: HashMap<StableIdentity, Vec<(Scope, u64)>>,
}

impl SourceBook {
    pub fn new() -> SourceBook {
        SourceBook::default()
    }

    /// The state of one (source, scope); `Registered` when unseen.
    pub fn state(&self, id: &StableIdentity, scope: Scope) -> SourceState {
        self.states
            .get(&(id.clone(), scope))
            .copied()
            .unwrap_or(SourceState::Registered)
    }

    pub fn remembered(&self, id: &StableIdentity, scope: Scope) -> Option<Remembered> {
        let a = self.scope_memory.get(&(id.clone(), scope)).copied();
        let b = self.wide_memory.get(id).copied();
        match (a, b) {
            (Some(a), Some(b)) => Some(a.stronger(b)),
            (Some(x), None) | (None, Some(x)) => Some(x),
            (None, None) => None,
        }
    }

    /// Apply one event to one (source, scope). `Hello` gets its `remembered` from the book
    /// whatever the caller passed, so reconnecting cannot escape a quarantine.
    pub fn apply(
        &mut self,
        id: &StableIdentity,
        scope: Scope,
        event: SourceEvent,
        now_ms: u64,
    ) -> Vec<ScopedEffect> {
        let event = match event {
            SourceEvent::Hello { .. } => SourceEvent::Hello {
                remembered: self.remembered(id, scope),
            },
            other @ (SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::TransportClosed
            | SourceEvent::UserRevoke
            | SourceEvent::PrivacyPause
            | SourceEvent::Resume { .. }
            | SourceEvent::DetachExpired
            | SourceEvent::WideQuarantine) => other,
        };
        let mut out = Vec::new();
        let mut wide = false;
        let cur = self.state(id, scope);
        let (next, effects) = step(cur, event);
        self.states.insert((id.clone(), scope), next);
        for eff in effects {
            match eff {
                SourceEffect::Remember(r) => {
                    let key = (id.clone(), scope);
                    let new = self.scope_memory.get(&key).map_or(r, |old| old.stronger(r));
                    self.scope_memory.insert(key, new);
                    if let Remembered::Quarantined = r {
                        wide |= self.note_quarantine(id, scope, now_ms);
                    }
                }
                SourceEffect::Forget => {
                    self.scope_memory.remove(&(id.clone(), scope));
                }
                SourceEffect::Mask
                | SourceEffect::Unmask
                | SourceEffect::PurgeRegisters
                | SourceEffect::ClosePort
                | SourceEffect::OpenPort
                | SourceEffect::ReFuseFocus
                | SourceEffect::Notice(_) => {}
            }
            out.push(ScopedEffect { scope, effect: eff });
        }
        if wide {
            self.wide_memory.insert(id.clone(), Remembered::Quarantined);
            let others: Vec<Scope> = self
                .states
                .keys()
                .filter(|(i, s)| i == id && *s != scope)
                .map(|(_, s)| *s)
                .collect();
            for s in others {
                let cur = self.state(id, s);
                let (next, effects) = step(cur, SourceEvent::WideQuarantine);
                self.states.insert((id.clone(), s), next);
                out.extend(
                    effects
                        .into_iter()
                        .map(|effect| ScopedEffect { scope: s, effect }),
                );
            }
        }
        out
    }

    /// Record a scope quarantine; true when the source-wide limit is reached.
    fn note_quarantine(&mut self, id: &StableIdentity, scope: Scope, now_ms: u64) -> bool {
        let log = self.quarantines.entry(id.clone()).or_default();
        log.retain(|(s, t)| *s != scope && now_ms.saturating_sub(*t) <= WIDE_WINDOW_MS);
        log.push((scope, now_ms));
        log.len() >= WIDE_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::PeerProof;
    use gorca_model::ids::{AppId, ConnectorId};

    fn id() -> StableIdentity {
        StableIdentity {
            name: "ext".into(),
            peer: PeerProof::Extension("e".into()),
        }
    }

    fn scope(n: u32) -> Scope {
        Scope {
            app: AppId {
                connector: ConnectorId(1),
                serial: n,
            },
            doc: None,
        }
    }

    #[test]
    fn three_strikes_in_a_minute_quarantine() {
        let mut s = SourceState::Active(Strikes::NONE);
        for t in [0, 10_000] {
            let (n, e) = step(s, SourceEvent::Violation { at_ms: t });
            assert!(e.is_empty());
            s = n;
        }
        let (n, e) = step(s, SourceEvent::Violation { at_ms: 59_000 });
        assert_eq!(n, SourceState::Quarantined);
        assert!(e.contains(&SourceEffect::Notice(Notice::Quarantined)));
        assert!(e.contains(&SourceEffect::ReFuseFocus));
    }

    #[test]
    fn strikes_outside_the_window_expire() {
        let mut s = SourceState::Active(Strikes::NONE);
        for t in [0, 30_000, 100_000, 130_000] {
            s = step(s, SourceEvent::Violation { at_ms: t }).0;
            assert!(matches!(s, SourceState::Active(_)), "t={t}");
        }
    }

    #[test]
    fn reconnect_does_not_escape_quarantine() {
        let mut b = SourceBook::new();
        let (i, sc) = (id(), scope(1));
        b.apply(&i, sc, SourceEvent::Hello { remembered: None }, 0);
        for t in [1, 2, 3] {
            b.apply(&i, sc, SourceEvent::Violation { at_ms: t }, t);
        }
        assert_eq!(b.state(&i, sc), SourceState::Quarantined);
        b.apply(&i, sc, SourceEvent::TransportClosed, 10);
        // A Hello that lies about its memory is overridden by the book.
        let fresh = StableIdentity {
            name: "ext".into(),
            peer: PeerProof::Extension("e".into()),
        };
        b.apply(&fresh, sc, SourceEvent::Hello { remembered: None }, 20);
        assert_eq!(b.state(&i, sc), SourceState::Quarantined);
        // Another scope of the same source is unaffected.
        b.apply(&i, scope(2), SourceEvent::Hello { remembered: None }, 21);
        assert!(b.state(&i, scope(2)).facts_win());
    }

    #[test]
    fn three_quarantined_scopes_quarantine_the_source() {
        let mut b = SourceBook::new();
        let i = id();
        for n in 1..=4 {
            b.apply(&i, scope(n), SourceEvent::Hello { remembered: None }, 0);
        }
        for n in 1..=3 {
            for t in 0..3 {
                b.apply(
                    &i,
                    scope(n),
                    SourceEvent::Violation { at_ms: t },
                    u64::from(n) * 1000,
                );
            }
        }
        for n in 1..=4 {
            assert_eq!(b.state(&i, scope(n)), SourceState::Quarantined, "scope {n}");
        }
        // A brand new scope of the same identity is quarantined on Hello too.
        b.apply(&i, scope(9), SourceEvent::Hello { remembered: None }, 5000);
        assert_eq!(b.state(&i, scope(9)), SourceState::Quarantined);
    }

    #[test]
    fn detach_purges_after_grace_unless_reattached() {
        let (s, _) = step(
            SourceState::Active(Strikes::NONE),
            SourceEvent::TransportClosed,
        );
        let (s2, e) = step(s, SourceEvent::DetachExpired);
        assert_eq!(s2, SourceState::Detached { purged: true });
        assert_eq!(e, vec![SourceEffect::PurgeRegisters]);
        let (s3, _) = step(s, SourceEvent::Hello { remembered: None });
        assert!(s3.facts_win());
    }
}
