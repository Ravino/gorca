// SPDX-License-Identifier: GPL-3.0-or-later
//! Product rows (L4, compiled in) and the assembly of a snapshot's inputs from a load of the
//! global file and the profiles directory (ADR-0016 sections 2.7, 2.9 and 6, slices P4 and P6).
//!
//! Pure: the daemon's config task reads the files and calls [`build_layers`]; the Reader
//! publishes the result.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use crate::diag::Diag;
use crate::patch::{KeyId, SettingsPatch, Value};
use crate::settings::Settings;

use super::compile::{ProfileLoader, ProfileSet};
use super::file::{ExtSchema, Profile, RowOrigin, parse_rows};
use super::snapshot::{BaseLayers, ProfileSnapshot, UserBase};

/// The product row files, one `(name, text)` per file of `crates/gorca-config/product/`.
const PRODUCT_FILES: &[(&str, &str)] = &[("web-apps", include_str!("../../product/web-apps.toml"))];

/// Parse every product file once. A product file that does not parse is a bug caught by
/// `product_rows_are_valid`; at run time its rows are simply absent.
fn parsed() -> &'static Product {
    static ROWS: OnceLock<Product> = OnceLock::new();
    ROWS.get_or_init(|| {
        let mut rows = Vec::new();
        let mut diags = Vec::new();
        for (name, text) in PRODUCT_FILES {
            let r = parse_rows(text, RowOrigin::Product, &ExtSchema::new());
            rows.extend(r.rows.into_iter().map(Arc::new));
            diags.extend(r.diags.into_iter().map(|d| ((*name).to_owned(), d)));
        }
        Product { rows, diags }
    })
}

struct Product {
    rows: Vec<Arc<Profile>>,
    diags: Vec<(String, Diag)>,
}

/// The product rows (layer L4), parsed once.
pub fn product_rows() -> Vec<Arc<Profile>> {
    parsed().rows.clone()
}

/// Problems found while parsing the product files (always empty in a good build).
pub fn product_row_diags() -> &'static [(String, Diag)] {
    &parsed().diags
}

/// The inputs of one snapshot.
#[derive(Clone, Debug)]
pub struct BuiltLayers {
    pub set: Arc<ProfileSet>,
    pub base: Arc<BaseLayers>,
    /// `(file stem or "", diagnostic)` of the profiles directory, the rows and `profiles.rows`.
    pub diags: Vec<(String, Diag)>,
}

/// Fold the pieces of a load into the compiled set and the layers under it: `files` are the
/// `(stem, text)` of the profiles directory, `global` is the sparse layer the global file sets
/// (`Loaded::patch`) and `hash` the hash of its text (`Loaded::hash`).
pub fn build_layers(
    global: &SettingsPatch,
    hash: u64,
    files: &[(String, String)],
    ext: &ExtSchema,
    loader: &mut ProfileLoader,
) -> BuiltLayers {
    let (users, mut diags) = loader.reload(files, ext);
    let (set, set_diags) = ProfileSet::build(product_rows(), users);
    diags.extend(set_diags.into_iter().map(|d| (String::new(), d)));
    let rows_enabled: BTreeMap<String, bool> = KeyId::of("profiles", "rows")
        .and_then(|k| global.get(k))
        .and_then(|v| match v {
            Value::Map(m) => Some(m.clone()),
            Value::Bool(_) | Value::Int(_) | Value::Str(_) | Value::StrList(_) => None,
        })
        .unwrap_or_default();
    diags.extend(
        set.unknown_row_entries(&rows_enabled)
            .into_iter()
            .map(|d| (String::new(), d)),
    );
    let base = BaseLayers {
        defaults: Settings::default(),
        ext_defaults: BTreeMap::new(),
        user: UserBase {
            file: global.clone(),
            ext: BTreeMap::new(),
            hash,
        },
        ext_schema: ext.clone(),
        reserved_ids: loader.unparsed_ids().clone(),
    };
    BuiltLayers {
        set: Arc::new(set),
        base: Arc::new(base),
        diags,
    }
}

/// The snapshot a Reader starts with before the first load: the product rows over the defaults.
pub fn initial_snapshot() -> ProfileSnapshot {
    let (set, _) = ProfileSet::build(product_rows(), Vec::new());
    ProfileSnapshot::new(Arc::new(set), Arc::new(BaseLayers::default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Presets;
    use crate::profiles::file::ProfileSource;
    use crate::profiles::snapshot::ProfileMode;

    #[test]
    fn product_rows_are_valid() {
        assert!(product_row_diags().is_empty(), "{:?}", product_row_diags());
        let rows = product_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, "gorca/vscode");
        assert_eq!(rows[0].source, ProfileSource::Product);
        assert!(rows[0].enabled, "product rows are on automatically (PF-5)");
    }

    #[test]
    fn build_layers_reports_unknown_rows_entries_and_parse_problems() {
        let l = crate::load::load(
            Some("[profiles]\nrows = { \"gorca/nope\" = false }\n"),
            &Presets::builtin(),
        );
        let mut loader = ProfileLoader::new();
        let files = vec![
            (
                "good".to_owned(),
                "schema = 1\n[profile]\nname = \"Good\"\ntrigger = [{ app = { name = [\"good\"] } }]\n"
                    .to_owned(),
            ),
            ("bad".to_owned(), "this is not toml = = =".to_owned()),
        ];
        let b = build_layers(&l.patch, l.hash, &files, &ExtSchema::new(), &mut loader);
        assert!(b.set.id_of("good").is_some());
        assert!(b.set.id_of("bad").is_none());
        assert!(b.set.id_of("gorca/vscode").is_some());
        assert!(b.base.reserved_ids.contains("bad"));
        assert!(b.diags.iter().any(|(f, _)| f == "bad"), "{:?}", b.diags);
        assert!(
            b.diags.iter().any(|(_, d)| d.to_string().contains("nope")),
            "an unknown profiles.rows entry is reported: {:?}",
            b.diags
        );
        assert_eq!(b.base.user.hash, l.hash);
    }

    #[test]
    fn the_initial_snapshot_carries_the_product_rows() {
        let s = initial_snapshot();
        assert!(s.set.id_of("gorca/vscode").is_some());
        let app = crate::profiles::AppFacts {
            name: "Code",
            exe: None,
            desktop_id: None,
        };
        assert!(!s.resolve_app(&app).browse_enabled());
    }

    // ---- P5 parity with the CHR-0 interim table (`app_modes.rs`, deleted in P6) ----------------

    /// The CHR-0 decision, reimplemented from its table: exact application name, compared
    /// ASCII case-insensitively, against the built-in `vscode` row.
    fn legacy_browse_off(app: &str) -> bool {
        [
            "code",
            "visual studio code",
            "code - oss",
            "vscodium",
            "cursor",
        ]
        .iter()
        .any(|a| a.eq_ignore_ascii_case(app))
    }

    fn browse_enabled(s: &ProfileSnapshot, name: &str) -> bool {
        let app = crate::profiles::AppFacts {
            name,
            exe: None,
            desktop_id: None,
        };
        s.resolve_app(&app).browse_enabled()
    }

    fn names() -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for base in [
            "code",
            "visual studio code",
            "code - oss",
            "vscodium",
            "cursor",
        ] {
            v.push(base.to_owned());
            v.push(base.to_ascii_uppercase());
            let mut t = base.to_owned();
            t[..1].make_ascii_uppercase();
            v.push(t);
            v.push(format!("{base} "));
            v.push(format!(" {base}"));
            v.push(format!("{base}x"));
            v.push(format!("x{base}"));
        }
        for other in [
            "Firefox",
            "Signal",
            "chromium",
            "Google Chrome",
            "Code Helper",
            "Visual Studio",
            "",
            "*",
            "code*",
            "c?de",
            // Non-ASCII look-alikes are not the ASCII names (compared exactly).
            "\u{421}ode",
            "\u{ff43}\u{ff4f}\u{ff44}\u{ff45}",
            "CODE\u{307}",
        ] {
            v.push(other.to_owned());
        }
        // A deterministic spread of short names over a small alphabet.
        let alphabet: Vec<char> = "codeCODE -sui".chars().collect();
        let mut x: u32 = 0x9e37_79b9;
        for _ in 0..400 {
            let mut name = String::new();
            x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let len = 1 + (x >> 28) as usize % 7;
            for _ in 0..len {
                x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                name.push(alphabet[(x >> 16) as usize % alphabet.len()]);
            }
            v.push(name);
        }
        v
    }

    #[test]
    fn parity_every_application_gets_the_chr0_decision() {
        let s = initial_snapshot();
        let mut off = 0;
        for n in names() {
            let want_off = legacy_browse_off(&n);
            off += usize::from(want_off);
            assert_eq!(browse_enabled(&s, &n), !want_off, "{n:?}");
        }
        assert!(off >= 15, "the sample exercises the row ({off})");
    }

    #[test]
    fn parity_holds_in_safe_mode_and_under_suspend() {
        // CHR-0 compiled the table in, so it applied in every mode (M-5).
        let active = initial_snapshot();
        for mode in [ProfileMode::Suspended, ProfileMode::Disabled] {
            let s = active.with_mode(mode);
            for n in names() {
                assert_eq!(
                    browse_enabled(&s, &n),
                    !legacy_browse_off(&n),
                    "{mode:?} {n:?}"
                );
            }
        }
    }

    #[test]
    fn parity_holds_after_a_load_with_user_files_that_do_not_touch_the_key() {
        let l = crate::load::load(None, &Presets::builtin());
        let mut loader = ProfileLoader::new();
        let files = vec![(
            "signal-1".to_owned(),
            "schema = 1\n[profile]\nname = \"Signal\"\ntrigger = [{ app = { name = [\"signal\"] } }]\n[speech]\nrate = 70\n"
                .to_owned(),
        )];
        let b = build_layers(&l.patch, l.hash, &files, &ExtSchema::new(), &mut loader);
        assert!(b.diags.is_empty(), "{:?}", b.diags);
        let s = ProfileSnapshot::new(b.set, b.base);
        for n in names() {
            assert_eq!(browse_enabled(&s, &n), !legacy_browse_off(&n), "{n:?}");
        }
    }

    // ---- F-6: the effective cache is tied to the snapshot generation ---------------------------

    #[test]
    fn the_effective_cache_never_serves_a_value_of_an_older_generation() {
        use crate::patch::{KeyId, get_value};
        use crate::profiles::{
            AppFacts, Edit, EditTargetKind, EffectiveCache, FocusFacts, KeyRef, snapshot::AppKey,
        };
        let l = crate::load::load(None, &Presets::builtin());
        let files = vec![(
            "ff".to_owned(),
            "schema = 1\n[profile]\nname = \"ff\"\ntrigger = [{ app = { name = [\"ff\"] } }]\n[speech]\npitch = 70\n"
                .to_owned(),
        )];
        let b = build_layers(
            &l.patch,
            l.hash,
            &files,
            &ExtSchema::new(),
            &mut ProfileLoader::new(),
        );
        let s = ProfileSnapshot::new(b.set, b.base);
        let facts = FocusFacts {
            app: Some(AppFacts {
                name: "ff",
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        };
        let pitch_key = KeyId::of("speech", "pitch").unwrap();
        let pitch = |e: &crate::profiles::Effective| get_value(&e.settings, pitch_key);
        let mut cache = EffectiveCache::new(8);
        let key = s.focus_key_for(&facts);
        assert_eq!(pitch(&cache.get_or_fold(&s, key)), Value::Int(70));
        assert!(cache.contains(&s, key));
        // An unsaved edit that lands in the same profile: same key, new generation.
        let plan = s
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(pitch_key),
                    value: Value::Int(30),
                },
            )
            .unwrap();
        let s3 = s.apply_edit(&facts, &plan).snapshot;
        assert_eq!(
            s3.focus_key_for(&facts),
            key,
            "the key is stable across edits"
        );
        assert!(s3.generation > s.generation);
        assert!(
            !cache.contains(&s3, key),
            "a newer generation empties the cache"
        );
        assert_eq!(pitch(&cache.get_or_fold(&s3, key)), Value::Int(30));
        // A prewarmed value of an older generation is refused, a current one kept.
        let old = Arc::new(s.resolve_focus(key));
        assert!(!cache.insert_prewarmed(&s3, key, old));
        let fresh = Arc::new(s3.resolve_focus(key));
        assert!(cache.insert_prewarmed(&s3, key, fresh));
        // Suspend folds a different value for the same key.
        let s4 = s3.with_mode(ProfileMode::Suspended);
        assert_eq!(s4.focus_key_for(&facts), key);
        assert_eq!(pitch(&cache.get_or_fold(&s4, key)), Value::Int(50));
        let s5 = s4.with_target(&AppKey::of("ff"), EditTargetKind::Global);
        assert!(s5.generation > s4.generation);
        assert_eq!(cache.len(), 1);
    }

    // ---- R-1: a save completes with what was written, not with what is in memory now ------------

    #[test]
    fn an_edit_made_while_a_save_runs_is_not_marked_saved() {
        use crate::patch::KeyId;
        use crate::profiles::{DiskView, Edit, FocusFacts, KeyRef};
        let s = initial_snapshot();
        let facts = FocusFacts::default();
        let rate = KeyId::of("speech", "rate").unwrap();
        let plan = s
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(rate),
                    value: Value::Int(70),
                },
            )
            .unwrap();
        let s = s.apply_edit(&facts, &plan).snapshot;
        let save = s.plan_save(&DiskView::default()).unwrap();
        // While the write runs: another change to the same key and a row toggle.
        let plan = s
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(rate),
                    value: Value::Int(80),
                },
            )
            .unwrap();
        let s = s.apply_edit(&facts, &plan).snapshot;
        let s = s.toggle_enabled("gorca/vscode").unwrap();
        let done = s.complete(&save, save.steps.len());
        // The written value is in the file layer; the later edits are still dirty.
        assert_eq!(done.base.user.file.get(rate), Some(&Value::Int(70)));
        assert_eq!(done.deltas.base.core.get(rate), Some(&Value::Int(80)));
        let rows = KeyId::of("profiles", "rows").unwrap();
        assert!(
            done.deltas.base.core.get(rows).is_some(),
            "the row toggle is still unsaved and would be written by the next save"
        );
        let next = done
            .plan_save(&DiskView {
                global: Some(save.steps[0].text.clone()),
                profiles: std::collections::BTreeMap::default(),
            })
            .unwrap();
        assert!(next.steps[0].text.contains("rate = 80"));
        assert!(next.steps[0].text.contains("gorca/vscode"));
    }

    #[test]
    fn an_existing_empty_profile_file_is_replaced_not_created() {
        use crate::profiles::{DiskView, Edit, FocusFacts, KeyRef};
        let l = crate::load::load(None, &Presets::builtin());
        let files = vec![(
            "ff".to_owned(),
            "schema = 1\n[profile]\nname = \"ff\"\ntrigger = [{ app = { name = [\"ff\"] } }]\n"
                .to_owned(),
        )];
        let b = build_layers(
            &l.patch,
            l.hash,
            &files,
            &ExtSchema::new(),
            &mut ProfileLoader::new(),
        );
        let s = ProfileSnapshot::new(b.set, b.base);
        let facts = FocusFacts {
            app: Some(crate::profiles::AppFacts {
                name: "ff",
                exe: None,
                desktop_id: None,
            }),
            ..FocusFacts::default()
        };
        let plan = s
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(crate::patch::KeyId::of("speech", "rate").unwrap()),
                    value: Value::Int(60),
                },
            )
            .unwrap();
        let s = s.apply_edit(&facts, &plan).snapshot;
        let disk = DiskView {
            global: None,
            profiles: [("ff".to_owned(), files[0].1.clone())]
                .into_iter()
                .collect(),
        };
        let save = s.plan_save(&disk).unwrap();
        assert!(!save.steps[0].create);
        // A profile that has no file yet is a create.
        let s2 = initial_snapshot();
        let plan = s2
            .plan_edit(
                &facts,
                Edit {
                    key: KeyRef::Core(crate::patch::KeyId::of("speech", "rate").unwrap()),
                    value: Value::Int(60),
                },
            )
            .unwrap();
        let s2 = s2.apply_edit(&facts, &plan).snapshot;
        let save = s2.plan_save(&DiskView::default()).unwrap();
        let created: Vec<bool> = save.steps.iter().map(|x| x.create).collect();
        assert_eq!(created, [true]);
    }

    #[test]
    fn the_user_can_switch_the_row_off_or_override_it_per_app() {
        // CHR-0 documented that user layers can override the row; the row id is stable.
        let l = crate::load::load(
            Some("[profiles]\nrows = { \"gorca/vscode\" = false }\n"),
            &Presets::builtin(),
        );
        let b = build_layers(
            &l.patch,
            l.hash,
            &[],
            &ExtSchema::new(),
            &mut ProfileLoader::new(),
        );
        assert!(b.diags.is_empty(), "{:?}", b.diags);
        let s = ProfileSnapshot::new(b.set, b.base);
        assert!(browse_enabled(&s, "Code"), "row disabled by the user");
        let l = crate::load::load(None, &Presets::builtin());
        let files = vec![(
            "code-1".to_owned(),
            "schema = 1\n[profile]\nname = \"Code\"\ntrigger = [{ app = { name = [\"code\"] } }]\n[browse]\nenabled = true\n"
                .to_owned(),
        )];
        let b = build_layers(
            &l.patch,
            l.hash,
            &files,
            &ExtSchema::new(),
            &mut ProfileLoader::new(),
        );
        let s = ProfileSnapshot::new(b.set, b.base);
        assert!(browse_enabled(&s, "Code"), "the app profile beats the row");
        assert!(!browse_enabled(&s, "VSCodium"), "other apps keep the row");
    }
}
