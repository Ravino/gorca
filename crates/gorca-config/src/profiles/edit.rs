// SPDX-License-Identifier: GPL-3.0-or-later
//! Edits and saves (ADR-0016 sections 2.5 and 2.5b): where a command's change is written
//! (PE-1, PE-1a/b/c, PE-2), the unsaved delta per writable layer, the save plan with its hash
//! guard (PE-5), and save-to-global with the touched set (PE-4, SG-1 to SG-6).
//!
//! Pure planning: the config task reads the files into a [`DiskView`], gets a [`SavePlan`] of
//! ordered [`WriteStep`]s, performs the atomic writes, and reports back how many succeeded to
//! `complete`. Nothing here touches the disk or blocks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use crate::diag::fnv1a;
use crate::glob::{Glob, escape_literal};
use crate::patch::{KeyId, Value, set_value};
use crate::save::{SaveError, remove_keys, render_profile, set_keys, set_profile_enabled};
use crate::settings::{KeyScope, Settings};

use super::compile::ProfileId;
use super::file::{
    ExtType, KeyRef, Origin, Profile, ProfileSource, VOLUME_FLOOR, admit_core, admit_ext,
};
use super::naming::{display_name, file_id};
use super::resolve::LayerRef;
use super::snapshot::{AppKey, EditTargetKind, Interner, LayerDelta, Pin, ProfileSnapshot};
use super::trigger::{AppFacts, AppMatcher, FocusFacts, Specificity, Trigger, TriggerKind};

/// One change a command asks for.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Edit {
    pub key: KeyRef,
    pub value: Value,
}

/// Why the edit goes where it goes (spoken in the confirmation, PE-3).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TargetWhy {
    /// PE-1: the pinned profile.
    Pinned,
    /// PE-1: the application's existing profile.
    AppProfile,
    /// PE-1: the first edit for this application creates its profile.
    NewAppProfile,
    /// PE-1a: a Site or Window profile supplies the key.
    MoreSpecificSupplier,
    /// PE-2: the site target, existing profile.
    SiteProfile,
    /// PE-2: the site target, profile created.
    NewSiteProfile,
    /// PE-1b: a `Base` key.
    BaseKey,
    /// PE-1b: a privacy key no profile holds.
    PrivacyKey,
    /// PE-1c: the target for this application is "global".
    ExplicitGlobal,
    /// No focused application.
    NoApplication,
}

/// Where the change is written.
#[derive(Clone, Debug)]
pub enum Target {
    /// The global file (L5).
    Global,
    /// A user profile by id; `create` carries the new profile when this edit creates it.
    Profile {
        id: String,
        create: Option<Arc<Profile>>,
    },
}

/// A planned edit.
#[derive(Clone, Debug)]
pub struct EditPlan {
    pub edit: Edit,
    pub target: Target,
    pub why: TargetWhy,
    /// Display name of a profile this edit creates ("new profile for Firefox", spoken once).
    pub announce_new: Option<String>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum EditRefusal {
    #[error("no such setting")]
    UnknownKey,
    #[error("the value does not fit the setting")]
    WrongValue,
    /// PC-1: a command may not write a volume below the floor into a profile; offer global.
    #[error("volume below {VOLUME_FLOOR} cannot go into a profile")]
    VolumeFloor,
    #[error("{0}")]
    NotAllowedHere(&'static str),
    /// The application or host name cannot be turned into a profile (empty or too long).
    #[error("the name cannot be used for a profile")]
    UnusableName,
}

/// Result of applying a planned edit.
#[derive(Clone, Debug)]
pub struct EditApplied {
    pub snapshot: ProfileSnapshot,
    /// The effective value for the focus changed.
    pub changed: bool,
    /// PE-3: when it did not change, the layer that holds the value ("rate 60 globally;
    /// Firefox keeps 45").
    pub held_by: Option<LayerRef>,
}

fn ext_fits(ty: &ExtType, v: &Value) -> bool {
    match (ty, v) {
        (ExtType::Bool, Value::Bool(_))
        | (ExtType::Str, Value::Str(_))
        | (ExtType::StrList, Value::StrList(_)) => true,
        (ExtType::Int { min, max }, Value::Int(n)) => (min..=max).contains(&n),
        (ExtType::Enum(names), Value::Str(s)) => names.iter().any(|n| n == s),
        (
            ExtType::Bool
            | ExtType::Str
            | ExtType::StrList
            | ExtType::Int { .. }
            | ExtType::Enum(_),
            Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::StrList(_) | Value::Map(_),
        ) => false,
    }
}

fn app_matcher_literal(name: &str) -> Result<AppMatcher, EditRefusal> {
    let g = Glob::new(&escape_literal(name)).map_err(|_| EditRefusal::UnusableName)?;
    Ok(AppMatcher {
        name: vec![g],
        ..AppMatcher::default()
    })
}

impl ProfileSnapshot {
    /// The user profiles that apply to `facts` in fold order, with their specificity (the pin
    /// replaces automatic selection). Disabled profiles are left out.
    fn user_list(&self, facts: &FocusFacts<'_>) -> Vec<(ProfileId, Specificity)> {
        let key = self.focus_key_for(facts);
        let ids = self.interner.set(key.user);
        ids.iter()
            .filter_map(|id| {
                let p = self.set.get(*id)?;
                if !self.is_enabled(p) {
                    return None;
                }
                let spec = p
                    .triggers
                    .iter()
                    .filter_map(|t| t.matches(facts))
                    .max()
                    .unwrap_or_default();
                Some((*id, spec))
            })
            .collect()
    }

    /// Does the profile currently hold `key` in its file values or its unsaved delta?
    fn profile_holds(&self, p: &Profile, key: &KeyRef) -> bool {
        p.holds(key)
            || self
                .deltas
                .profiles
                .get(&p.id)
                .is_some_and(|d| d.holds(key))
    }

    fn id_free(&self, id: &str) -> bool {
        self.set.id_of(id).is_none() && !self.base.reserved_ids.contains(id)
    }

    fn unused_id(&self, wanted: &str) -> String {
        if self.id_free(wanted) {
            return wanted.to_owned();
        }
        for n in 2..1000 {
            let c = format!("{wanted}-{n}");
            if self.id_free(&c) {
                return c;
            }
        }
        format!("{wanted}-x")
    }

    fn new_app_profile(&self, app: &AppFacts<'_>) -> Result<Arc<Profile>, EditRefusal> {
        let name = display_name(app.name);
        if name.is_empty() {
            return Err(EditRefusal::UnusableName);
        }
        let m = app_matcher_literal(app.name)?;
        let id = self.unused_id(&file_id(app.name));
        Ok(Arc::new(Profile::new(
            &id,
            &name,
            ProfileSource::User,
            vec![Trigger::App { app: m }],
        )))
    }

    fn new_site_profile(
        &self,
        app: &AppFacts<'_>,
        host: &str,
    ) -> Result<Arc<Profile>, EditRefusal> {
        let name = display_name(host);
        if name.is_empty() {
            return Err(EditRefusal::UnusableName);
        }
        let m = app_matcher_literal(app.name)?;
        let hg = Glob::new(&escape_literal(host)).map_err(|_| EditRefusal::UnusableName)?;
        let id = self.unused_id(&file_id(&format!("site {host}")));
        Ok(Arc::new(Profile::new(
            &id,
            &name,
            ProfileSource::User,
            vec![Trigger::Site {
                app: m,
                host: hg,
                path: None,
            }],
        )))
    }

    /// Check that `key` and `value` belong to the global schema and fit (PK-1).
    fn check_edit(&self, edit: &Edit) -> Result<(), EditRefusal> {
        match &edit.key {
            KeyRef::Core(k) => {
                let mut probe = Settings::default();
                if set_value(&mut probe, *k, &edit.value) {
                    Ok(())
                } else {
                    Err(EditRefusal::WrongValue)
                }
            }
            KeyRef::Ext(p) => {
                let rest = p.strip_prefix("ext.").ok_or(EditRefusal::UnknownKey)?;
                let (id, key) = rest.split_once('.').ok_or(EditRefusal::UnknownKey)?;
                let ty = self
                    .base
                    .ext_schema
                    .key(id, key)
                    .ok_or(EditRefusal::UnknownKey)?;
                if ext_fits(ty, &edit.value) {
                    Ok(())
                } else {
                    Err(EditRefusal::WrongValue)
                }
            }
        }
    }

    fn admits(p: &Profile, key: &KeyRef) -> bool {
        match key {
            KeyRef::Core(k) => admit_core(*k, &p.triggers, Origin::User).is_ok(),
            KeyRef::Ext(path) => {
                let id = path
                    .strip_prefix("ext.")
                    .and_then(|r| r.split_once('.'))
                    .map_or("", |(i, _)| i);
                admit_ext(&p.triggers, Origin::User, id).is_ok()
            }
        }
    }

    /// Where does this edit go (PE-1 to PE-2)? `facts` describe the focus the command ran in.
    #[allow(clippy::too_many_lines)] // one decision list, in the order of the ADR
    pub fn plan_edit(&self, facts: &FocusFacts<'_>, edit: Edit) -> Result<EditPlan, EditRefusal> {
        self.check_edit(&edit)?;
        let plan =
            |target: Target, why: TargetWhy, announce: Option<String>, edit: Edit| EditPlan {
                edit,
                target,
                why,
                announce_new: announce,
            };
        let (scope, privacy) = match &edit.key {
            KeyRef::Core(k) => (k.def().scope, k.def().privacy),
            KeyRef::Ext(_) => (KeyScope::Focus, false),
        };
        // PE-1b: Base keys are inherently global and never enter a profile.
        if scope == KeyScope::Base {
            return Ok(plan(Target::Global, TargetWhy::BaseKey, None, edit));
        }
        let user = match facts.app.as_ref() {
            Some(a) if !a.name.is_empty() => self.user_list(facts),
            Some(_) | None => {
                return Ok(plan(Target::Global, TargetWhy::NoApplication, None, edit));
            }
        };
        let Some(app) = facts.app.as_ref() else {
            return Ok(plan(Target::Global, TargetWhy::NoApplication, None, edit));
        };
        let appkey = AppKey::of(app.name);
        // The most specific profile that holds the key: PE-1a and the privacy exception.
        let holder = user.iter().rev().find_map(|(id, spec)| {
            let p = self.set.get(*id)?;
            self.profile_holds(p, &edit.key).then_some((p, *spec))
        });
        // PC-1 is checked once the target is known.
        let finish = |t: Target, why: TargetWhy, announce: Option<String>, edit: Edit| {
            if let (Target::Profile { .. }, KeyRef::Core(k)) = (&t, &edit.key)
                && k.def().section == "speech"
                && k.def().key == "volume"
                && matches!(edit.value, Value::Int(n) if n < VOLUME_FLOOR)
            {
                return Err(EditRefusal::VolumeFloor);
            }
            Ok(plan(t, why, announce, edit))
        };
        if privacy {
            // A privacy key a user profile already holds is edited there (PE-1a logic);
            // otherwise it is a choice about the person and the room: global.
            return match holder {
                Some((p, _)) => finish(
                    Target::Profile {
                        id: p.id.clone(),
                        create: None,
                    },
                    TargetWhy::MoreSpecificSupplier,
                    None,
                    edit,
                ),
                None => Ok(plan(Target::Global, TargetWhy::PrivacyKey, None, edit)),
            };
        }
        let kind = self.target_of(&appkey);
        // PE-1c.
        if kind == EditTargetKind::Global {
            return Ok(plan(Target::Global, TargetWhy::ExplicitGlobal, None, edit));
        }
        // PE-1a: a Site or Window profile that supplies the key gets the edit.
        if kind == EditTargetKind::App
            && let Some((p, spec)) = holder
            && matches!(spec.kind, Some(TriggerKind::Window | TriggerKind::Site))
        {
            return finish(
                Target::Profile {
                    id: p.id.clone(),
                    create: None,
                },
                TargetWhy::MoreSpecificSupplier,
                None,
                edit,
            );
        }
        // PE-2: the site target needs a document; the matrix keeps App keys out of it.
        if kind == EditTargetKind::Site
            && let Some(site) = facts.site.as_ref()
            && scope != KeyScope::App
        {
            let host_lit = site.host;
            let existing = self.set.iter().find(|(_, p)| {
                p.source == ProfileSource::User
                    && !p.manual
                    && Self::admits(p, &edit.key)
                    && p.triggers.iter().any(|t| {
                        matches!(t, Trigger::Site { app: m, host, path: None }
                            if host.matches(host_lit)
                                && host.literal().is_some()
                                && (m.is_empty() || m.matches(app).is_some()))
                    })
            });
            if let Some((_, p)) = existing {
                return finish(
                    Target::Profile {
                        id: p.id.clone(),
                        create: None,
                    },
                    TargetWhy::SiteProfile,
                    None,
                    edit,
                );
            }
            let p = self.new_site_profile(app, site.host)?;
            let name = p.name.clone();
            return finish(
                Target::Profile {
                    id: p.id.clone(),
                    create: Some(p),
                },
                TargetWhy::NewSiteProfile,
                Some(name),
                edit,
            );
        }
        // PE-1: the pinned profile, else the application's profile, else a new one.
        if let Some(Pin::Profile(pid)) = self.pin_of(&appkey)
            && let Some(id) = self.set.id_of(pid)
            && let Some(p) = self.set.get(id)
        {
            if !Self::admits(p, &edit.key) {
                return Err(EditRefusal::NotAllowedHere(
                    "the pinned profile cannot hold this setting",
                ));
            }
            return finish(
                Target::Profile {
                    id: p.id.clone(),
                    create: None,
                },
                TargetWhy::Pinned,
                None,
                edit,
            );
        }
        // The app profile: the most specific user profile with an App trigger for this app.
        let mut blocked = false;
        let app_profile = user.iter().rev().find_map(|(id, _)| {
            let p = self.set.get(*id)?;
            let has_app = p
                .triggers
                .iter()
                .any(|t| matches!(t, Trigger::App { app: m } if m.matches(app).is_some()));
            if has_app && !Self::admits(p, &edit.key) {
                blocked = true;
            }
            (has_app && Self::admits(p, &edit.key)).then_some(p)
        });
        if app_profile.is_none() && blocked {
            // F-5: a second profile for the same application would be a silent twin.
            return Err(EditRefusal::NotAllowedHere(
                "the application's profile also has a window or context trigger, so it cannot hold this setting",
            ));
        }
        if let Some(p) = app_profile {
            return finish(
                Target::Profile {
                    id: p.id.clone(),
                    create: None,
                },
                TargetWhy::AppProfile,
                None,
                edit,
            );
        }
        let p = self.new_app_profile(app)?;
        let name = p.name.clone();
        finish(
            Target::Profile {
                id: p.id.clone(),
                create: Some(p),
            },
            TargetWhy::NewAppProfile,
            Some(name),
            edit,
        )
    }

    /// Apply a planned edit as an unsaved delta; returns the new snapshot and whether the
    /// effective value for `facts` changed (PE-3).
    pub fn apply_edit(&self, facts: &FocusFacts<'_>, plan: &EditPlan) -> EditApplied {
        let before = self.resolve_for(facts).value(&plan.edit.key);
        let mut s = self.bumped();
        let mut d = (*self.deltas).clone();
        match &plan.target {
            Target::Global => {
                d.base.set(&plan.edit.key, plan.edit.value.clone());
            }
            Target::Profile { id, create } => {
                if let Some(p) = create {
                    let (set, _) = s.set.with_profile(p.clone());
                    s.set = Arc::new(set);
                    s.interner = Arc::new(Interner::new());
                    d.created.insert(id.clone(), p.clone());
                }
                d.profiles
                    .entry(id.clone())
                    .or_default()
                    .set(&plan.edit.key, plan.edit.value.clone());
                d.touched
                    .entry(id.clone())
                    .or_default()
                    .insert(plan.edit.key.clone());
            }
        }
        s.deltas = Arc::new(d);
        let after_eff = s.resolve_for(facts);
        let after = after_eff.value(&plan.edit.key);
        let changed = before != after;
        let held_by = if changed {
            None
        } else {
            after_eff.origin_of(&plan.edit.key)
        };
        EditApplied {
            snapshot: s,
            changed,
            held_by,
        }
    }

    /// Switch a profile or row on or off (`profiles.toggle_enabled`): a user profile gets an
    /// `enabled` delta, a row an entry of `profiles.rows` in the global delta (PF-2).
    #[must_use]
    pub fn toggle_enabled(&self, profile_id: &str) -> Option<ProfileSnapshot> {
        let pid = self.set.id_of(profile_id)?;
        let p = self.set.get(pid)?.clone();
        let now = !self.is_enabled(&p);
        let mut s = self.bumped();
        let mut d = (*self.deltas).clone();
        if p.source == ProfileSource::User {
            d.profiles.entry(p.id.clone()).or_default().enabled = Some(now);
        } else {
            let mut m = self.rows_map();
            m.insert(p.id.clone(), now);
            let k = KeyId::of("profiles", "rows")?;
            d.base.core.set(k, Value::Map(m));
        }
        s.deltas = Arc::new(d);
        Some(s)
    }
}

// ---- saving ------------------------------------------------------------------------------------

/// The current text of the files, as the config task read them just before planning.
#[derive(Clone, Debug, Default)]
pub struct DiskView {
    /// `None`: the global file does not exist.
    pub global: Option<String>,
    /// Profile files by id.
    pub profiles: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum FileId {
    Global,
    Profile(String),
}

/// One atomic file write: replace the file if its content still hashes to `expect_hash`
/// (FNV-1a; the hash of nothing for a file that must not exist yet).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteStep {
    pub file: FileId,
    pub expect_hash: u64,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SaveRefusal {
    /// SG-2: nothing was changed by command for this target in this session.
    #[error("nothing changed for `{0}` in this session")]
    NothingChanged(String),
    #[error("no such profile `{0}`")]
    UnknownProfile(String),
    /// PE-5 / SG-4: the file changed on disk since it was loaded; reload first.
    #[error("{0:?} changed on disk, reload first")]
    ChangedOnDisk(FileId),
    #[error("profile file `{0}` is missing on disk")]
    MissingOnDisk(String),
    #[error("{0}")]
    Text(SaveError),
    #[error("nothing to save")]
    NothingToSave,
}

#[derive(Clone, Debug)]
enum Effect {
    /// `settings.save` of the global delta.
    SaveBase,
    /// `settings.save` of one profile's delta (and creation of its file).
    SaveProfile { id: String },
    /// Save-to-global: copied keys into the global file.
    GlobalCopy { values: Vec<(KeyRef, Value)> },
    /// Save-to-global: keys removed from a profile file; `flush` when the file was rendered
    /// with the whole delta (a profile that had no file yet).
    ProfileRemove {
        id: String,
        keys: Vec<KeyRef>,
        flush: bool,
    },
}

/// What a save-to-global did, for the confirmation.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SaveGlobalReport {
    /// Keys copied into the global file.
    pub copied: Vec<KeyRef>,
    /// Keys whose value already equals the global one: removed from the profile only.
    pub already_global: Vec<KeyRef>,
    /// SG-3: keys of extensions that are not installed, kept for the profile only.
    pub kept_not_installed: Vec<KeyRef>,
    /// F-1: keys left in the profile because moving them would change the application's
    /// effective value (a row sets the key and a default-equal global entry is skipped, UB-1).
    pub kept_row_sets: Vec<KeyRef>,
    /// Other profiles that lost the keys (`apply_everywhere`).
    pub other_profiles_changed: Vec<String>,
}

/// An ordered set of writes with what to do in the snapshot once they are done.
#[derive(Clone, Debug)]
pub struct SavePlan {
    pub steps: Vec<WriteStep>,
    effects: Vec<Effect>,
    /// In-memory only: created profiles that lose keys, no file to write (apply_everywhere).
    memory: Vec<(String, Vec<KeyRef>)>,
    pub report: SaveGlobalReport,
}

fn hash_of(text: Option<&str>) -> u64 {
    fnv1a(text.unwrap_or("").as_bytes())
}

impl ProfileSnapshot {
    fn guard_global(&self, disk: &DiskView) -> Result<(), SaveRefusal> {
        if hash_of(disk.global.as_deref()) == self.base.user.hash {
            Ok(())
        } else {
            Err(SaveRefusal::ChangedOnDisk(FileId::Global))
        }
    }

    fn profile_disk<'a>(
        &self,
        p: &Profile,
        disk: &'a DiskView,
    ) -> Result<Option<&'a str>, SaveRefusal> {
        let created = self.deltas.created.contains_key(&p.id);
        match disk.profiles.get(&p.id) {
            Some(t) => {
                if created || fnv1a(t.as_bytes()) != p.hash {
                    return Err(SaveRefusal::ChangedOnDisk(FileId::Profile(p.id.clone())));
                }
                Ok(Some(t.as_str()))
            }
            None if created => Ok(None),
            None => Err(SaveRefusal::MissingOnDisk(p.id.clone())),
        }
    }

    /// PE-5: write every dirty delta to its own file. Refuses when a file changed on disk.
    pub fn plan_save(&self, disk: &DiskView) -> Result<SavePlan, SaveRefusal> {
        let mut plan = SavePlan {
            steps: Vec::new(),
            effects: Vec::new(),
            memory: Vec::new(),
            report: SaveGlobalReport::default(),
        };
        let b = &self.deltas.base;
        if !b.core.is_empty() || !b.ext.is_empty() {
            self.guard_global(disk)?;
            let vals: Vec<(KeyRef, Value)> = b
                .core
                .iter()
                .map(|(k, v)| (KeyRef::Core(k), v.clone()))
                .chain(
                    b.ext
                        .iter()
                        .map(|(p, v)| (KeyRef::Ext(p.clone()), v.clone())),
                )
                .collect();
            let text =
                set_keys(disk.global.as_deref().unwrap_or(""), &vals).map_err(SaveRefusal::Text)?;
            plan.steps.push(WriteStep {
                file: FileId::Global,
                expect_hash: self.base.user.hash,
                text,
            });
            plan.effects.push(Effect::SaveBase);
        }
        for (id, delta) in &self.deltas.profiles {
            let created = self.deltas.created.get(id);
            if delta.is_empty() && created.is_none() {
                continue;
            }
            let Some(pid) = self.set.id_of(id) else {
                return Err(SaveRefusal::UnknownProfile(id.clone()));
            };
            let Some(p) = self.set.get(pid) else {
                return Err(SaveRefusal::UnknownProfile(id.clone()));
            };
            let on_disk = self.profile_disk(p, disk)?;
            let vals: Vec<(KeyRef, Value)> = delta
                .core
                .iter()
                .map(|(k, v)| (KeyRef::Core(k), v.clone()))
                .chain(
                    delta
                        .ext
                        .iter()
                        .map(|(q, v)| (KeyRef::Ext(q.clone()), v.clone())),
                )
                .collect();
            let text = if let Some(t) = on_disk {
                let mut t = set_keys(t, &vals).map_err(SaveRefusal::Text)?;
                if let Some(en) = delta.enabled {
                    t = set_profile_enabled(&t, en).map_err(SaveRefusal::Text)?;
                }
                t
            } else {
                let mut q = (**p).clone();
                if let Some(en) = delta.enabled {
                    q.enabled = en;
                }
                render_profile(&q, &vals)
            };
            plan.steps.push(WriteStep {
                file: FileId::Profile(id.clone()),
                expect_hash: if on_disk.is_some() {
                    p.hash
                } else {
                    hash_of(None)
                },
                text,
            });
            plan.effects.push(Effect::SaveProfile { id: id.clone() });
        }
        if plan.steps.is_empty() {
            return Err(SaveRefusal::NothingToSave);
        }
        Ok(plan)
    }

    /// The value the user set for `key` in profile `p` this session (the delta over the file).
    fn profile_value(&self, p: &Profile, key: &KeyRef) -> Option<Value> {
        self.deltas
            .profiles
            .get(&p.id)
            .and_then(|d| d.get(key))
            .or_else(|| match key {
                KeyRef::Core(k) => p.values.get(*k).cloned(),
                KeyRef::Ext(path) => p.ext.get(path).cloned(),
            })
    }

    /// The value the global file explicitly sets for `key` (not the default: F-1).
    fn global_explicit(&self, key: &KeyRef) -> Option<Value> {
        match key {
            KeyRef::Core(k) => self.base.user.file.get(*k).cloned(),
            KeyRef::Ext(p) => self.base.user.ext.get(p).cloned(),
        }
    }

    /// PE-4 / SG-1..SG-6: copy the touched keys of `profile_id` into the global file and remove
    /// them from the profile; with `apply_everywhere` also from every other user profile.
    ///
    /// `facts` is the focus the command ran in: a key whose move would change the effective
    /// value there (a row sets it and global's default-equal entry is skipped, UB-1) stays in
    /// the profile and is reported in `kept_row_sets` (F-1).
    pub fn plan_save_global(
        &self,
        facts: &FocusFacts<'_>,
        profile_id: &str,
        apply_everywhere: bool,
        disk: &DiskView,
    ) -> Result<SavePlan, SaveRefusal> {
        let plan = self.plan_save_global_excl(profile_id, apply_everywhere, disk, &[])?;
        let before = self.resolve_for(facts);
        let after = self.complete(&plan, plan.steps.len()).resolve_for(facts);
        let offenders: Vec<KeyRef> = plan
            .report
            .copied
            .iter()
            .chain(plan.report.already_global.iter())
            .filter(|k| before.value(k) != after.value(k))
            .cloned()
            .collect();
        if offenders.is_empty() {
            return Ok(plan);
        }
        let mut plan =
            self.plan_save_global_excl(profile_id, apply_everywhere, disk, &offenders)?;
        plan.report.kept_row_sets = offenders;
        Ok(plan)
    }

    /// PE-4, second press of the save-to-global chord: the keys that the first press moved to
    /// the global file also leave every other user profile that holds them, so the global value
    /// applies everywhere. `except` is the profile that was just saved. Writes only user
    /// profile files; the global file already holds the value.
    pub fn plan_apply_everywhere(
        &self,
        keys: &[KeyRef],
        except: &str,
        disk: &DiskView,
    ) -> Result<SavePlan, SaveRefusal> {
        let mut plan = SavePlan {
            steps: Vec::new(),
            effects: Vec::new(),
            memory: Vec::new(),
            report: SaveGlobalReport::default(),
        };
        for (_, q) in self.set.iter() {
            if q.source != ProfileSource::User || q.id == except {
                continue;
            }
            let held: Vec<KeyRef> = keys
                .iter()
                .filter(|k| self.profile_holds(q, k))
                .cloned()
                .collect();
            if held.is_empty() {
                continue;
            }
            plan.report.other_profiles_changed.push(q.id.clone());
            if self.deltas.created.contains_key(&q.id) {
                plan.memory.push((q.id.clone(), held));
                continue;
            }
            let t = self
                .profile_disk(q, disk)?
                .ok_or_else(|| SaveRefusal::MissingOnDisk(q.id.clone()))?;
            let text = remove_keys(t, &held).map_err(SaveRefusal::Text)?;
            plan.steps.push(WriteStep {
                file: FileId::Profile(q.id.clone()),
                expect_hash: q.hash,
                text,
            });
            plan.effects.push(Effect::ProfileRemove {
                id: q.id.clone(),
                keys: held,
                flush: false,
            });
        }
        if plan.report.other_profiles_changed.is_empty() {
            return Err(SaveRefusal::NothingChanged(except.to_owned()));
        }
        Ok(plan)
    }

    #[allow(clippy::too_many_lines)] // one linear plan; splitting would hide the SG order
    fn plan_save_global_excl(
        &self,
        profile_id: &str,
        apply_everywhere: bool,
        disk: &DiskView,
        exclude: &[KeyRef],
    ) -> Result<SavePlan, SaveRefusal> {
        let pid = self
            .set
            .id_of(profile_id)
            .ok_or_else(|| SaveRefusal::UnknownProfile(profile_id.to_owned()))?;
        let p = self
            .set
            .get(pid)
            .ok_or_else(|| SaveRefusal::UnknownProfile(profile_id.to_owned()))?
            .clone();
        // SG-2: exactly the touched set, never anything else.
        let touched: Vec<KeyRef> = self
            .deltas
            .touched
            .get(profile_id)
            .map(|t| t.iter().filter(|k| !exclude.contains(k)).cloned().collect())
            .unwrap_or_default();
        if touched.is_empty() {
            return Err(SaveRefusal::NothingChanged(p.name.clone()));
        }
        let mut report = SaveGlobalReport::default();
        let mut to_copy: Vec<(KeyRef, Value)> = Vec::new();
        let mut to_remove: Vec<KeyRef> = Vec::new();
        for k in touched {
            let Some(v) = self.profile_value(&p, &k) else {
                continue;
            };
            // SG-3: only keys global can hold.
            let holds = match &k {
                KeyRef::Core(id) => id.def().scope != KeyScope::Base,
                KeyRef::Ext(path) => self.base.ext_schema.holds(path),
            };
            if !holds {
                report.kept_not_installed.push(k);
                continue;
            }
            // SG-4: an equal value only leaves the profile.
            // The user's explicit value is copied even when it equals the default (F-1), and
            // an unsaved global edit of the key is never "already global" (F-2).
            if self.global_explicit(&k).as_ref() == Some(&v) && !self.deltas.base.holds(&k) {
                report.already_global.push(k.clone());
            } else {
                report.copied.push(k.clone());
                to_copy.push((k.clone(), v));
            }
            to_remove.push(k);
        }
        if to_remove.is_empty() {
            // Only uninstalled-extension keys were touched.
            return Err(SaveRefusal::NothingChanged(p.name.clone()));
        }
        let mut steps = Vec::new();
        let mut effects = Vec::new();
        // SG-5: the global file first. (Skipped when every key already equals global.)
        // The global file must still be as loaded, or "already global" may be stale too.
        self.guard_global(disk)?;
        if !to_copy.is_empty() {
            let text = set_keys(disk.global.as_deref().unwrap_or(""), &to_copy)
                .map_err(SaveRefusal::Text)?;
            steps.push(WriteStep {
                file: FileId::Global,
                expect_hash: self.base.user.hash,
                text,
            });
            effects.push(Effect::GlobalCopy { values: to_copy });
        }
        // Then the target profile.
        let on_disk = self.profile_disk(&p, disk)?;
        let flush = on_disk.is_none();
        let text = if let Some(t) = on_disk {
            remove_keys(t, &to_remove).map_err(SaveRefusal::Text)?
        } else {
            // A profile with no file yet: render it with its remaining delta, minus the keys
            // that moved to global.
            let vals: Vec<(KeyRef, Value)> = self
                .deltas
                .profiles
                .get(&p.id)
                .map(|d| {
                    d.core
                        .iter()
                        .map(|(k, v)| (KeyRef::Core(k), v.clone()))
                        .chain(
                            d.ext
                                .iter()
                                .map(|(q, v)| (KeyRef::Ext(q.clone()), v.clone())),
                        )
                        .filter(|(k, _)| !to_remove.contains(k))
                        .collect()
                })
                .unwrap_or_default();
            let mut q = (*p).clone();
            for k in &to_remove {
                match k {
                    KeyRef::Core(id) => {
                        q.values.remove(*id);
                    }
                    KeyRef::Ext(path) => {
                        q.ext.remove(path);
                    }
                }
            }
            render_profile(&q, &vals)
        };
        steps.push(WriteStep {
            file: FileId::Profile(profile_id.to_owned()),
            expect_hash: if flush { hash_of(None) } else { p.hash },
            text,
        });
        effects.push(Effect::ProfileRemove {
            id: profile_id.to_owned(),
            keys: to_remove.clone(),
            flush,
        });
        // Apply everywhere: the same keys leave every other user profile that holds them.
        let mut memory = Vec::new();
        if apply_everywhere {
            for (_, q) in self.set.iter() {
                if q.source != ProfileSource::User || q.id == profile_id {
                    continue;
                }
                let held: Vec<KeyRef> = to_remove
                    .iter()
                    .filter(|k| self.profile_holds(q, k))
                    .cloned()
                    .collect();
                if held.is_empty() {
                    continue;
                }
                report.other_profiles_changed.push(q.id.clone());
                if self.deltas.created.contains_key(&q.id) {
                    memory.push((q.id.clone(), held));
                    continue;
                }
                let t = self
                    .profile_disk(q, disk)?
                    .ok_or_else(|| SaveRefusal::MissingOnDisk(q.id.clone()))?;
                let text = remove_keys(t, &held).map_err(SaveRefusal::Text)?;
                steps.push(WriteStep {
                    file: FileId::Profile(q.id.clone()),
                    expect_hash: q.hash,
                    text,
                });
                effects.push(Effect::ProfileRemove {
                    id: q.id.clone(),
                    keys: held,
                    flush: false,
                });
            }
        }
        Ok(SavePlan {
            steps,
            effects,
            memory,
            report,
        })
    }

    /// The snapshot after the first `done` steps of `plan` were written (SG-5: a failure after
    /// the first step leaves both files consistent and the touched set kept).
    #[must_use]
    pub fn complete(&self, plan: &SavePlan, done: usize) -> ProfileSnapshot {
        let mut s = self.bumped();
        let mut base = (*self.base).clone();
        let mut d = (*self.deltas).clone();
        let mut set = self.set.clone();
        for (step, effect) in plan.steps.iter().zip(plan.effects.iter()).take(done) {
            let hash = fnv1a(step.text.as_bytes());
            match effect {
                Effect::SaveBase => {
                    for (k, v) in d.base.core.iter() {
                        base.user.file.set(k, v.clone());
                    }
                    for (p, v) in &d.base.ext {
                        base.user.ext.insert(p.clone(), v.clone());
                    }
                    base.user.hash = hash;
                    d.base = LayerDelta::default();
                }
                Effect::SaveProfile { id } => {
                    if let Some(p) = set.id_of(id).and_then(|i| set.get(i)) {
                        let mut q = (**p).clone();
                        if let Some(delta) = d.profiles.remove(id) {
                            for (k, v) in delta.core.iter() {
                                q.values.set(k, v.clone());
                            }
                            q.ext.extend(delta.ext);
                            if let Some(en) = delta.enabled {
                                q.enabled = en;
                            }
                        }
                        q.hash = hash;
                        d.created.remove(id);
                        set = Arc::new(set.with_profile(Arc::new(q)).0);
                    }
                }
                Effect::GlobalCopy { values } => {
                    for (k, v) in values {
                        match k {
                            KeyRef::Core(id) => {
                                base.user.file.set(*id, v.clone());
                            }
                            KeyRef::Ext(p) => {
                                base.user.ext.insert(p.clone(), v.clone());
                            }
                        }
                        d.base.remove(k);
                    }
                    base.user.hash = hash;
                }
                Effect::ProfileRemove { id, keys, flush } => {
                    if let Some(p) = set.id_of(id).and_then(|i| set.get(i)) {
                        let mut q = (**p).clone();
                        for k in keys {
                            match k {
                                KeyRef::Core(kid) => {
                                    q.values.remove(*kid);
                                }
                                KeyRef::Ext(path) => {
                                    q.ext.remove(path);
                                }
                            }
                        }
                        if *flush {
                            // The rendered file carried the rest of the delta.
                            if let Some(delta) = d.profiles.remove(id) {
                                for (k, v) in delta.core.iter() {
                                    if !keys.contains(&KeyRef::Core(k)) {
                                        q.values.set(k, v.clone());
                                    }
                                }
                                for (path, v) in delta.ext {
                                    if !keys.contains(&KeyRef::Ext(path.clone())) {
                                        q.ext.insert(path, v);
                                    }
                                }
                            }
                            d.created.remove(id);
                        } else if let Some(delta) = d.profiles.get_mut(id) {
                            for k in keys {
                                delta.remove(k);
                            }
                        }
                        q.hash = hash;
                        set = Arc::new(set.with_profile(Arc::new(q)).0);
                    }
                    if let Some(t) = d.touched.get_mut(id) {
                        for k in keys {
                            t.remove(k);
                        }
                    }
                }
            }
        }
        if done >= plan.steps.len() {
            // Created profiles that only lost keys in memory.
            for (id, keys) in &plan.memory {
                if let Some(delta) = d.profiles.get_mut(id) {
                    for k in keys {
                        delta.remove(k);
                    }
                }
                if let Some(c) = d.created.get(id) {
                    let mut q = (**c).clone();
                    for k in keys {
                        match k {
                            KeyRef::Core(kid) => {
                                q.values.remove(*kid);
                            }
                            KeyRef::Ext(path) => {
                                q.ext.remove(path);
                            }
                        }
                    }
                    d.created.insert(id.clone(), Arc::new(q));
                }
                if let Some(t) = d.touched.get_mut(id) {
                    for k in keys {
                        t.remove(k);
                    }
                }
            }
        }
        d.touched.retain(|_, t| !t.is_empty());
        if !Arc::ptr_eq(&set, &self.set) {
            s.interner = Arc::new(Interner::new());
        }
        s.set = set;
        s.base = Arc::new(base);
        s.deltas = Arc::new(d);
        s
    }

    /// The touched keys of a target, for tests and `settings.explain_current`.
    pub fn touched(&self, profile_id: &str) -> BTreeSet<KeyRef> {
        self.deltas
            .touched
            .get(profile_id)
            .cloned()
            .unwrap_or_default()
    }
}
