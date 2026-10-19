// SPDX-License-Identifier: GPL-3.0-or-later
//! The published profile snapshot (ADR-0016 section 2.4): one immutable value that every
//! resolver reads, replaced as a whole through an [`ArcSwap`].
//!
//! The Reader is the single writer (INV-P4). Every mutation builds a new snapshot from the old
//! one (`Arc`s are shared, so this is a handful of pointer copies) with `generation` bumped; readers
//! that hold an older snapshot are unaffected. Profile identity inside the snapshot is the
//! string id of the file or row, which survives reloads; the numeric [`ProfileId`] is valid only
//! for one [`ProfileSet`] and appears in [`ProfileKey`] together with that set's epoch.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::hash::Hash;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, PoisonError, RwLock};

use arc_swap::ArcSwap;

use crate::patch::{SettingsPatch, Value};
use crate::settings::Settings;

use super::compile::{ProfileId, ProfileSet};
use super::file::{ExtSchema, KeyRef, Profile};

/// A set of match results, interned (TR-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct MatchSetId(pub u32);

/// An interned pin choice.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct PinId(pub u32);

/// What a pin selects, in terms of the current [`ProfileSet`].
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum PinSel {
    Profile(ProfileId),
    /// No user profile and no row for this app (PS-1 "global only").
    GlobalOnly,
}

/// TR-3: the cache key of an effective snapshot. Valid for the interner epoch it was made in.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub struct ProfileKey {
    pub epoch: u32,
    pub rows: MatchSetId,
    pub user: MatchSetId,
    pub pin: Option<PinId>,
}

static NEXT_EPOCH: AtomicU32 = AtomicU32::new(1);

#[derive(Debug, Default)]
struct InternerInner {
    sets: Vec<Arc<[ProfileId]>>,
    set_ids: HashMap<Arc<[ProfileId]>, u32>,
    pins: Vec<PinSel>,
    pin_ids: HashMap<PinSel, u32>,
}

/// Append-only interner for match sets and pins. Shared by every snapshot built on one
/// [`ProfileSet`]; interning is a short write lock off the key path (prewarm) or on a miss.
#[derive(Debug)]
pub struct Interner {
    epoch: u32,
    inner: RwLock<InternerInner>,
}

impl Default for Interner {
    fn default() -> Self {
        Self::new()
    }
}

impl Interner {
    pub fn new() -> Self {
        let mut inner = InternerInner::default();
        // Id 0 is the empty set, so `MatchSetId(0)` is always "no profiles".
        let empty: Arc<[ProfileId]> = Arc::from(Vec::new());
        inner.set_ids.insert(empty.clone(), 0);
        inner.sets.push(empty);
        Interner {
            epoch: NEXT_EPOCH.fetch_add(1, Ordering::Relaxed),
            inner: RwLock::new(inner),
        }
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn intern_set(&self, ids: &[ProfileId]) -> MatchSetId {
        {
            let r = self.inner.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(i) = r.set_ids.get(ids) {
                return MatchSetId(*i);
            }
        }
        let mut w = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = w.set_ids.get(ids) {
            return MatchSetId(*i);
        }
        let arc: Arc<[ProfileId]> = Arc::from(ids.to_vec());
        let i = u32::try_from(w.sets.len()).unwrap_or(u32::MAX);
        w.sets.push(arc.clone());
        w.set_ids.insert(arc, i);
        MatchSetId(i)
    }

    pub fn intern_pin(&self, p: PinSel) -> PinId {
        let mut w = self.inner.write().unwrap_or_else(PoisonError::into_inner);
        if let Some(i) = w.pin_ids.get(&p) {
            return PinId(*i);
        }
        let i = u32::try_from(w.pins.len()).unwrap_or(u32::MAX);
        w.pins.push(p);
        w.pin_ids.insert(p, i);
        PinId(i)
    }

    pub fn set(&self, id: MatchSetId) -> Arc<[ProfileId]> {
        let r = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        r.sets
            .get(usize::try_from(id.0).unwrap_or(usize::MAX))
            .cloned()
            .unwrap_or_else(|| Arc::from(Vec::new()))
    }

    pub fn pin(&self, id: PinId) -> Option<PinSel> {
        let r = self.inner.read().unwrap_or_else(PoisonError::into_inner);
        r.pins
            .get(usize::try_from(id.0).unwrap_or(usize::MAX))
            .copied()
    }

    /// Number of interned sets (tests, metrics).
    pub fn sets_len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .sets
            .len()
    }
}

/// Application identity for session state: the name, ASCII-lowercased.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct AppKey(String);

impl AppKey {
    pub fn of(app_name: &str) -> Self {
        AppKey(app_name.to_ascii_lowercase())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A session pin for one application (PS-1). Absent = automatic selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Pin {
    /// Exactly this user profile (by id), triggered or manual.
    Profile(String),
    /// No user profile and no row for this application.
    GlobalOnly,
}

/// Where edits for an application go (PE-2). Absent = the application's profile.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum EditTargetKind {
    App,
    /// The profile of the focused document's host; only inside a document.
    Site,
    Global,
}

impl EditTargetKind {
    /// "this application" -> "this site" (only in a document) -> "global" -> ...
    #[must_use]
    pub fn next(self, in_document: bool) -> Self {
        match self {
            EditTargetKind::App if in_document => EditTargetKind::Site,
            EditTargetKind::App | EditTargetKind::Site => EditTargetKind::Global,
            EditTargetKind::Global => EditTargetKind::App,
        }
    }
}

/// Session pins and edit targets, per application.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Pins(pub BTreeMap<AppKey, Pin>);

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditTargets(pub BTreeMap<AppKey, EditTargetKind>);

/// How far profiles apply (PC-2, ADR-0016 section 2.1 columns "safe mode" and "suspend").
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum ProfileMode {
    /// Every layer.
    Active,
    /// `profiles.suspend` or an own mode: L0, L1, L2, product rows (L4), L5.
    Suspended,
    /// Safe mode: L0, L1, product rows (L4), L5.
    Disabled,
}

/// Unsaved edits of one writable layer, folded directly on top of that layer's file values.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LayerDelta {
    pub core: SettingsPatch,
    /// `ext.<id>.<key>` values.
    pub ext: BTreeMap<String, Value>,
    /// Profiles only: `enabled` switched by `profiles.toggle_enabled`.
    pub enabled: Option<bool>,
}

impl LayerDelta {
    pub fn is_empty(&self) -> bool {
        self.core.is_empty() && self.ext.is_empty() && self.enabled.is_none()
    }

    pub fn holds(&self, key: &KeyRef) -> bool {
        match key {
            KeyRef::Core(k) => self.core.contains(*k),
            KeyRef::Ext(p) => self.ext.contains_key(p),
        }
    }

    pub fn get(&self, key: &KeyRef) -> Option<Value> {
        match key {
            KeyRef::Core(k) => self.core.get(*k).cloned(),
            KeyRef::Ext(p) => self.ext.get(p).cloned(),
        }
    }

    pub fn set(&mut self, key: &KeyRef, v: Value) -> bool {
        match key {
            KeyRef::Core(k) => self.core.set(*k, v),
            KeyRef::Ext(p) => {
                self.ext.insert(p.clone(), v);
                true
            }
        }
    }

    pub fn remove(&mut self, key: &KeyRef) {
        match key {
            KeyRef::Core(k) => {
                self.core.remove(*k);
            }
            KeyRef::Ext(p) => {
                self.ext.remove(p);
            }
        }
    }
}

/// All unsaved edits: one delta per writable layer (L5 and each L6 profile), the profiles created
/// by an edit and not yet saved, and the touched sets (SG-1).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Deltas {
    pub base: LayerDelta,
    pub profiles: BTreeMap<String, LayerDelta>,
    /// Auto-created profiles that have no file yet (written by the next save).
    pub created: BTreeMap<String, Arc<Profile>>,
    /// Keys changed by command in this session, per edit target profile (SG-1).
    pub touched: BTreeMap<String, BTreeSet<KeyRef>>,
}

impl Deltas {
    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
            && self.profiles.values().all(LayerDelta::is_empty)
            && self.created.is_empty()
    }
}

/// The file layers under the deltas.
#[derive(Clone, Debug)]
pub struct UserBase {
    /// Keys the global file sets (valid ones), as loaded.
    pub file: SettingsPatch,
    /// `ext.<id>.<key>` values the global file sets for installed extensions.
    pub ext: BTreeMap<String, Value>,
    /// FNV-1a of the global file text as loaded (of the empty text when missing).
    pub hash: u64,
}

impl Default for UserBase {
    fn default() -> Self {
        UserBase {
            file: SettingsPatch::new(),
            ext: BTreeMap::new(),
            hash: crate::diag::fnv1a(b""),
        }
    }
}

/// Everything below the profiles: defaults, the preset's defaults, extension defaults, the
/// user's global file and the installed extensions' schemas. Replaced on a reload.
#[derive(Clone, Debug, Default)]
pub struct BaseLayers {
    /// L0 and L1: `Settings::default()` with the preset's keyboard-implied defaults.
    pub defaults: Settings,
    /// L2: `ext.<id>.<key>` defaults of installed extensions.
    pub ext_defaults: BTreeMap<String, Value>,
    pub user: UserBase,
    pub ext_schema: ExtSchema,
}

/// The published snapshot.
#[derive(Clone, Debug)]
pub struct ProfileSnapshot {
    /// Bumps on any field change.
    pub generation: u64,
    pub set: Arc<ProfileSet>,
    pub base: Arc<BaseLayers>,
    pub deltas: Arc<Deltas>,
    pub pins: Arc<Pins>,
    pub targets: Arc<EditTargets>,
    pub mode: ProfileMode,
    pub(crate) interner: Arc<Interner>,
}

impl ProfileSnapshot {
    pub fn new(set: Arc<ProfileSet>, base: Arc<BaseLayers>) -> Self {
        ProfileSnapshot {
            generation: 1,
            set,
            base,
            deltas: Arc::new(Deltas::default()),
            pins: Arc::new(Pins::default()),
            targets: Arc::new(EditTargets::default()),
            mode: ProfileMode::Active,
            interner: Arc::new(Interner::new()),
        }
    }

    pub fn interner(&self) -> &Arc<Interner> {
        &self.interner
    }

    pub(crate) fn bumped(&self) -> ProfileSnapshot {
        let mut s = self.clone();
        s.generation += 1;
        s
    }

    #[must_use]
    pub fn with_mode(&self, mode: ProfileMode) -> ProfileSnapshot {
        if self.mode == mode {
            return self.clone();
        }
        let mut s = self.bumped();
        s.mode = mode;
        s
    }

    #[must_use]
    pub fn with_pin(&self, app: &AppKey, pin: Option<Pin>) -> ProfileSnapshot {
        let mut pins = (*self.pins).clone();
        match pin {
            Some(p) => {
                pins.0.insert(app.clone(), p);
            }
            None => {
                pins.0.remove(app);
            }
        }
        let mut s = self.bumped();
        s.pins = Arc::new(pins);
        s
    }

    #[must_use]
    pub fn with_target(&self, app: &AppKey, target: EditTargetKind) -> ProfileSnapshot {
        let mut t = (*self.targets).clone();
        if target == EditTargetKind::App {
            t.0.remove(app);
        } else {
            t.0.insert(app.clone(), target);
        }
        let mut s = self.bumped();
        s.targets = Arc::new(t);
        s
    }

    pub fn target_of(&self, app: &AppKey) -> EditTargetKind {
        self.targets
            .0
            .get(app)
            .copied()
            .unwrap_or(EditTargetKind::App)
    }

    pub fn pin_of(&self, app: &AppKey) -> Option<&Pin> {
        self.pins.0.get(app)
    }

    /// Replace the compiled set and base layers after a reload (HR-4 per layer): unsaved edits
    /// of keys whose file value changed are dropped, edits of profiles that disappeared are
    /// dropped, unsaved created profiles are kept, touched sets lose changed keys (SG-1).
    #[must_use]
    pub fn reload(&self, set: Arc<ProfileSet>, base: Arc<BaseLayers>) -> ProfileSnapshot {
        let mut d = (*self.deltas).clone();
        // L5.
        let old_s = self.base.user.file.apply(&self.base.defaults);
        let new_s = base.user.file.apply(&base.defaults);
        d.base.core.drop_changed(&old_s, &new_s);
        d.base
            .ext
            .retain(|p, _| self.base.user.ext.get(p) == base.user.ext.get(p));
        // L6, per profile.
        let mut gone: Vec<String> = Vec::new();
        for (id, delta) in &mut d.profiles {
            let new_p = set.id_of(id).and_then(|i| set.get(i)).cloned();
            let old_p = self.set.id_of(id).and_then(|i| self.set.get(i)).cloned();
            match (old_p, new_p) {
                (Some(o), Some(n)) => {
                    let (os, ns) = (
                        o.values.apply(&base.defaults),
                        n.values.apply(&base.defaults),
                    );
                    delta.core.drop_changed(&os, &ns);
                    delta.ext.retain(|p, _| o.ext.get(p) == n.ext.get(p));
                    if o.enabled != n.enabled {
                        delta.enabled = None;
                    }
                    if let Some(t) = d.touched.get_mut(id) {
                        t.retain(|k| !changed(&o, &n, k, &base.defaults));
                    }
                }
                (None, Some(_)) => {}
                (Some(_) | None, None) => {
                    if !d.created.contains_key(id) {
                        gone.push(id.clone());
                    }
                }
            }
        }
        for id in gone {
            d.profiles.remove(&id);
            d.touched.remove(&id);
        }
        // Re-add the unsaved created profiles to the new set.
        let mut set = set;
        for (id, p) in &d.created {
            if set.id_of(id).is_none() {
                let (s2, _) = set.with_profile(p.clone());
                set = Arc::new(s2);
            }
        }
        let mut s = self.bumped();
        s.set = set;
        s.base = base;
        s.deltas = Arc::new(d);
        s.interner = Arc::new(Interner::new());
        s
    }

    /// Revert: clear every unsaved edit and the profiles they created.
    #[must_use]
    pub fn revert(&self) -> ProfileSnapshot {
        let mut set = self.set.clone();
        for id in self.deltas.created.keys() {
            let (s2, _) = set.without(id);
            set = Arc::new(s2);
        }
        let mut s = self.bumped();
        let changed = !Arc::ptr_eq(&set, &self.set);
        s.set = set;
        s.deltas = Arc::new(Deltas::default());
        if changed {
            s.interner = Arc::new(Interner::new());
        }
        s
    }
}

/// Did the file value of `key` differ between the old and the new parse of a profile?
fn changed(o: &Profile, n: &Profile, key: &KeyRef, defaults: &Settings) -> bool {
    match key {
        KeyRef::Core(k) => {
            crate::patch::get_value(&o.values.apply(defaults), *k)
                != crate::patch::get_value(&n.values.apply(defaults), *k)
        }
        KeyRef::Ext(p) => o.ext.get(p) != n.ext.get(p),
    }
}

/// The cell the Reader publishes the snapshot through (lock-free reads).
#[derive(Debug)]
pub struct ProfileCell {
    cell: ArcSwap<ProfileSnapshot>,
}

impl ProfileCell {
    pub fn new(snapshot: ProfileSnapshot) -> Self {
        ProfileCell {
            cell: ArcSwap::from_pointee(snapshot),
        }
    }

    /// The current snapshot.
    pub fn load(&self) -> Arc<ProfileSnapshot> {
        self.cell.load_full()
    }

    /// Publish a new snapshot (single writer: the Reader).
    pub fn store(&self, snapshot: ProfileSnapshot) {
        self.cell.store(Arc::new(snapshot));
    }
}

/// A small LRU map (ADR-0016 SW-1: 64 entries of `ProfileKey` to effective settings), hand
/// rolled on std. Eviction scans the entries, which is fine at this size.
#[derive(Debug)]
pub struct LruMap<K, V> {
    cap: usize,
    tick: u64,
    map: HashMap<K, (V, u64)>,
}

impl<K: Eq + Hash + Clone, V> LruMap<K, V> {
    pub fn new(cap: usize) -> Self {
        LruMap {
            cap: cap.max(1),
            tick: 0,
            map: HashMap::with_capacity(cap.max(1)),
        }
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn clear(&mut self) {
        self.map.clear();
    }

    pub fn get(&mut self, k: &K) -> Option<&V> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = t;
            &e.0
        })
    }

    pub fn contains(&self, k: &K) -> bool {
        self.map.contains_key(k)
    }

    pub fn insert(&mut self, k: K, v: V) {
        self.tick += 1;
        if !self.map.contains_key(&k)
            && self.map.len() >= self.cap
            && let Some(oldest) = self
                .map
                .iter()
                .min_by_key(|(_, e)| e.1)
                .map(|(key, _)| key.clone())
        {
            self.map.remove(&oldest);
        }
        self.map.insert(k, (v, self.tick));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interner_is_stable_and_shared() {
        let i = Interner::new();
        let a = i.intern_set(&[ProfileId(1), ProfileId(2)]);
        let b = i.intern_set(&[ProfileId(1), ProfileId(2)]);
        let c = i.intern_set(&[ProfileId(2), ProfileId(1)]);
        assert_eq!(a, b);
        assert_ne!(a, c, "order is part of the set (fold order)");
        assert_eq!(i.intern_set(&[]), MatchSetId(0));
        assert_eq!(&*i.set(a), &[ProfileId(1), ProfileId(2)]);
        let p = i.intern_pin(PinSel::GlobalOnly);
        assert_eq!(i.intern_pin(PinSel::GlobalOnly), p);
        assert_eq!(i.pin(p), Some(PinSel::GlobalOnly));
        assert!(
            i.set(MatchSetId(99)).is_empty(),
            "unknown id is the empty set"
        );
        assert_ne!(Interner::new().epoch(), i.epoch());
    }

    #[test]
    fn lru_evicts_the_least_recently_used() {
        let mut l: LruMap<u32, u32> = LruMap::new(3);
        l.insert(1, 10);
        l.insert(2, 20);
        l.insert(3, 30);
        assert_eq!(l.get(&1), Some(&10));
        l.insert(4, 40);
        assert!(!l.contains(&2), "2 was the oldest");
        assert!(l.contains(&1) && l.contains(&3) && l.contains(&4));
        assert_eq!(l.len(), 3);
        l.insert(3, 31);
        assert_eq!(l.get(&3), Some(&31));
        assert_eq!(l.len(), 3);
        l.clear();
        assert!(l.is_empty());
    }

    #[test]
    fn lru_of_one_still_works() {
        let mut l: LruMap<u8, u8> = LruMap::new(0);
        l.insert(1, 1);
        l.insert(2, 2);
        assert_eq!(l.len(), 1);
        assert_eq!(l.get(&2), Some(&2));
    }

    #[test]
    fn edit_target_cycle() {
        use EditTargetKind::{App, Global, Site};
        assert_eq!(App.next(true), Site);
        assert_eq!(Site.next(true), Global);
        assert_eq!(Global.next(true), App);
        assert_eq!(App.next(false), Global, "no site outside a document");
        assert_eq!(Site.next(false), Global);
    }

    #[test]
    fn the_cell_publishes_whole_snapshots() {
        let snap = ProfileSnapshot::new(
            Arc::new(ProfileSet::empty()),
            Arc::new(BaseLayers::default()),
        );
        let cell = ProfileCell::new(snap.clone());
        let before = cell.load();
        cell.store(snap.with_mode(ProfileMode::Suspended));
        let after = cell.load();
        assert_eq!(
            before.mode,
            ProfileMode::Active,
            "an old reader keeps its snapshot"
        );
        assert_eq!(after.mode, ProfileMode::Suspended);
        assert!(after.generation > before.generation);
        // Setting the same mode does not bump generation.
        assert_eq!(
            after.with_mode(ProfileMode::Suspended).generation,
            after.generation
        );
    }
}
