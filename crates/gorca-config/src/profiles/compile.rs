// SPDX-License-Identifier: GPL-3.0-or-later
//! The compiled profile set (ADR-0016 TR-2) and the reload logic that keeps the last good parse
//! of every file (PC-0).
//!
//! `ProfileSet` is immutable and cheap to share (`Arc<Profile>` inside). Lookup tables are built
//! once: profiles with an application matcher are evaluated once per application
//! ([`ProfileSet::for_app`], cached by the caller per `AppId`), context triggers are indexed per
//! context, window and site triggers without an application are kept in one list. A focus change
//! then evaluates the full triggers of the few candidates only.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use crate::diag::{Diag, DiagMsg};

use super::file::{ExtSchema, MAX_PROFILE_FILES, Profile, ProfileSource, parse_user_profile};
use super::trigger::{
    AppFacts, ContextSet, FocusContext, FocusFacts, N_CONTEXT, Specificity, Trigger,
};

/// Index of a profile in its [`ProfileSet`]. Ids are assigned in `(layer, file id)` order, so a
/// lower id of the same layer is first in id order (PM-1 tie-break: it wins).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct ProfileId(pub u32);

impl ProfileId {
    pub fn index(self) -> usize {
        usize::try_from(self.0).unwrap_or(usize::MAX)
    }
}

/// One matching profile with the specificity of its best matching trigger.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Matched {
    pub id: ProfileId,
    pub spec: Specificity,
}

/// Profiles whose application matcher accepts one application (TR-2: evaluated once per app).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AppCandidates {
    ids: Box<[ProfileId]>,
}

impl AppCandidates {
    pub fn ids(&self) -> &[ProfileId] {
        &self.ids
    }
}

/// The compiled profiles and rows of one load.
#[derive(Clone, Debug, Default)]
pub struct ProfileSet {
    profiles: Vec<Arc<Profile>>,
    by_id: HashMap<String, ProfileId>,
    /// Profiles with a trigger that names applications (App, or Window/Site with a matcher).
    app_bound: Vec<ProfileId>,
    /// Profiles with a Window or Site trigger that names no application.
    any_app: Vec<ProfileId>,
    by_context: [Vec<ProfileId>; N_CONTEXT],
}

impl ProfileSet {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Compile rows and user profiles. More than [`MAX_PROFILE_FILES`] user profiles: the first
    /// by id are kept and one diagnostic says so (PC-3). Two profiles of one layer with the same
    /// trigger get one diagnostic naming both (PM-1).
    pub fn build(rows: Vec<Arc<Profile>>, users: Vec<Arc<Profile>>) -> (ProfileSet, Vec<Diag>) {
        let mut diags = Vec::new();
        let mut all: Vec<Arc<Profile>> = rows;
        let mut users = users;
        users.sort_by(|a, b| a.id.cmp(&b.id));
        if users.len() > MAX_PROFILE_FILES {
            users.truncate(MAX_PROFILE_FILES);
            diags.push(Diag::error(
                "profiles",
                None,
                DiagMsg::ProfileLimit {
                    what: "profile files",
                    limit: MAX_PROFILE_FILES,
                },
            ));
        }
        all.extend(users);
        all.sort_by(|a, b| {
            a.source
                .layer_rank()
                .cmp(&b.source.layer_rank())
                .then_with(|| a.id.cmp(&b.id))
        });
        let mut set = ProfileSet::default();
        let mut seen: HashMap<(u8, String), String> = HashMap::new();
        for p in all {
            let id = ProfileId(u32::try_from(set.profiles.len()).unwrap_or(u32::MAX));
            if set.by_id.contains_key(&p.id) {
                diags.push(Diag::error(
                    &p.id,
                    None,
                    DiagMsg::ProfileInvalid {
                        what: "profile",
                        why: "duplicate id".to_owned(),
                    },
                ));
                continue;
            }
            let mut bound = false;
            let mut anyapp = false;
            let mut ctxs = [false; N_CONTEXT];
            for t in &p.triggers {
                match t {
                    Trigger::App { .. } => bound = true,
                    Trigger::Window { app, .. } | Trigger::Site { app, .. } => {
                        if app.is_empty() {
                            anyapp = true;
                        } else {
                            bound = true;
                        }
                    }
                    Trigger::Context { ctx } => ctxs[ctx.index()] = true,
                }
                let key = (p.source.layer_rank(), t.canonical());
                match seen.get(&key) {
                    Some(other) if *other != p.id => diags.push(Diag::warning(
                        &p.id,
                        None,
                        DiagMsg::SameTrigger {
                            other: other.clone(),
                        },
                    )),
                    Some(_) => {}
                    None => {
                        seen.insert(key, p.id.clone());
                    }
                }
            }
            if bound {
                set.app_bound.push(id);
            }
            if anyapp {
                set.any_app.push(id);
            }
            for c in FocusContext::ALL {
                if ctxs[c.index()] {
                    set.by_context[c.index()].push(id);
                }
            }
            set.by_id.insert(p.id.clone(), id);
            set.profiles.push(p);
        }
        (set, diags)
    }

    pub fn len(&self) -> usize {
        self.profiles.len()
    }

    /// PF-2 (F-7): entries of `profiles.rows` that name no row, one warning each.
    pub fn unknown_row_entries(&self, rows: &BTreeMap<String, bool>) -> Vec<Diag> {
        rows.keys()
            .filter(|id| {
                self.id_of(id)
                    .and_then(|i| self.get(i))
                    .is_none_or(|p| !p.source.is_row())
            })
            .map(|id| {
                Diag::warning(
                    format!("profiles.rows.{id}"),
                    None,
                    DiagMsg::ProfileInvalid {
                        what: "profiles.rows",
                        why: format!("no row is named `{id}`"),
                    },
                )
            })
            .collect()
    }

    pub fn is_empty(&self) -> bool {
        self.profiles.is_empty()
    }

    pub fn get(&self, id: ProfileId) -> Option<&Arc<Profile>> {
        self.profiles.get(id.index())
    }

    pub fn id_of(&self, profile_id: &str) -> Option<ProfileId> {
        self.by_id.get(profile_id).copied()
    }

    pub fn iter(&self) -> impl Iterator<Item = (ProfileId, &Arc<Profile>)> {
        self.profiles
            .iter()
            .enumerate()
            .map(|(i, p)| (ProfileId(u32::try_from(i).unwrap_or(u32::MAX)), p))
    }

    /// The set with `p` added (an auto-created app profile), or replaced when the id exists.
    #[must_use]
    pub fn with_profile(&self, p: Arc<Profile>) -> (ProfileSet, Vec<Diag>) {
        let (mut rows, mut users): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
        for (_, q) in self.iter() {
            if q.id == p.id {
                continue;
            }
            if q.source.is_row() {
                rows.push(q.clone());
            } else {
                users.push(q.clone());
            }
        }
        if p.source.is_row() {
            rows.push(p);
        } else {
            users.push(p);
        }
        ProfileSet::build(rows, users)
    }

    /// The set without the profile `profile_id`.
    #[must_use]
    pub fn without(&self, profile_id: &str) -> (ProfileSet, Vec<Diag>) {
        let (mut rows, mut users): (Vec<_>, Vec<_>) = (Vec::new(), Vec::new());
        for (_, q) in self.iter() {
            if q.id == profile_id {
                continue;
            }
            if q.source.is_row() {
                rows.push(q.clone());
            } else {
                users.push(q.clone());
            }
        }
        ProfileSet::build(rows, users)
    }

    /// Evaluate the application matchers once for `app` (TR-1/TR-2).
    pub fn for_app(&self, app: &AppFacts<'_>) -> AppCandidates {
        let ids = self
            .app_bound
            .iter()
            .copied()
            .filter(|id| {
                self.get(*id).is_some_and(|p| {
                    p.triggers
                        .iter()
                        .any(|t| t.app().is_some_and(|m| m.matches(app).is_some()))
                })
            })
            .collect();
        AppCandidates { ids }
    }

    /// Every non-manual profile with a trigger matching `facts`, in fold order: layer, then
    /// specificity ascending (the last one of a layer wins a key), ties by id order (the lower id
    /// is later, so it wins).
    pub fn select(&self, app: &AppCandidates, facts: &FocusFacts<'_>) -> Vec<Matched> {
        let mut cand: Vec<ProfileId> = Vec::new();
        cand.extend_from_slice(&app.ids);
        cand.extend_from_slice(&self.any_app);
        for c in facts.contexts.iter() {
            cand.extend_from_slice(&self.by_context[c.index()]);
        }
        cand.sort_unstable();
        cand.dedup();
        let mut out: Vec<Matched> = Vec::with_capacity(cand.len());
        for id in cand {
            let Some(p) = self.get(id) else { continue };
            if p.manual {
                continue;
            }
            let best = p.triggers.iter().filter_map(|t| t.matches(facts)).max();
            if let Some(spec) = best {
                out.push(Matched { id, spec });
            }
        }
        self.sort_matched(&mut out);
        out
    }

    /// `select` with the application candidates computed on the spot (tests, tools).
    pub fn matching(&self, facts: &FocusFacts<'_>) -> Vec<Matched> {
        let cands = facts
            .app
            .as_ref()
            .map(|a| self.for_app(a))
            .unwrap_or_default();
        self.select(&cands, facts)
    }

    pub(crate) fn sort_matched(&self, out: &mut [Matched]) {
        out.sort_by(|a, b| {
            let la = self.get(a.id).map_or(0, |p| p.source.layer_rank());
            let lb = self.get(b.id).map_or(0, |p| p.source.layer_rank());
            la.cmp(&lb)
                .then_with(|| a.spec.cmp(&b.spec))
                .then_with(|| b.id.cmp(&a.id))
        });
    }

    /// Profiles (user, non-manual, enabled in file) that have an App trigger matching `app`.
    pub fn app_profiles(&self, app: &AppFacts<'_>) -> Vec<ProfileId> {
        self.iter()
            .filter(|(_, p)| {
                p.source == ProfileSource::User
                    && !p.manual
                    && p.triggers
                        .iter()
                        .any(|t| matches!(t, Trigger::App { app: m } if m.matches(app).is_some()))
            })
            .map(|(id, _)| id)
            .collect()
    }

    /// Context triggers present for `ctx` (for prewarm: which contexts have any profile).
    pub fn has_context_profiles(&self, ctx: FocusContext) -> bool {
        !self.by_context[ctx.index()].is_empty()
    }

    /// Contexts that at least one profile names.
    pub fn named_contexts(&self) -> ContextSet {
        FocusContext::ALL
            .into_iter()
            .filter(|c| self.has_context_profiles(*c))
            .fold(ContextSet::EMPTY, ContextSet::with)
    }
}

/// Reload state for the profiles directory: parses files, keeps the last good parse of each
/// (PC-0), skips unchanged texts.
#[derive(Debug, Default)]
pub struct ProfileLoader {
    last: BTreeMap<String, Arc<Profile>>,
    unparsed: std::collections::BTreeSet<String>,
}

impl ProfileLoader {
    pub fn new() -> Self {
        Self::default()
    }

    /// `files` are `(file stem, text)` of the directory. Returns the profiles to compile and the
    /// diagnostics as `(file stem, diag)`. A file that fails to parse keeps its last good
    /// version; one that never parsed is skipped; a file that vanished drops out.
    pub fn reload(
        &mut self,
        files: &[(String, String)],
        ext: &ExtSchema,
    ) -> (Vec<Arc<Profile>>, Vec<(String, Diag)>) {
        let mut diags: Vec<(String, Diag)> = Vec::new();
        let mut sorted: Vec<&(String, String)> = files.iter().collect();
        sorted.sort_by(|a, b| a.0.cmp(&b.0));
        if sorted.len() > MAX_PROFILE_FILES {
            sorted.truncate(MAX_PROFILE_FILES);
            diags.push((
                String::new(),
                Diag::error(
                    "profiles",
                    None,
                    DiagMsg::ProfileLimit {
                        what: "profile files",
                        limit: MAX_PROFILE_FILES,
                    },
                ),
            ));
        }
        let mut next: BTreeMap<String, Arc<Profile>> = BTreeMap::new();
        let mut unparsed = std::collections::BTreeSet::new();
        for (id, text) in sorted {
            let hash = crate::diag::fnv1a(text.as_bytes());
            // The extension schema may have changed, so unchanged text is reparsed only when
            // the profile has inactive keys or the caller passes a fresh loader; the parse is
            // cheap (one TOML document of at most 1 MiB), so always reparse.
            let parsed = parse_user_profile(id, text, ext);
            for d in parsed.diags {
                diags.push((id.clone(), d));
            }
            if let Some(mut p) = parsed.profile {
                p.hash = hash;
                next.insert(id.clone(), Arc::new(p));
            } else {
                unparsed.insert(id.clone());
                if let Some(prev) = self.last.get(id) {
                    diags.push((
                        id.clone(),
                        Diag::error(
                            id,
                            None,
                            DiagMsg::KeptLastGood("the profile file does not parse".to_owned()),
                        ),
                    ));
                    next.insert(id.clone(), prev.clone());
                }
            }
        }
        self.last = next;
        self.unparsed = unparsed;
        (self.last.values().cloned().collect(), diags)
    }

    /// Stems of files that did not parse (and have no last good version): reserve them so a
    /// new profile never takes their name (`BaseLayers::reserved_ids`).
    pub fn unparsed_ids(&self) -> &std::collections::BTreeSet<String> {
        &self.unparsed
    }

    pub fn profile(&self, id: &str) -> Option<&Arc<Profile>> {
        self.last.get(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glob::Glob;
    use crate::profiles::file::{RowOrigin, parse_rows};
    use crate::profiles::trigger::{SiteFacts, WindowFacts, WindowKind};

    fn user(id: &str, body: &str) -> Arc<Profile> {
        let r = parse_user_profile(id, body, &ExtSchema::new());
        assert!(
            r.diags.iter().all(|d| d.severity != crate::Severity::Error),
            "{:?}",
            r.diags
        );
        Arc::new(r.profile.unwrap())
    }

    fn app(name: &str) -> FocusFacts<'_> {
        FocusFacts {
            app: Some(AppFacts {
                name,
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        }
    }

    fn ids(set: &ProfileSet, f: &FocusFacts<'_>) -> Vec<String> {
        set.matching(f)
            .iter()
            .filter_map(|m| set.get(m.id).map(|p| p.id.clone()))
            .collect()
    }

    #[test]
    fn app_context_window_and_site_candidates() {
        let a = user(
            "a",
            "[profile]\ntrigger = [{ app = { name = [\"firefox*\"] } }]\n[speech]\nrate = 60\n",
        );
        let w = user(
            "w",
            "[profile]\ntrigger = [{ window = { title = \"*.md\" } }]\n[speech]\nrate = 61\n",
        );
        let c = user(
            "c",
            "[profile]\ntrigger = [{ context = \"terminal\" }]\n[speech]\nrate = 62\n",
        );
        let s = user(
            "s",
            "[profile]\ntrigger = [{ site = { app = { name = [\"firefox*\"] }, host = \"x.org\" } }]\n[speech]\nrate = 63\n",
        );
        let (set, d) = ProfileSet::build(Vec::new(), vec![a, w, c, s]);
        assert!(d.is_empty(), "{d:?}");
        let mut f = app("Firefox");
        assert_eq!(ids(&set, &f), ["a"]);
        f.site = Some(SiteFacts {
            host: "X.org",
            path: None,
        });
        assert_eq!(
            ids(&set, &f),
            ["a", "s"],
            "site is more specific, folds later"
        );
        f.window = Some(WindowFacts {
            title: "notes.md",
            kind: WindowKind::Normal,
        });
        assert_eq!(ids(&set, &f), ["a", "w", "s"]);
        f.contexts = ContextSet::EMPTY.with(FocusContext::Terminal);
        assert_eq!(ids(&set, &f), ["c", "a", "w", "s"]);
        // No app: only application-less triggers.
        let g = FocusFacts {
            contexts: ContextSet::EMPTY.with(FocusContext::Terminal),
            ..FocusFacts::default()
        };
        assert_eq!(ids(&set, &g), ["c"]);
        assert_eq!(
            set.named_contexts(),
            ContextSet::EMPTY.with(FocusContext::Terminal)
        );
    }

    #[test]
    fn manual_profiles_are_never_triggered() {
        let m = user(
            "m",
            "[profile]\nmanual = true\ntrigger = [{ app = { name = [\"x\"] } }]\n[speech]\nrate = 1\n",
        );
        let (set, _) = ProfileSet::build(Vec::new(), vec![m]);
        assert!(set.matching(&app("x")).is_empty());
    }

    #[test]
    fn pm1_specificity_then_prefix_then_id() {
        let long = user(
            "b",
            "[profile]\ntrigger = [{ app = { name = [\"firefox-esr*\"] } }]\n[speech]\nrate = 1\n",
        );
        let short = user(
            "a",
            "[profile]\ntrigger = [{ app = { name = [\"fire*\"] } }]\n[speech]\nrate = 2\n",
        );
        let tie1 = user(
            "t1",
            "[profile]\ntrigger = [{ app = { name = [\"zed\"] } }]\n[speech]\nrate = 3\n",
        );
        let tie2 = user(
            "t2",
            "[profile]\ntrigger = [{ app = { name = [\"ZED\"] } }]\n[speech]\nrate = 4\n",
        );
        let (set, d) = ProfileSet::build(Vec::new(), vec![long, short, tie1, tie2]);
        // Longer literal prefix is more specific: folds later.
        assert_eq!(ids(&set, &app("firefox-esr")), ["a", "b"]);
        // Equal specificity: the lower id folds later (wins), and one diagnostic names both.
        assert_eq!(ids(&set, &app("zed")), ["t2", "t1"]);
        assert_eq!(d.len(), 1, "{d:?}");
        assert!(matches!(&d[0].msg, DiagMsg::SameTrigger { other } if other == "t1"));
    }

    #[test]
    fn layers_order_third_party_then_first_party_then_user() {
        let names = vec![Glob::new("firefox*").unwrap()];
        let text = "[[profile]]\nid = \"r\"\ntriggers = [{ app = { name = [\"firefox*\"] } }]\n[profile.values]\nspeech.punctuation = \"all\"\n";
        let third = parse_rows(
            text,
            RowOrigin::ThirdParty {
                ext: "ext-a",
                app_names: &names,
            },
            &ExtSchema::new(),
        )
        .rows;
        let first = parse_rows(
            text,
            RowOrigin::FirstParty {
                ext: "ext-b",
                app_names: &names,
            },
            &ExtSchema::new(),
        )
        .rows;
        let prod = parse_rows(text, RowOrigin::Product, &ExtSchema::new()).rows;
        let u = user(
            "u",
            "[profile]\ntrigger = [{ app = { name = [\"firefox*\"] } }]\n[speech]\nrate = 1\n",
        );
        let rows: Vec<_> = third
            .into_iter()
            .chain(first)
            .chain(prod)
            .map(Arc::new)
            .collect();
        let (set, _) = ProfileSet::build(rows, vec![u]);
        let got = ids(&set, &app("firefox"));
        assert_eq!(got[0], "ext-a/r", "third-party first (lowest)");
        assert_eq!(got.last().map(String::as_str), Some("u"));
        assert_eq!(got.len(), 4);
    }

    #[test]
    fn limits_and_duplicates() {
        let mut users = Vec::new();
        for i in 0..(MAX_PROFILE_FILES + 5) {
            users.push(user(
                &format!("p{i:04}"),
                "[profile]\nmanual = true\n[speech]\nrate = 1\n",
            ));
        }
        let (set, d) = ProfileSet::build(Vec::new(), users);
        assert_eq!(set.len(), MAX_PROFILE_FILES);
        assert!(
            d.iter()
                .any(|x| matches!(x.msg, DiagMsg::ProfileLimit { .. }))
        );
    }

    #[test]
    fn with_profile_and_without() {
        let a = user(
            "a",
            "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }]\n[speech]\nrate = 1\n",
        );
        let (set, _) = ProfileSet::build(Vec::new(), vec![a]);
        let b = user(
            "b",
            "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }]\n[speech]\nrate = 2\n",
        );
        let (set2, _) = set.with_profile(b);
        assert_eq!(set2.len(), 2);
        assert!(set.id_of("b").is_none() && set2.id_of("b").is_some());
        let (set3, _) = set2.without("a");
        assert_eq!(set3.len(), 1);
        assert_eq!(
            set2.app_profiles(&AppFacts {
                name: "abc",
                exe: None,
                desktop_id: None
            })
            .len(),
            2
        );
    }

    #[test]
    fn pc0_last_good_parse_is_kept_and_vanished_files_drop() {
        let mut l = ProfileLoader::new();
        let ext = ExtSchema::new();
        let good = "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }]\n[speech]\nrate = 33\n"
            .to_owned();
        let (ps, d) = l.reload(
            &[("a".into(), good.clone()), ("b".into(), "[speech\n".into())],
            &ext,
        );
        assert_eq!(ps.len(), 1, "b never parsed: skipped");
        assert!(d.iter().any(|(id, _)| id == "b"));
        // A half-written edit keeps the last good parse.
        let (ps, d) = l.reload(&[("a".into(), "[profile\n".into())], &ext);
        assert_eq!(ps.len(), 1);
        assert_eq!(ps[0].values.len(), 1);
        assert!(
            d.iter()
                .any(|(_, x)| matches!(x.msg, DiagMsg::KeptLastGood(_)))
        );
        // A good edit replaces it; deleting the file drops it.
        let (ps, _) = l.reload(&[("a".into(), good.replace("33", "44"))], &ext);
        assert_eq!(ps.len(), 1);
        let (ps, _) = l.reload(&[], &ext);
        assert!(ps.is_empty());
        assert!(l.profile("a").is_none());
    }

    #[test]
    fn compile_200_profiles_with_4_triggers_is_fast() {
        let mut users = Vec::new();
        for i in 0..200 {
            let body = format!(
                "[profile]\ntrigger = [{{ app = {{ name = [\"app{i}*\"] }} }}, {{ window = {{ title = \"w{i}*\" }} }}, {{ context = \"menu\" }}, {{ site = {{ host = \"h{i}.org\" }} }}]\n[speech]\npitch = {}\n",
                i % 100
            );
            users.push(user(&format!("p{i:03}"), &body));
        }
        let t = std::time::Instant::now();
        let (set, _) = ProfileSet::build(Vec::new(), users);
        let el = t.elapsed();
        assert_eq!(set.len(), 200);
        // P2 gate: compile <= 50 ms (release); generous here because tests run unoptimised.
        assert!(el.as_millis() < 500, "{el:?}");
    }
}
