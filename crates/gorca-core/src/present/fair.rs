// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-owner fairness for the presenters that speak or read on an application's say-so
//! (announcements, notification banners, window activations, M4b).
//!
//! One token bucket per (owner, kind), refilled continuously, so the state of an owner is a few
//! words and the table can hold thousands of them:
//!
//! - an **owner** is a process (`directory::Owner`), never a bus name, an object path, a role or
//!   a notification source, so an application cannot spend more by varying those;
//! - a flooding owner empties only its own buckets; every other owner keeps its own;
//! - a new owner takes a free slot, or an **idle** one (all its buckets full, nothing waiting):
//!   evicting that forgets nothing, because a full bucket is what a new owner starts with. A busy
//!   owner is never evicted, so creating owners cannot reset anybody's budget;
//! - when [`TABLE_MAX`] owners are busy at once, a newcomer falls into one of [`OVERFLOW`]
//!   shared buckets chosen by a hash of the owner: starving a victim then needs [`TABLE_MAX`]
//!   live busy processes and a hash collision, and the table never grows.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gorca_model::{NodeKey, Prio, Utterance};
use smallvec::SmallVec;

use crate::directory::Owner;

/// Owners tracked individually.
pub const TABLE_MAX: usize = 4096;
/// Shared buckets for owners that find the table full of busy ones.
pub const OVERFLOW: usize = 64;
/// Spoken notices and announcements per second over all owners together (burst and rate). It
/// is far above what one owner may spend, so one flooder never reaches it; only many
/// principals flooding at once do, and then the ceiling, not the speech lane, is what drops.
pub const GLOBAL_SPEAK_BURST: f32 = 16.0;
/// Owners with an utterance waiting for their speech budget (bounded memory for waiting text).
pub const WAITING_MAX: usize = 64;
/// Nodes whose read was asked recently, per owner.
pub const ASKED_MAX: usize = 8;

/// What is being limited.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Kind {
    /// An announcement or a notice spoken.
    Speak,
    /// A node read asked of the application.
    Read,
    /// A window activation handled (title read and spoken).
    Activate,
    /// A node read asked for the verified Shell, which maps dozens of actors for one banner and
    /// whose notice is among them: a larger budget than [`Kind::Read`], for a trusted app.
    Probe,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Speak, Kind::Read, Kind::Activate, Kind::Probe];

    fn index(self) -> usize {
        match self {
            Kind::Speak => 0,
            Kind::Read => 1,
            Kind::Activate => 2,
            Kind::Probe => 3,
        }
    }

    /// `(burst, tokens per second)`.
    fn limits(self) -> (f32, f32) {
        match self {
            Kind::Speak => (3.0, 3.0),
            Kind::Read => (8.0, 8.0),
            Kind::Activate => (4.0, 4.0),
            Kind::Probe => (64.0, 64.0),
        }
    }
}

#[derive(Copy, Clone, Debug)]
struct Bucket {
    tokens: f32,
    at: Instant,
}

impl Bucket {
    fn full(kind: Kind, now: Instant) -> Self {
        Self {
            tokens: kind.limits().0,
            at: now,
        }
    }

    fn refill(&mut self, kind: Kind, now: Instant) {
        let (burst, rate) = kind.limits();
        let dt = now.saturating_duration_since(self.at).as_secs_f32();
        self.tokens = (self.tokens + dt * rate).min(burst);
        self.at = now;
    }

    fn take(&mut self, kind: Kind, now: Instant) -> bool {
        self.refill(kind, now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn is_full(&mut self, kind: Kind, now: Instant) -> bool {
        self.refill(kind, now);
        self.tokens >= kind.limits().0
    }

    /// When one token is available.
    fn ready_at(&self, kind: Kind) -> Instant {
        let (_, rate) = kind.limits();
        let missing = (1.0 - self.tokens).max(0.0);
        self.at + Duration::from_secs_f32(missing / rate)
    }
}

/// The state of one owner.
#[derive(Debug)]
pub struct Entry {
    buckets: [Bucket; 4],
    /// The last announcement text hash, to speak a repeat once.
    pub last_announcement: Option<(u64, Instant)>,
    /// Nodes whose read was asked recently (a banner re-shows while it animates).
    pub asked: SmallVec<[(NodeKey, Instant); ASKED_MAX]>,
    /// The newest ordinary (notification-class) utterance over the speech budget (drop-oldest).
    pub waiting: Option<Utterance>,
    /// The newest urgent one (alert, message-class or above). Never replaced by an ordinary
    /// utterance, and spoken first (R-3).
    pub waiting_urgent: Option<Utterance>,
}

impl Entry {
    fn new(now: Instant) -> Self {
        Self {
            buckets: Kind::ALL.map(|k| Bucket::full(k, now)),
            last_announcement: None,
            asked: SmallVec::new(),
            waiting: None,
            waiting_urgent: None,
        }
    }

    pub fn take(&mut self, kind: Kind, now: Instant) -> bool {
        self.buckets[kind.index()].take(kind, now)
    }

    /// When the next token of `kind` is available.
    pub fn ready_at(&self, kind: Kind) -> Instant {
        self.buckets[kind.index()].ready_at(kind)
    }

    fn idle(&mut self, now: Instant) -> bool {
        self.waiting.is_none()
            && self.waiting_urgent.is_none()
            && self
                .buckets
                .iter_mut()
                .zip(Kind::ALL)
                .all(|(b, k)| b.is_full(k, now))
    }
}

/// The budgets of all owners.
#[derive(Debug)]
pub struct Fair {
    /// A ceiling on speech over all owners together, so many principals cannot jointly flood.
    global: Bucket,
    map: HashMap<Owner, Entry>,
    overflow: Vec<Entry>,
    /// Owners that have an utterance waiting, oldest first.
    waiting: Vec<Owner>,
}

fn bucket_of(owner: Owner) -> usize {
    // FNV-1a over the owner's identity; not secret, only spread.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut mix = |v: u64| {
        for b in v.to_le_bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    match owner {
        Owner::Pid(p) => {
            mix(1);
            mix(u64::from(p));
        }
        Owner::Unknown => mix(2),
    }
    // The modulus is below u64::MAX, so the result fits usize.
    usize::try_from(h % OVERFLOW as u64).unwrap_or(0)
}

impl Fair {
    pub fn new(now: Instant) -> Self {
        Self {
            global: Bucket {
                tokens: GLOBAL_SPEAK_BURST,
                at: now,
            },
            map: HashMap::new(),
            overflow: (0..OVERFLOW).map(|_| Entry::new(now)).collect(),
            waiting: Vec::new(),
        }
    }

    /// Owners individually tracked (bounded by [`TABLE_MAX`]).
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// The entry of `owner`, and whether it is a shared overflow bucket (which keeps no waiting
    /// utterance and no per-node memory worth trusting).
    pub fn entry(&mut self, owner: Owner, now: Instant) -> (&mut Entry, bool) {
        if !self.map.contains_key(&owner) {
            if self.map.len() >= TABLE_MAX {
                let idle = self
                    .map
                    .iter_mut()
                    .find_map(|(k, e)| e.idle(now).then_some(*k));
                match idle {
                    Some(k) => {
                        self.map.remove(&k);
                    }
                    None => return (&mut self.overflow[bucket_of(owner)], true),
                }
            }
            self.map.insert(owner, Entry::new(now));
        }
        match self.map.get_mut(&owner) {
            Some(e) => (e, false),
            None => (&mut self.overflow[bucket_of(owner)], true),
        }
    }

    /// Spend a token of `kind` for `owner`.
    pub fn allow(&mut self, owner: Owner, kind: Kind, now: Instant) -> bool {
        if !self.entry(owner, now).0.take(kind, now) {
            return false;
        }
        // Speech also spends the joint ceiling (see [`GLOBAL_SPEAK_BURST`]).
        kind != Kind::Speak || self.global_take(now)
    }

    fn global_take(&mut self, now: Instant) -> bool {
        let dt = now.saturating_duration_since(self.global.at).as_secs_f32();
        self.global.tokens = (self.global.tokens + dt * GLOBAL_SPEAK_BURST).min(GLOBAL_SPEAK_BURST);
        self.global.at = now;
        if self.global.tokens >= 1.0 {
            self.global.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Keep `u` as `owner`'s newest waiting utterance. Returns how many utterances were dropped
    /// by it (0 or 1; the newcomer itself when the owner cannot wait).
    pub fn wait(&mut self, owner: Owner, u: Utterance, now: Instant) -> u64 {
        if self.waiting.len() >= WAITING_MAX && !self.waiting.contains(&owner) {
            return 1;
        }
        let (e, over) = self.entry(owner, now);
        if over {
            return 1;
        }
        let slot = if u.prio < Prio::Notification {
            &mut e.waiting_urgent
        } else {
            &mut e.waiting
        };
        let dropped = u64::from(slot.replace(u).is_some());
        if !self.waiting.contains(&owner) {
            self.waiting.push(owner);
        }
        dropped
    }

    /// When the global ceiling has a token.
    fn global_ready_at(&self) -> Instant {
        let missing = (1.0 - self.global.tokens).max(0.0);
        self.global.at + Duration::from_secs_f32(missing / GLOBAL_SPEAK_BURST)
    }

    /// When the first waiting utterance can be spoken: its owner has a token and so has the
    /// joint ceiling.
    pub fn next_due(&self) -> Option<Instant> {
        let owners = self
            .waiting
            .iter()
            .filter_map(|o| self.map.get(o))
            .map(|e| e.ready_at(Kind::Speak))
            .min()?;
        Some(owners.max(self.global_ready_at()))
    }

    /// Waiting utterances whose owner has a speech token at `now` and that fit under the joint
    /// ceiling (L-1), taken out, oldest owner first; the rest keep waiting.
    pub fn take_due(&mut self, now: Instant) -> Vec<Utterance> {
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for o in std::mem::take(&mut self.waiting) {
            let ready = match self.map.get(&o) {
                Some(e) if e.waiting.is_some() || e.waiting_urgent.is_some() => {
                    e.ready_at(Kind::Speak) <= now
                }
                Some(_) | None => continue,
            };
            if ready && self.global_take(now) {
                if let Some(e) = self.map.get_mut(&o) {
                    e.take(Kind::Speak, now);
                    out.extend(e.waiting_urgent.take().or_else(|| e.waiting.take()));
                    if e.waiting.is_some() || e.waiting_urgent.is_some() {
                        keep.push(o);
                    }
                }
            } else {
                keep.push(o);
            }
        }
        self.waiting = keep;
        out
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
mod tests {
    use super::*;

    fn owner(n: u32) -> Owner {
        Owner::Pid(n)
    }

    #[test]
    fn a_bucket_allows_its_burst_then_one_per_refill() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        let o = owner(1);
        let n = (0..10).filter(|_| f.allow(o, Kind::Speak, t)).count();
        assert_eq!(n, 3);
        assert!(!f.allow(o, Kind::Speak, t + Duration::from_millis(300)));
        assert!(f.allow(o, Kind::Speak, t + Duration::from_millis(340)));
    }

    #[test]
    fn a_flooding_owner_does_not_touch_a_quiet_one() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        for _ in 0..1000 {
            f.allow(owner(1), Kind::Speak, t);
            f.allow(owner(1), Kind::Read, t);
            f.allow(owner(1), Kind::Activate, t);
        }
        assert!(f.allow(owner(2), Kind::Speak, t));
        assert!(f.allow(owner(2), Kind::Read, t));
        assert!(f.allow(owner(2), Kind::Activate, t));
    }

    #[test]
    fn kinds_have_separate_buckets() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        while f.allow(owner(1), Kind::Speak, t) {}
        assert!(f.allow(owner(1), Kind::Read, t));
    }

    #[test]
    fn the_table_is_bounded_and_a_busy_owner_is_never_evicted() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        // The victim spends everything.
        while f.allow(owner(0), Kind::Read, t) {}
        // An attacker fills the table with busy owners and beyond.
        for p in 1..(TABLE_MAX as u32 + 500) {
            f.allow(owner(p), Kind::Read, t);
        }
        assert!(f.len() <= TABLE_MAX);
        assert!(
            !f.allow(owner(0), Kind::Read, t),
            "its budget was not reset by the churn"
        );
    }

    #[test]
    fn idle_owners_are_reclaimed_and_cost_nothing() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        for p in 0..TABLE_MAX as u32 {
            f.allow(owner(p), Kind::Read, t);
        }
        // Long after, every bucket is full again: a newcomer takes a slot and is not shared.
        let later = t + Duration::from_secs(10);
        let (_, over) = f.entry(owner(99_999), later);
        assert!(!over);
        assert!(f.len() <= TABLE_MAX);
    }

    #[test]
    fn newcomers_beyond_a_table_of_busy_owners_share_hashed_buckets_not_one() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        for p in 0..TABLE_MAX as u32 {
            while f.allow(owner(p), Kind::Read, t) {}
        }
        // Newcomers hit the overflow; a quiet newcomer is not starved by a flooding one unless
        // they collide in the same bucket.
        let flood = owner(50_000);
        for _ in 0..100 {
            f.allow(flood, Kind::Read, t);
        }
        let quiet = (50_001..50_200)
            .map(owner)
            .filter(|o| bucket_of(*o) != bucket_of(flood))
            .take(5)
            .filter(|o| f.allow(*o, Kind::Read, t))
            .count();
        assert_eq!(quiet, 5);
    }

    #[test]
    fn an_urgent_utterance_waits_in_its_own_slot_and_is_spoken_first() {
        use gorca_model::{Epoch, Prio, UttId};
        let t = Instant::now();
        let mut f = Fair::new(t);
        let u = |i: u64, prio| Utterance {
            id: UttId(i),
            segments: SmallVec::new(),
            lang: None,
            epoch: Epoch(1),
            prio,
            may_refine: false,
            survives_focus_change: false,
            ttl_secs: 0,
            refines: None,
        };
        let o = owner(1);
        f.wait(o, u(1, Prio::Notification), t);
        f.wait(o, u(2, Prio::Message), t);
        // Later polite news replaces the older polite one, never the alert.
        assert_eq!(f.wait(o, u(3, Prio::Notification), t), 1);
        let later = t + Duration::from_secs(60);
        let first = f.take_due(later);
        assert_eq!(first.iter().map(|x| x.id.0).collect::<Vec<_>>(), [2]);
        let second = f.take_due(later + Duration::from_secs(60));
        assert_eq!(second.iter().map(|x| x.id.0).collect::<Vec<_>>(), [3]);
    }

    #[test]
    fn waiting_keeps_the_newest_and_is_bounded() {
        use gorca_model::{Epoch, Prio, UttId};
        let t = Instant::now();
        let mut f = Fair::new(t);
        let u = |i: u64| Utterance {
            id: UttId(i),
            segments: SmallVec::new(),
            lang: None,
            epoch: Epoch(1),
            prio: Prio::Notification,
            may_refine: false,
            survives_focus_change: false,
            ttl_secs: 0,
            refines: None,
        };
        assert_eq!(f.wait(owner(1), u(1), t), 0);
        assert_eq!(f.wait(owner(1), u(2), t), 1, "the older one is dropped");
        for p in 2..(WAITING_MAX as u32 + 50) {
            f.wait(owner(p), u(100), t);
        }
        assert!(f.waiting.len() <= WAITING_MAX);
        while f.allow(owner(1), Kind::Speak, t) {}
        let due = f.take_due(t + Duration::from_secs(2));
        assert!(due.iter().any(|x| x.id == UttId(2)));
    }

    #[test]
    fn many_principals_together_hit_a_global_ceiling() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        let spoken = (0..500u32)
            .filter(|p| f.allow(owner(*p), Kind::Speak, t))
            .count();
        assert_eq!(spoken, GLOBAL_SPEAK_BURST as usize, "joint flood is capped");
        // A little later the ceiling refills.
        assert!(f.allow(owner(9_999), Kind::Speak, t + Duration::from_secs(1)));
    }

    #[test]
    fn waiting_notices_released_together_respect_the_global_ceiling() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        let waiting = |i: u64| Utterance {
            id: gorca_model::UttId(i),
            segments: smallvec::SmallVec::new(),
            lang: None,
            epoch: gorca_model::Epoch(1),
            prio: gorca_model::Prio::Notification,
            may_refine: false,
            survives_focus_change: true,
            ttl_secs: 0,
            refines: None,
        };
        for o in 0..64u32 {
            while f.allow(owner(o), Kind::Speak, t) {}
            f.wait(owner(o), waiting(u64::from(o)), t);
        }
        let later = t + Duration::from_millis(400);
        let out = f.take_due(later);
        assert!(out.len() <= 16, "{}", out.len());
        // The rest is still waiting, due after the ceiling refills, and drains over time.
        let due = f.next_due().expect("some are still waiting");
        assert!(due > later);
        let mut total = out.len();
        let mut now = later;
        for _ in 0..200 {
            now += Duration::from_millis(100);
            total += f.take_due(now).len();
        }
        assert_eq!(total, 64, "nothing is lost, only paced");
    }
}

#[cfg(test)]
mod m4b_review_probe {
    use super::*;
    use gorca_model::{Epoch, Prio, UttId};

    fn u(n: u64) -> Utterance {
        Utterance {
            id: UttId(n),
            segments: smallvec::SmallVec::new(),
            lang: None,
            epoch: Epoch(1),
            prio: Prio::Notification,
            may_refine: false,
            survives_focus_change: true,
            ttl_secs: 0,
            refines: None,
        }
    }

    /// P4: waiting utterances released by `take_due` must respect the global ceiling.
    #[test]
    #[allow(clippy::cast_precision_loss)]
    fn probe_p4_take_due_respects_the_global_ceiling() {
        let t = Instant::now();
        let mut f = Fair::new(t);
        for o in 0..64u32 {
            while f.allow(Owner::Pid(o), Kind::Speak, t) {}
            f.wait(Owner::Pid(o), u(u64::from(o)), t);
        }
        let out = f.take_due(t + Duration::from_millis(400));
        assert!(
            out.len() as f32 <= GLOBAL_SPEAK_BURST,
            "{} waiting notices released at once, ceiling {}",
            out.len(),
            GLOBAL_SPEAK_BURST
        );
    }
}
