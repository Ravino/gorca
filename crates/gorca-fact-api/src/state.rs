// SPDX-License-Identifier: GPL-3.0-or-later
//! The per-(source, scope) state machine (ADR-0024 section 2.6, r2 table). `step` is pure and
//! exhaustive; `SourceBook` adds the stable-identity memory (M-7) and the source-wide
//! quarantine (3 scopes within 10 minutes) around it.

use std::collections::HashMap;

use crate::admit::Scope;
use crate::descriptor::{PeerProof, StableIdentity};

/// Window of the strike counter: 3 strikes within this many milliseconds quarantine a scope.
pub const STRIKE_WINDOW_MS: u64 = 60_000;
/// Strikes that quarantine a (source, scope).
pub const STRIKE_LIMIT: usize = 3;
/// Quarantined scopes within `WIDE_WINDOW_MS` that quarantine the whole source.
pub const WIDE_LIMIT: usize = 3;
pub const WIDE_WINDOW_MS: u64 = 600_000;
/// Bound on the quarantine log per peer and on tracked per-scope memory (review B-4).
/// Benign navigation races tolerated per window before they count as strikes (review).
pub const RACE_LIMIT: usize = 3;
pub const RACE_WINDOW_MS: u64 = 10_000;
pub const MAX_QUARANTINE_LOG: usize = 64;
pub const MAX_TRACKED_SCOPES: usize = 4096;

/// Timestamps (ms) of the strikes inside the window, oldest first.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct Strikes {
    at: [u64; STRIKE_LIMIT],
    n: u8,
    /// Navigation-race rejections inside `RACE_WINDOW_MS`; more than `RACE_LIMIT` strike.
    races: [u64; RACE_LIMIT + 1],
    nr: u8,
}

impl Strikes {
    pub const NONE: Strikes = Strikes {
        at: [0; STRIKE_LIMIT],
        n: 0,
        races: [0; RACE_LIMIT + 1],
        nr: 0,
    };

    pub const fn count(self) -> usize {
        self.n as usize
    }

    /// Record a strike at `now_ms`; the bool is true when the limit is reached.
    #[must_use]
    pub fn record(self, now_ms: u64) -> (Strikes, bool) {
        let mut out = Strikes {
            at: [0; STRIKE_LIMIT],
            n: 0,
            ..self
        };
        for t in self.at.iter().take(self.count()) {
            if now_ms.saturating_sub(*t) <= STRIKE_WINDOW_MS {
                out.push(*t);
            }
        }
        out.push(now_ms);
        let hit = out.count() >= STRIKE_LIMIT;
        (out, hit)
    }

    /// Record a benign `DocChanged` race; the bool is true when there are more than
    /// `RACE_LIMIT` inside the window, which the machine turns into a strike.
    #[must_use]
    pub fn record_race(self, now_ms: u64) -> (Strikes, bool) {
        let mut out = Strikes {
            races: [0; RACE_LIMIT + 1],
            nr: 0,
            ..self
        };
        for t in self.races.iter().take(self.nr as usize) {
            if now_ms.saturating_sub(*t) <= RACE_WINDOW_MS
                && let Some(slot) = out.races.get_mut(out.nr as usize)
            {
                *slot = *t;
                out.nr += 1;
            }
        }
        if let Some(slot) = out.races.get_mut(out.nr as usize) {
            *slot = now_ms;
            out.nr += 1;
        }
        let exceeded = out.nr as usize > RACE_LIMIT;
        (out, exceeded)
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
    /// A `DocChanged` rejection: benign up to `RACE_LIMIT` per `RACE_WINDOW_MS`, then a strike.
    DocRace {
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
    /// Forget this scope's memory only (end of a privacy pause, automatic resume).
    ForgetScope,
    /// Forget the scope's memory and the source-wide memory: explicit user resume only.
    ForgetAll,
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

fn resume(by_user: bool) -> Out {
    (
        SourceState::Active(Strikes::NONE),
        vec![
            if by_user {
                SourceEffect::ForgetAll
            } else {
                SourceEffect::ForgetScope
            },
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
            | SourceEvent::DocRace { .. }
            | SourceEvent::DetachExpired => stay(s),
        },
        SourceState::Active(st) | SourceState::Throttled(st) => {
            let throttled = matches!(s, SourceState::Throttled(_));
            match e {
                SourceEvent::Violation { at_ms } => strike(s, st, at_ms, throttled),
                SourceEvent::DocRace { at_ms } => {
                    let (st, exceeded) = st.record_race(at_ms);
                    if exceeded {
                        strike(s, st, at_ms, throttled)
                    } else if throttled {
                        (SourceState::Throttled(st), Vec::new())
                    } else {
                        (SourceState::Active(st), Vec::new())
                    }
                }
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
            SourceEvent::Resume { by_user } => resume(by_user),
            SourceEvent::WideQuarantine => (
                SourceState::Quarantined,
                vec![SourceEffect::Remember(Remembered::Quarantined)],
            ),
            SourceEvent::Hello { .. }
            | SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::PrivacyPause
            | SourceEvent::DocRace { .. }
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
            | SourceEvent::DocRace { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::TransportClosed
            | SourceEvent::Resume { .. } => stay(s),
        },
        SourceState::Quarantined => match e {
            SourceEvent::TransportClosed => (s, vec![SourceEffect::PurgeRegisters]),
            SourceEvent::UserRevoke => revoke(false),
            SourceEvent::Resume { by_user: true } => resume(true),
            SourceEvent::Resume { by_user: false }
            | SourceEvent::Hello { .. }
            | SourceEvent::Violation { .. }
            | SourceEvent::RateExceeded
            | SourceEvent::Quiet
            | SourceEvent::PrivacyPause
            | SourceEvent::DocRace { .. }
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
    // Memory is keyed on the verified peer, never on the self-declared name: a source cannot
    // escape quarantine or revocation by renaming itself.
    scope_memory: HashMap<(PeerProof, Scope), Remembered>,
    wide_memory: HashMap<PeerProof, Remembered>,
    quarantines: HashMap<PeerProof, Vec<(Scope, u64)>>,
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
        let a = self.scope_memory.get(&(id.peer.clone(), scope)).copied();
        let b = self.wide_memory.get(&id.peer).copied();
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
            | SourceEvent::DocRace { .. }
            | SourceEvent::DetachExpired
            | SourceEvent::WideQuarantine) => other,
        };
        let mut out = Vec::new();
        let mut wide = false;
        let mut cur = self.state(id, scope);
        // A remembered quarantine or revocation wins over any resume: bring the machine to the
        // remembered state first, so a non-user resume cannot reactivate the peer.
        if let SourceEvent::Resume { .. } = event
            && let SourceState::Registered | SourceState::Paused = cur
        {
            let pre = match self.remembered(id, scope) {
                Some(Remembered::Revoked) => Some(SourceEvent::UserRevoke),
                Some(Remembered::Quarantined) => Some(SourceEvent::WideQuarantine),
                Some(Remembered::Paused) | None => None,
            };
            if let Some(pre) = pre {
                let (n, fx) = step(cur, pre);
                cur = n;
                out.extend(fx.into_iter().map(|effect| ScopedEffect { scope, effect }));
            }
        }
        let (next, effects) = step(cur, event);
        self.states.insert((id.clone(), scope), next);
        self.evict(id, scope, next);
        for eff in effects {
            match eff {
                SourceEffect::Remember(r) => {
                    let key = (id.peer.clone(), scope);
                    let new = self.scope_memory.get(&key).map_or(r, |old| old.stronger(r));
                    self.scope_memory.insert(key, new);
                    if let Remembered::Quarantined = r {
                        wide |= self.note_quarantine(id, scope, now_ms);
                    }
                }
                SourceEffect::ForgetScope => {
                    self.scope_memory.remove(&(id.peer.clone(), scope));
                }
                SourceEffect::ForgetAll => {
                    self.scope_memory.remove(&(id.peer.clone(), scope));
                    // Only an explicit user resume clears the source-wide memory (review B-3);
                    // an automatic resume must never lift a source-wide quarantine.
                    self.wide_memory.remove(&id.peer);
                    self.quarantines.remove(&id.peer);
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
            self.wide_memory
                .insert(id.peer.clone(), Remembered::Quarantined);
            let others: Vec<(StableIdentity, Scope)> = self
                .states
                .keys()
                .filter(|(i, s)| i.peer == id.peer && !(i == id && *s == scope))
                .cloned()
                .collect();
            for (oid, s) in others {
                let cur = self.state(&oid, s);
                let (next, effects) = step(cur, SourceEvent::WideQuarantine);
                self.states.insert((oid, s), next);
                out.extend(
                    effects
                        .into_iter()
                        .map(|effect| ScopedEffect { scope: s, effect }),
                );
            }
        }
        out
    }

    /// Drop tracking that can no longer matter (review B-4): a purged `Detached` source with no
    /// memory, and, over the cap, memory for scopes nobody tracks any more.
    fn evict(&mut self, id: &StableIdentity, scope: Scope, next: SourceState) {
        if let SourceState::Detached { purged: true } = next
            && !self.scope_memory.contains_key(&(id.peer.clone(), scope))
        {
            self.states.remove(&(id.clone(), scope));
        }
        if self.scope_memory.len() > MAX_TRACKED_SCOPES {
            let live: std::collections::HashSet<(PeerProof, Scope)> = self
                .states
                .keys()
                .map(|(i, s)| (i.peer.clone(), *s))
                .collect();
            self.scope_memory.retain(|k, _| live.contains(k));
        }
    }

    /// A document or tab went away: forget its machines and memory so the maps do not grow
    /// with every navigation. Source-wide memory is untouched.
    pub fn forget_scope(&mut self, scope: Scope) {
        self.states.retain(|(_, s), _| *s != scope);
        self.scope_memory.retain(|(_, s), _| *s != scope);
    }

    /// Number of tracked (identity, scope) machines and remembered scopes.
    pub fn tracked(&self) -> (usize, usize) {
        (self.states.len(), self.scope_memory.len())
    }

    /// Record a scope quarantine; true when the source-wide limit is reached.
    fn note_quarantine(&mut self, id: &StableIdentity, scope: Scope, now_ms: u64) -> bool {
        let log = self.quarantines.entry(id.peer.clone()).or_default();
        // Count distinct sites (document hosts), not tabs: a hostile site reloading itself
        // is one site (review B-1).
        let key = scope.site_key();
        log.retain(|(s, t)| s.site_key() != key && now_ms.saturating_sub(*t) <= WIDE_WINDOW_MS);
        log.push((scope, now_ms));
        if log.len() > MAX_QUARANTINE_LOG {
            log.remove(0);
        }
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
            site: Some(u64::from(n)),
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

    fn doc(d: u64, site: u64) -> Scope {
        Scope {
            app: AppId {
                connector: ConnectorId(1),
                serial: 1,
            },
            doc: Some(d),
            site: Some(site),
        }
    }

    fn named(name: &str) -> StableIdentity {
        StableIdentity {
            name: name.into(),
            peer: PeerProof::Extension("gorca@ext".into()),
        }
    }

    fn quarantine(b: &mut SourceBook, i: &StableIdentity, s: Scope, t: u64) {
        b.apply(i, s, SourceEvent::Hello { remembered: None }, t);
        for k in 0..3 {
            b.apply(i, s, SourceEvent::Violation { at_ms: t + k }, t + k);
        }
    }

    #[test]
    fn one_site_reloading_never_quarantines_other_tabs() {
        let mut b = SourceBook::new();
        let i = named("bus");
        b.apply(&i, doc(100, 99), SourceEvent::Hello { remembered: None }, 0);
        for (n, d) in [1u64, 2, 3, 4, 5].iter().enumerate() {
            quarantine(&mut b, &i, doc(*d, 7), 1000 * n as u64);
        }
        assert!(b.state(&i, doc(100, 99)).facts_win());
        // A fresh tab on an innocent site is still welcome.
        b.apply(
            &i,
            doc(101, 98),
            SourceEvent::Hello { remembered: None },
            9000,
        );
        assert!(b.state(&i, doc(101, 98)).facts_win());
    }

    #[test]
    fn three_distinct_sites_still_quarantine_the_source() {
        let mut b = SourceBook::new();
        let i = named("bus");
        b.apply(&i, doc(100, 99), SourceEvent::Hello { remembered: None }, 0);
        for d in 1..=3u64 {
            quarantine(&mut b, &i, doc(d, d), d * 10);
        }
        assert_eq!(b.state(&i, doc(100, 99)), SourceState::Quarantined);
    }

    #[test]
    fn renaming_does_not_escape_quarantine() {
        let mut b = SourceBook::new();
        quarantine(&mut b, &named("bus"), doc(1, 1), 0);
        b.apply(
            &named("bus2"),
            doc(1, 1),
            SourceEvent::Hello { remembered: None },
            10,
        );
        assert_eq!(b.state(&named("bus2"), doc(1, 1)), SourceState::Quarantined);
    }

    #[test]
    fn a_stronger_memory_of_any_name_wins() {
        let mut b = SourceBook::new();
        let (a, c) = (named("a"), named("c"));
        b.apply(&a, doc(1, 1), SourceEvent::Hello { remembered: None }, 0);
        b.apply(&a, doc(1, 1), SourceEvent::PrivacyPause, 1);
        quarantine(&mut b, &c, doc(1, 1), 2);
        b.apply(&c, doc(1, 1), SourceEvent::UserRevoke, 5);
        assert_eq!(
            b.remembered(&named("zzz"), doc(1, 1)),
            Some(Remembered::Revoked)
        );
    }

    #[test]
    fn user_resume_clears_the_source_wide_memory() {
        let mut b = SourceBook::new();
        let i = named("bus");
        for d in 1..=3u64 {
            quarantine(&mut b, &i, doc(d, d), d * 10);
        }
        b.apply(&i, doc(1, 1), SourceEvent::Resume { by_user: true }, 100);
        b.apply(&i, doc(1, 1), SourceEvent::TransportClosed, 110);
        b.apply(&i, doc(1, 1), SourceEvent::Hello { remembered: None }, 120);
        assert!(b.state(&i, doc(1, 1)).facts_win());
        assert!(b.remembered(&i, doc(50, 50)).is_none());
    }

    #[test]
    fn privacy_pause_end_does_not_lift_a_wide_quarantine() {
        let mut b = SourceBook::new();
        let i = named("bus");
        let paused = doc(50, 50);
        b.apply(&i, paused, SourceEvent::Hello { remembered: None }, 0);
        b.apply(&i, paused, SourceEvent::PrivacyPause, 1);
        for d in 1..=3u64 {
            quarantine(&mut b, &i, doc(d, d), d * 10);
        }
        // The wide quarantine moved the paused scope to Quarantined; the pause end is automatic.
        b.apply(&i, paused, SourceEvent::Resume { by_user: false }, 100);
        assert_eq!(b.state(&i, paused), SourceState::Quarantined);
        // Even a machine that was Paused when the memory appeared is not reactivated.
        let late = doc(60, 60);
        b.apply(&i, late, SourceEvent::PrivacyPause, 101);
        b.apply(&i, late, SourceEvent::Resume { by_user: false }, 102);
        assert!(!b.state(&i, late).facts_win());
        assert_eq!(b.remembered(&i, doc(70, 70)), Some(Remembered::Quarantined));
        // The explicit user resume does lift it.
        b.apply(&i, paused, SourceEvent::Resume { by_user: true }, 200);
        assert!(b.state(&i, paused).facts_win());
        assert!(b.remembered(&i, doc(70, 70)).is_none());
    }

    #[test]
    fn scopes_without_a_site_are_not_collapsed() {
        let nosite = |d: u64| Scope {
            site: None,
            ..doc(d, 0)
        };
        let mut b = SourceBook::new();
        let i = named("bus");
        b.apply(&i, nosite(100), SourceEvent::Hello { remembered: None }, 0);
        for d in 1..=3u64 {
            quarantine(&mut b, &i, nosite(d), d * 10);
        }
        // Three distinct documents without site info count as three keys, not one.
        assert_eq!(b.state(&i, nosite(100)), SourceState::Quarantined);
        // Yet reloading the *same* document scope repeatedly is still one key.
        let mut b = SourceBook::new();
        for k in 0..5u64 {
            quarantine(&mut b, &i, nosite(1), k * 100);
        }
        b.apply(
            &i,
            nosite(100),
            SourceEvent::Hello { remembered: None },
            9999,
        );
        assert!(b.state(&i, nosite(100)).facts_win());
    }

    #[test]
    fn tracking_is_bounded_across_navigations() {
        let mut b = SourceBook::new();
        let i = named("bus");
        for d in 0..2000u64 {
            let s = doc(d, d);
            b.apply(&i, s, SourceEvent::Hello { remembered: None }, d);
            b.apply(&i, s, SourceEvent::TransportClosed, d);
            b.apply(&i, s, SourceEvent::DetachExpired, d);
        }
        assert_eq!(b.tracked().0, 0, "purged detached machines are evicted");
        for d in 0..50u64 {
            let s = doc(d, d);
            b.apply(&i, s, SourceEvent::Hello { remembered: None }, d);
            b.apply(&i, s, SourceEvent::PrivacyPause, d);
            b.forget_scope(s);
        }
        assert_eq!(b.tracked(), (0, 0));
        for k in 0..300 {
            quarantine(&mut b, &i, doc(10_000 + k, 10_000 + k), 1_000_000 * k);
        }
        assert!(
            b.quarantines
                .values()
                .all(|v| v.len() <= MAX_QUARANTINE_LOG)
        );
    }
}
