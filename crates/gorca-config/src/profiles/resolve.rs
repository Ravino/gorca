// SPDX-License-Identifier: GPL-3.0-or-later
//! Resolvers over one snapshot (ADR-0016 section 2.4, SS-1): the profile key of a focus and the
//! fold of the layer stack into one [`Effective`] value.
//!
//! Everything here is a pure function of the snapshot (INV-P1): the same `generation` and the same key
//! or facts give the same answer, whichever actor asks.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::patch::{KeyId, SettingsPatch, Value, get_value, is_default, set_value};
use crate::settings::{KeyScope, SETTINGS, Settings, TerminalSettings};

use super::compile::{AppCandidates, ProfileId};
use super::file::{KeyRef, Profile, ProfileSource, VOLUME_FLOOR};
use super::snapshot::{Pin, PinSel, ProfileKey, ProfileMode, ProfileSnapshot};
use super::trigger::{AppFacts, FocusFacts, SiteFacts};

/// Which layer supplied a value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayerRef {
    Default,
    Preset,
    ExtDefault(Arc<str>),
    ThirdParty(ProfileId),
    /// A product row or a row of a first-party extension (L4).
    FirstParty(ProfileId),
    UserBase,
    User(ProfileId),
}

/// Presentation flags compiled once per effective snapshot (M22d consumes; no `report.*` keys
/// exist yet, so the value is 0).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct ReportFlags(pub u64);

/// An extension key's value and the layer it came from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExtEntry {
    pub value: Value,
    pub origin: LayerRef,
}

/// The effective settings for one profile key: precomputed, immutable, shared as an `Arc`.
#[derive(Clone, Debug)]
pub struct Effective {
    /// `generation` of the snapshot it was folded from.
    pub generation: u64,
    pub settings: Settings,
    /// Keys `ext.<id>.<key>` by dotted path.
    pub ext: BTreeMap<String, ExtEntry>,
    /// Per [`KeyId`]: which layer supplied the value (PE-1, explain).
    pub origin: Box<[LayerRef]>,
    pub report: ReportFlags,
    /// Keys where a user-base entry equal to the default was skipped although a row set the key
    /// (UB-1): the row wins, and `settings.explain_current` says so.
    pub ub1_skipped: Box<[KeyRef]>,
}

impl Effective {
    pub fn value(&self, key: &KeyRef) -> Option<Value> {
        match key {
            KeyRef::Core(k) => Some(get_value(&self.settings, *k)),
            KeyRef::Ext(p) => self.ext.get(p).map(|e| e.value.clone()),
        }
    }

    pub fn origin_of(&self, key: &KeyRef) -> Option<LayerRef> {
        match key {
            KeyRef::Core(k) => self.origin.get(k.index()).cloned(),
            KeyRef::Ext(p) => self.ext.get(p).map(|e| e.origin.clone()),
        }
    }
}

/// Facts about the focused document that Doc keys depend on.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct DocFacts<'a> {
    pub site: Option<SiteFacts<'a>>,
}

/// Keys resolved per application (`KeyScope::App`).
#[derive(Clone, Debug)]
pub struct AppScoped {
    eff: Effective,
}

impl AppScoped {
    /// An `App` key; `None` for keys of another scope (use `resolve_doc` or `resolve_focus`).
    pub fn get(&self, key: KeyId) -> Option<Value> {
        (key.def().scope == KeyScope::App).then(|| get_value(&self.eff.settings, key))
    }

    pub fn terminal(&self) -> &TerminalSettings {
        &self.eff.settings.terminal
    }

    /// `browse.enabled`: false means no virtual buffer for the application (the CHR-0 `Off`).
    pub fn browse_enabled(&self) -> bool {
        self.eff.settings.browse.enabled
    }

    pub fn effective(&self) -> &Effective {
        &self.eff
    }
}

/// Keys resolved per document (`KeyScope::Doc`, and the `App` keys beneath).
#[derive(Clone, Debug)]
pub struct DocScoped {
    eff: Effective,
}

impl DocScoped {
    pub fn get(&self, key: KeyId) -> Option<Value> {
        match key.def().scope {
            KeyScope::App | KeyScope::Doc => Some(get_value(&self.eff.settings, key)),
            KeyScope::Base | KeyScope::Focus => None,
        }
    }

    pub fn say_all_on_load(&self) -> bool {
        self.eff.settings.browse.say_all_on_load
    }

    pub fn effective(&self) -> &Effective {
        &self.eff
    }
}

fn ext_id_of(path: &str) -> &str {
    path.strip_prefix("ext.")
        .and_then(|r| r.split_once('.'))
        .map_or("", |(id, _)| id)
}

/// Fold one layer on top of the settings so far.
fn layer(
    cx: &mut Fold,
    patch: &SettingsPatch,
    ext: &BTreeMap<String, Value>,
    lref: &LayerRef,
    clamp: bool,
) {
    for (k, v) in patch.iter() {
        // PC-1: rows and profiles can never push the volume below the floor.
        if clamp
            && k.def().section == "speech"
            && k.def().key == "volume"
            && matches!(v, Value::Int(n) if *n < VOLUME_FLOOR)
        {
            continue;
        }
        if set_value(&mut cx.settings, k, v) {
            cx.origin[k.index()] = lref.clone();
        }
    }
    for (p, v) in ext {
        cx.ext.insert(
            p.clone(),
            ExtEntry {
                value: v.clone(),
                origin: lref.clone(),
            },
        );
    }
}

struct Fold {
    settings: Settings,
    origin: Vec<LayerRef>,
    ext: BTreeMap<String, ExtEntry>,
    ub1: Vec<KeyRef>,
}

impl ProfileSnapshot {
    /// The effective `profiles.rows` map: the global file with the unsaved edit on top.
    pub(crate) fn rows_map(&self) -> BTreeMap<String, bool> {
        let Some(k) = KeyId::of("profiles", "rows") else {
            return BTreeMap::new();
        };
        let v = self
            .deltas
            .base
            .core
            .get(k)
            .or_else(|| self.base.user.file.get(k));
        match v {
            Some(Value::Map(m)) => m.clone(),
            Some(Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::StrList(_)) | None => {
                BTreeMap::new()
            }
        }
    }

    /// Is the profile or row switched on (PF-2, PF-5, `enabled`, the unsaved toggle)?
    pub fn is_enabled(&self, p: &Profile) -> bool {
        match &p.source {
            ProfileSource::User => self.is_enabled_in(p, &BTreeMap::new()),
            ProfileSource::Product
            | ProfileSource::FirstParty { .. }
            | ProfileSource::ThirdParty { .. } => self.is_enabled_in(p, &self.rows_map()),
        }
    }

    /// [`Self::is_enabled`] with the effective `profiles.rows` map computed once by the caller
    /// (F-9: a fold asks for every matching row).
    pub(crate) fn is_enabled_in(&self, p: &Profile, rows: &BTreeMap<String, bool>) -> bool {
        match &p.source {
            ProfileSource::User => self
                .deltas
                .profiles
                .get(&p.id)
                .and_then(|d| d.enabled)
                .unwrap_or(p.enabled),
            ProfileSource::Product | ProfileSource::FirstParty { .. } => {
                rows.get(&p.id).copied().unwrap_or(p.enabled)
            }
            ProfileSource::ThirdParty { .. } => rows.get(&p.id).copied().unwrap_or(false),
        }
    }

    /// The profile key of a focus: the rows and user profiles that match, in fold order, and
    /// the pin for the application (TR-3). `cands` are the application's cached candidates.
    pub fn focus_key(&self, cands: &AppCandidates, facts: &FocusFacts<'_>) -> ProfileKey {
        let matched = self.set.select(cands, facts);
        let mut rows: Vec<ProfileId> = Vec::new();
        let mut user: Vec<ProfileId> = Vec::new();
        for m in &matched {
            let is_user = self
                .set
                .get(m.id)
                .is_some_and(|p| p.source == ProfileSource::User);
            if is_user {
                user.push(m.id);
            } else {
                rows.push(m.id);
            }
        }
        let mut pin = None;
        if let Some(app) = facts.app.as_ref()
            && let Some(p) = self.pins.0.get(&super::snapshot::AppKey::of(app.name))
        {
            match p {
                Pin::GlobalOnly => {
                    rows.clear();
                    user.clear();
                    pin = Some(self.interner.intern_pin(PinSel::GlobalOnly));
                }
                Pin::Profile(id) => {
                    if let Some(pid) = self.set.id_of(id)
                        && self
                            .set
                            .get(pid)
                            .is_some_and(|q| q.source == ProfileSource::User)
                    {
                        user.clear();
                        user.push(pid);
                        pin = Some(self.interner.intern_pin(PinSel::Profile(pid)));
                    }
                }
            }
        }
        ProfileKey {
            epoch: self.interner.epoch(),
            rows: self.interner.intern_set(&rows),
            user: self.interner.intern_set(&user),
            pin,
        }
    }

    /// `focus_key` with the application candidates computed on the spot.
    pub fn focus_key_for(&self, facts: &FocusFacts<'_>) -> ProfileKey {
        let cands = facts
            .app
            .as_ref()
            .map(|a| self.set.for_app(a))
            .unwrap_or_default();
        self.focus_key(&cands, facts)
    }

    /// Effective settings for a profile key. A key made by another interner epoch (a stale key
    /// from before a reload) resolves to the layers below the profiles.
    pub fn resolve_focus(&self, key: ProfileKey) -> Effective {
        if key.epoch == self.interner.epoch() {
            let rows = self.interner.set(key.rows);
            let user = self.interner.set(key.user);
            self.fold(&rows, &user)
        } else {
            self.fold(&[], &[])
        }
    }

    /// Fold the layers for the facts of a focus.
    pub fn resolve_for(&self, facts: &FocusFacts<'_>) -> Effective {
        self.resolve_focus(self.focus_key_for(facts))
    }

    /// App keys for one application (no window, context or site: only App triggers can match,
    /// and the matrix keeps the other triggers away from App keys).
    pub fn resolve_app(&self, app: &AppFacts<'_>) -> AppScoped {
        let facts = FocusFacts {
            app: Some(*app),
            ..FocusFacts::default()
        };
        AppScoped {
            eff: self.resolve_for(&facts),
        }
    }

    /// Doc keys for one document of an application.
    pub fn resolve_doc(&self, app: &AppFacts<'_>, doc: &DocFacts<'_>) -> DocScoped {
        let facts = FocusFacts {
            app: Some(*app),
            site: doc.site,
            ..FocusFacts::default()
        };
        DocScoped {
            eff: self.resolve_for(&facts),
        }
    }

    #[allow(clippy::too_many_lines)] // one linear walk over the layer table
    fn fold(&self, rows: &[ProfileId], user: &[ProfileId]) -> Effective {
        let mode = self.mode;
        let base = &*self.base;
        let rows_map = self.rows_map();
        let mut cx = Fold {
            settings: base.defaults.clone(),
            origin: vec![LayerRef::Default; SETTINGS.len()],
            ext: BTreeMap::new(),
            ub1: Vec::new(),
        };
        // L2 extension defaults (not in safe mode).
        if mode != ProfileMode::Disabled {
            for (path, v) in &base.ext_defaults {
                cx.ext.insert(
                    path.clone(),
                    ExtEntry {
                        value: v.clone(),
                        origin: LayerRef::ExtDefault(Arc::from(ext_id_of(path))),
                    },
                );
            }
        }
        // L3 third-party rows, then L4 rows: the list is already in layer order.
        for pid in rows {
            let Some(p) = self.set.get(*pid) else {
                continue;
            };
            let (lref, allowed) = match &p.source {
                ProfileSource::ThirdParty { .. } => {
                    (LayerRef::ThirdParty(*pid), mode == ProfileMode::Active)
                }
                ProfileSource::FirstParty { .. } => {
                    (LayerRef::FirstParty(*pid), mode == ProfileMode::Active)
                }
                // Product rows apply in every mode (M-5).
                ProfileSource::Product => (LayerRef::FirstParty(*pid), true),
                ProfileSource::User => continue,
            };
            if !allowed || !self.is_enabled_in(p, &rows_map) {
                continue;
            }
            layer(&mut cx, &p.values, &p.ext, &lref, true);
        }
        // L5 user base: the file, skipping entries equal to the default (UB-1), then its delta.
        for (k, v) in base.user.file.iter() {
            if !k.def().privacy && is_default(&base.defaults, k, v) {
                if cx.origin[k.index()] != LayerRef::Default {
                    cx.ub1.push(KeyRef::Core(k));
                }
                continue;
            }
            if set_value(&mut cx.settings, k, v) {
                cx.origin[k.index()] = LayerRef::UserBase;
            }
        }
        if mode != ProfileMode::Disabled {
            for (path, v) in &base.user.ext {
                if !base.ext_schema.holds(path) {
                    continue;
                }
                if base.ext_defaults.get(path) == Some(v) {
                    let row_set = cx.ext.get(path).is_some_and(|e| {
                        matches!(e.origin, LayerRef::ThirdParty(_) | LayerRef::FirstParty(_))
                    });
                    if row_set {
                        cx.ub1.push(KeyRef::Ext(path.clone()));
                    }
                    continue;
                }
                cx.ext.insert(
                    path.clone(),
                    ExtEntry {
                        value: v.clone(),
                        origin: LayerRef::UserBase,
                    },
                );
            }
        }
        let d = &self.deltas.base;
        layer(
            &mut cx,
            &d.core,
            &BTreeMap::new(),
            &LayerRef::UserBase,
            false,
        );
        if mode != ProfileMode::Disabled {
            layer(
                &mut cx,
                &SettingsPatch::new(),
                &d.ext,
                &LayerRef::UserBase,
                false,
            );
        }
        // L6 user profiles (only while profiles are active).
        if mode == ProfileMode::Active {
            for pid in user {
                let Some(p) = self.set.get(*pid) else {
                    continue;
                };
                if p.source != ProfileSource::User || !self.is_enabled_in(p, &rows_map) {
                    continue;
                }
                let lref = LayerRef::User(*pid);
                layer(&mut cx, &p.values, &p.ext, &lref, true);
                if let Some(pd) = self.deltas.profiles.get(&p.id) {
                    layer(&mut cx, &pd.core, &pd.ext, &lref, true);
                }
            }
        }
        // Extension keys of extensions that are not installed never apply; in safe mode no
        // extension is loaded.
        if mode == ProfileMode::Disabled {
            cx.ext.clear();
        } else {
            cx.ext.retain(|path, _| base.ext_schema.holds(path));
        }
        Effective {
            generation: self.generation,
            settings: cx.settings,
            ext: cx.ext,
            origin: cx.origin.into_boxed_slice(),
            report: ReportFlags::default(),
            ub1_skipped: cx.ub1.into_boxed_slice(),
        }
    }
}

/// A small cache of folded settings per profile key, tied to the snapshot it was filled from
/// (F-6): a key is the same across delta edits, pins of other applications and suspend, so the
/// cache is emptied whenever the snapshot `generation` it is asked about differs from the one it
/// holds.
#[derive(Debug)]
pub struct EffectiveCache {
    generation: u64,
    lru: super::snapshot::LruMap<ProfileKey, Arc<Effective>>,
}

impl EffectiveCache {
    pub fn new(cap: usize) -> Self {
        EffectiveCache {
            generation: 0,
            lru: super::snapshot::LruMap::new(cap),
        }
    }

    fn sync(&mut self, generation: u64) {
        if self.generation != generation {
            self.lru.clear();
            self.generation = generation;
        }
    }

    /// The effective settings for `key` in `snap`: cached, or folded now.
    pub fn get_or_fold(&mut self, snap: &ProfileSnapshot, key: ProfileKey) -> Arc<Effective> {
        self.sync(snap.generation);
        if let Some(e) = self.lru.get(&key) {
            return Arc::clone(e);
        }
        let e = Arc::new(snap.resolve_focus(key));
        self.lru.insert(key, Arc::clone(&e));
        e
    }

    /// Is `key` cached for `snap`? (Prewarm tests, metrics.)
    pub fn contains(&mut self, snap: &ProfileSnapshot, key: ProfileKey) -> bool {
        self.sync(snap.generation);
        self.lru.contains(&key)
    }

    /// Store a value folded elsewhere (prewarm). Dropped when it was folded from an older
    /// generation than `snap`, so a late prewarm never serves stale settings.
    pub fn insert_prewarmed(
        &mut self,
        snap: &ProfileSnapshot,
        key: ProfileKey,
        eff: Arc<Effective>,
    ) -> bool {
        self.sync(snap.generation);
        if eff.generation != snap.generation {
            return false;
        }
        self.lru.insert(key, eff);
        true
    }

    pub fn len(&self) -> usize {
        self.lru.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lru.is_empty()
    }
}
