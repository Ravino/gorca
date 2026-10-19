// SPDX-License-Identifier: GPL-3.0-or-later
//! P3 tests: the fold against an independent specification oracle, INV-P6, UB-1, layer sets per
//! mode, PE-1 targets, PE-6 hostile names, SG-1..SG-6 and the save plans (ADR-0016 section 6.1).

#![allow(clippy::format_push_string, clippy::type_complexity)]

use std::collections::BTreeMap;
use std::sync::Arc;

use super::*;
use crate::diag::{DiagMsg, Severity, fnv1a};
use crate::glob::Glob;
use crate::load::load;
use crate::patch::{KeyId, Value, get_value};
use crate::presets::Presets;
use crate::settings::Settings;

fn k(path: &str) -> KeyId {
    KeyId::parse(path).unwrap()
}

fn kr(path: &str) -> KeyRef {
    KeyRef::Core(k(path))
}

fn user(id: &str, text: &str) -> Arc<Profile> {
    let r = parse_user_profile(id, text, &ext_schema());
    assert!(
        r.diags.iter().all(|d| d.severity != Severity::Error),
        "{id}: {:?}",
        r.diags
    );
    Arc::new(r.profile.unwrap())
}

fn ext_schema() -> ExtSchema {
    let mut e = ExtSchema::new();
    e.add(
        "ext-foo",
        [
            ("level".to_owned(), ExtType::Int { min: 0, max: 10 }),
            ("on".to_owned(), ExtType::Bool),
        ],
    );
    e
}

fn rows(text: &str, origin: RowOrigin<'_>) -> Vec<Arc<Profile>> {
    let r = parse_rows(text, origin, &ext_schema());
    assert!(r.diags.is_empty(), "{:?}", r.diags);
    r.rows.into_iter().map(Arc::new).collect()
}

fn snapshot_with(
    users: Vec<Arc<Profile>>,
    rows: Vec<Arc<Profile>>,
    global: Option<&str>,
    ext: ExtSchema,
) -> ProfileSnapshot {
    let (set, d) = ProfileSet::build(rows, users);
    assert!(d.iter().all(|x| x.severity != Severity::Error), "{d:?}");
    let l = load(global, &Presets::builtin());
    let base = BaseLayers {
        defaults: Settings::default(),
        ext_defaults: BTreeMap::new(),
        user: UserBase {
            file: l.patch.clone(),
            ext: BTreeMap::new(),
            hash: l.hash,
        },
        ext_schema: ext,
        reserved_ids: std::collections::BTreeSet::new(),
    };
    ProfileSnapshot::new(Arc::new(set), Arc::new(base))
}

fn snapshot(
    users: Vec<Arc<Profile>>,
    rows: Vec<Arc<Profile>>,
    global: Option<&str>,
) -> ProfileSnapshot {
    snapshot_with(users, rows, global, ext_schema())
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

fn val(e: &Effective, path: &str) -> Value {
    get_value(&e.settings, k(path))
}

fn int(e: &Effective, path: &str) -> i64 {
    match val(e, path) {
        Value::Int(n) => n,
        other => panic!("{path} is {other:?}"),
    }
}

fn text(e: &Effective, path: &str) -> String {
    match val(e, path) {
        Value::Str(s) => s,
        other => panic!("{path} is {other:?}"),
    }
}

const FF: &str =
    "[profile]\nname = \"Firefox\"\n[[profile.trigger]]\napp = { name = [\"firefox*\"] }\n";

fn product_row(id: &str, app: &str, key: &str) -> Vec<Arc<Profile>> {
    rows(
        &format!(
            "[[profile]]\nid = \"{id}\"\ntriggers = [{{ app = {{ name = [\"{app}\"] }} }}]\n[profile.values]\n{key}\n"
        ),
        RowOrigin::Product,
    )
}

// ---- layers, origins, modes --------------------------------------------------------------------

#[test]
fn layer_order_and_origins() {
    let names = vec![Glob::new("firefox*").unwrap()];
    let row_text = |v: &str| {
        format!(
            "[[profile]]\nid = \"r\"\ntriggers = [{{ app = {{ name = [\"firefox*\"] }} }}]\n[profile.values]\nspeech.punctuation = \"{v}\"\n"
        )
    };
    let third = rows(
        &row_text("none"),
        RowOrigin::ThirdParty {
            ext: "ext-a",
            app_names: &names,
        },
    );
    let first = rows(
        &row_text("all"),
        RowOrigin::FirstParty {
            ext: "ext-b",
            app_names: &names,
        },
    );
    let all_rows: Vec<_> = third.into_iter().chain(first).collect();
    // Third-party rows are off until the user enables them (L5 `profiles.rows`).
    let s = snapshot(Vec::new(), all_rows.clone(), None);
    let e = s.resolve_for(&app("firefox"));
    assert_eq!(
        text(&e, "speech.punctuation"),
        "all",
        "first-party row alone"
    );
    assert!(matches!(
        e.origin[k("speech.punctuation").index()],
        LayerRef::FirstParty(_)
    ));
    let s = snapshot(
        Vec::new(),
        all_rows.clone(),
        Some("[profiles]\nrows = { \"ext-a/r\" = true }\n"),
    );
    let e = s.resolve_for(&app("firefox"));
    assert_eq!(
        text(&e, "speech.punctuation"),
        "all",
        "L4 first-party beats L3 third-party"
    );
    // Switch the first-party row off: the enabled third-party row shows.
    let s = snapshot(
        Vec::new(),
        all_rows.clone(),
        Some("[profiles]\nrows = { \"ext-a/r\" = true, \"ext-b/r\" = false }\n"),
    );
    let e = s.resolve_for(&app("firefox"));
    assert_eq!(text(&e, "speech.punctuation"), "none");
    assert!(matches!(
        e.origin[k("speech.punctuation").index()],
        LayerRef::ThirdParty(_)
    ));
    // The user base beats rows for a non-default value; a user profile beats the base.
    let s = snapshot(
        Vec::new(),
        all_rows.clone(),
        Some("[speech]\npunctuation = \"most\"\n"),
    );
    let e = s.resolve_for(&app("firefox"));
    assert_eq!(text(&e, "speech.punctuation"), "most");
    assert_eq!(
        e.origin[k("speech.punctuation").index()],
        LayerRef::UserBase
    );
    let u = user("ff", &format!("{FF}[speech]\npunctuation = \"some\"\n"));
    let s = snapshot(
        vec![u],
        all_rows,
        Some("[speech]\npunctuation = \"most\"\n"),
    );
    let e = s.resolve_for(&app("firefox"));
    assert_eq!(text(&e, "speech.punctuation"), "some");
    assert!(matches!(
        e.origin[k("speech.punctuation").index()],
        LayerRef::User(_)
    ));
    // Other applications see none of it.
    assert_eq!(
        text(&s.resolve_for(&app("signal")), "speech.punctuation"),
        "most"
    );
}

#[test]
fn ub1_default_dump_does_not_cancel_rows() {
    let row = product_row("vs", "code", "speech.punctuation = \"all\"");
    let dump = crate::settings::defaults_toml();
    let s = snapshot(Vec::new(), row.clone(), Some(&dump));
    let e = s.resolve_for(&app("code"));
    assert_eq!(
        text(&e, "speech.punctuation"),
        "all",
        "a defaults dump must not cancel the row"
    );
    assert_eq!(
        e.ub1_skipped.as_ref(),
        [kr("speech.punctuation")],
        "the skip is named"
    );
    // A non-default global value does win.
    let s = snapshot(
        Vec::new(),
        row.clone(),
        Some("[speech]\npunctuation = \"none\"\n"),
    );
    assert_eq!(
        text(&s.resolve_for(&app("code")), "speech.punctuation"),
        "none"
    );
    // Privacy keys of the user base are never skipped (rows cannot set them anyway).
    let s = snapshot(Vec::new(), row, Some("[echo]\ntyped_characters = true\n"));
    let e = s.resolve_for(&app("code"));
    assert_eq!(
        e.origin[k("echo.typed_characters").index()],
        LayerRef::UserBase
    );
}

#[test]
fn layer_sets_per_mode() {
    let names = vec![Glob::new("code").unwrap()];
    let prod = product_row("vs", "code", "speech.punctuation = \"all\"");
    let first = rows(
        "[[profile]]\nid = \"x\"\ntriggers = [{ app = { name = [\"code\"] } }]\n[profile.values]\ntext.caret_edge_ms = 500\n",
        RowOrigin::FirstParty {
            ext: "ext-b",
            app_names: &names,
        },
    );
    let u = user(
        "u",
        "[profile]\ntrigger = [{ app = { name = [\"code\"] } }]\n[speech]\npitch = 77\n",
    );
    let s = snapshot(
        vec![u],
        prod.into_iter().chain(first).collect(),
        Some("[speech]\nrate = 33\n"),
    );
    let f = app("code");
    let e = s.resolve_for(&f);
    assert_eq!(
        (
            int(&e, "speech.pitch"),
            int(&e, "text.caret_edge_ms"),
            text(&e, "speech.punctuation")
        ),
        (77, 500, "all".into())
    );
    // Suspended: L0, L1, L2, product rows, L5. Not first-party extension rows, not profiles.
    let e = s.with_mode(ProfileMode::Suspended).resolve_for(&f);
    assert_eq!(int(&e, "speech.pitch"), 50);
    assert_eq!(int(&e, "text.caret_edge_ms"), 100);
    assert_eq!(
        text(&e, "speech.punctuation"),
        "all",
        "product rows apply under suspend"
    );
    assert_eq!(int(&e, "speech.rate"), 33);
    // Safe mode: the same layer set for core keys.
    let e = s.with_mode(ProfileMode::Disabled).resolve_for(&f);
    assert_eq!(
        text(&e, "speech.punctuation"),
        "all",
        "product rows apply in safe mode"
    );
    assert_eq!(int(&e, "speech.pitch"), 50);
    assert_eq!(int(&e, "speech.rate"), 33);
}

#[test]
fn toggle_enabled_for_profiles_and_rows() {
    let names = vec![Glob::new("slack").unwrap()];
    let third = rows(
        "[[profile]]\nid = \"r\"\ntriggers = [{ app = { name = [\"slack\"] } }]\n[profile.values]\nspeech.punctuation = \"all\"\n",
        RowOrigin::ThirdParty {
            ext: "ext-a",
            app_names: &names,
        },
    );
    let u = user(
        "u",
        "[profile]\ntrigger = [{ app = { name = [\"slack\"] } }]\n[speech]\npitch = 77\n",
    );
    let s = snapshot(vec![u], third, None);
    let f = app("slack");
    assert_eq!(int(&s.resolve_for(&f), "speech.pitch"), 77);
    let s2 = s.toggle_enabled("u").unwrap();
    assert_eq!(int(&s2.resolve_for(&f), "speech.pitch"), 50);
    assert!(s2.generation > s.generation);
    assert_eq!(
        int(
            &s2.toggle_enabled("u").unwrap().resolve_for(&f),
            "speech.pitch"
        ),
        77
    );
    // A third-party row is off; the toggle writes `profiles.rows` into the global delta.
    assert_eq!(text(&s.resolve_for(&f), "speech.punctuation"), "some");
    let s3 = s.toggle_enabled("ext-a/r").unwrap();
    assert_eq!(text(&s3.resolve_for(&f), "speech.punctuation"), "all");
    assert!(s3.deltas.base.core.contains(k("profiles.rows")));
    assert!(s.toggle_enabled("nope").is_none());
}

#[test]
fn pins_replace_automatic_selection() {
    let auto = user(
        "auto",
        "[profile]\ntrigger = [{ app = { name = [\"app\"] } }]\n[speech]\npitch = 11\n",
    );
    let man = user("man", "[profile]\nmanual = true\n[speech]\npitch = 22\n");
    let row = product_row("r", "app", "speech.punctuation = \"all\"");
    let s = snapshot(vec![auto, man], row, None);
    let f = app("app");
    let key = AppKey::of("APP");
    assert_eq!(int(&s.resolve_for(&f), "speech.pitch"), 11);
    let pinned = s.with_pin(&key, Some(Pin::Profile("man".into())));
    let e = pinned.resolve_for(&f);
    assert_eq!(
        int(&e, "speech.pitch"),
        22,
        "a manual profile is reachable through the pin"
    );
    assert_eq!(text(&e, "speech.punctuation"), "all", "rows still apply");
    let g = s.with_pin(&key, Some(Pin::GlobalOnly)).resolve_for(&f);
    assert_eq!(
        (int(&g, "speech.pitch"), text(&g, "speech.punctuation")),
        (50, "some".into())
    );
    // A pin to a profile that no longer exists is ignored.
    let dead = s.with_pin(&key, Some(Pin::Profile("gone".into())));
    assert_eq!(int(&dead.resolve_for(&f), "speech.pitch"), 11);
    // Pins are per application.
    assert_eq!(int(&pinned.resolve_for(&app("other")), "speech.pitch"), 50);
    assert_eq!(
        int(&pinned.with_pin(&key, None).resolve_for(&f), "speech.pitch"),
        11
    );
}

#[test]
fn the_volume_floor_clamps_even_hand_built_profiles() {
    let mut p = Profile::new(
        "v",
        "v",
        ProfileSource::User,
        match parse_user_profile(
            "t",
            "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }]\n",
            &ExtSchema::new(),
        )
        .profile
        {
            Some(q) => q.triggers,
            None => Vec::new(),
        },
    );
    p.values.set(k("speech.volume"), Value::Int(5));
    p.values.set(k("speech.pitch"), Value::Int(9));
    let s = snapshot(
        vec![Arc::new(p)],
        Vec::new(),
        Some("[speech]\nvolume = 70\n"),
    );
    let e = s.resolve_for(&app("abc"));
    assert_eq!(
        int(&e, "speech.volume"),
        70,
        "the lower layer's value is kept"
    );
    assert_eq!(int(&e, "speech.pitch"), 9);
}

#[test]
fn stale_keys_resolve_to_the_base_layers() {
    let u = user(
        "u",
        "[profile]\ntrigger = [{ app = { name = [\"abc\"] } }]\n[speech]\npitch = 9\n",
    );
    let s = snapshot(vec![u.clone()], Vec::new(), None);
    let key = s.focus_key_for(&app("abc"));
    assert_eq!(int(&s.resolve_focus(key), "speech.pitch"), 9);
    let s2 = s.reload(s.set.clone(), s.base.clone());
    assert_eq!(
        int(&s2.resolve_focus(key), "speech.pitch"),
        50,
        "key of another epoch"
    );
    assert_eq!(
        int(
            &s2.resolve_focus(s2.focus_key_for(&app("abc"))),
            "speech.pitch"
        ),
        9
    );
}

#[test]
fn keys_are_interned_and_shared() {
    let u = user(
        "u",
        "[profile]\ntrigger = [{ app = { name = [\"abc*\"] } }]\n[speech]\npitch = 9\n",
    );
    let s = snapshot(vec![u], Vec::new(), None);
    let a = s.focus_key_for(&app("abc"));
    let b = s.focus_key_for(&app("ABCD"));
    assert_eq!(a, b, "different apps with the same match set share a key");
    assert_eq!(s.focus_key_for(&app("zzz")).user, MatchSetId(0));
    let before = s.interner().sets_len();
    let _ = s.focus_key_for(&app("abc"));
    assert_eq!(s.interner().sets_len(), before, "no growth on a hit");
}

// ---- oracle ------------------------------------------------------------------------------------

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % u64::try_from(n).unwrap()).unwrap()
    }

    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }

    fn chance(&mut self, pct: u64) -> bool {
        self.next() % 100 < pct
    }
}

/// Keys the generator sets: (path, toml literal pool).
const FOCUS_KEYS: &[(&str, &[&str])] = &[
    ("speech.pitch", &["10", "40", "50", "90"]),
    ("speech.rate", &["20", "50", "80"]),
    (
        "speech.punctuation",
        &["\"none\"", "\"some\"", "\"most\"", "\"all\""],
    ),
    (
        "verbosity.level",
        &["\"brief\"", "\"normal\"", "\"verbose\""],
    ),
    ("text.caret_edge_ms", &["100", "200", "300"]),
    ("editing.word", &["\"auto\"", "\"fine\"", "\"coarse\""]),
    ("echo.typed_words", &["true", "false"]),
];
const VENDOR_KEYS: &[usize] = &[2, 3, 4, 5];
const NAMES: &[&str] = &["abc", "abcd", "xyz", "Abc"];
const PATTERNS: &[&str] = &["abc", "abc*", "ab*", "xyz", "*"];

struct Gen {
    users: Vec<(String, String)>,
    rows: Vec<(String, String, bool)>, // (id, text, third_party)
    global: String,
}

fn gen_triggers(r: &mut Rng) -> String {
    let n = 1 + r.below(2);
    let mut v = Vec::new();
    for _ in 0..n {
        if r.chance(65) {
            // `*` alone has too few literals for add-ons but is fine for users and the product.
            v.push(format!(
                "{{ app = {{ name = [\"{}\"] }} }}",
                r.pick(PATTERNS)
            ));
        } else {
            v.push(format!(
                "{{ context = \"{}\" }}",
                r.pick(&["terminal", "menu"])
            ));
        }
    }
    v.join(", ")
}

fn gen_values(r: &mut Rng, vendor_only: bool, dotted: bool) -> String {
    let n = 1 + r.below(3);
    let mut seen = std::collections::BTreeSet::new();
    let mut out = String::new();
    for _ in 0..n {
        let i = if vendor_only {
            *r.pick(VENDOR_KEYS)
        } else {
            r.below(FOCUS_KEYS.len())
        };
        if !seen.insert(i) {
            continue;
        }
        let (path, pool) = FOCUS_KEYS[i];
        let v = r.pick(pool);
        if dotted {
            out.push_str(&format!("{path} = {v}\n"));
        } else {
            let (s, key) = path.split_once('.').unwrap();
            out.push_str(&format!("[{s}]\n{key} = {v}\n"));
        }
    }
    out
}

fn generate(r: &mut Rng) -> Gen {
    let mut users = Vec::new();
    for i in 0..r.below(6) {
        let manual = r.chance(15);
        let enabled = r.chance(85);
        let mut body = format!(
            "[profile]\nenabled = {enabled}\nmanual = {manual}\ntrigger = [{}]\n",
            gen_triggers(r)
        );
        // Values come as sections after the header table: group by section to stay valid.
        let mut by_section: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for line in gen_values(r, false, true).lines() {
            let (path, v) = line.split_once(" = ").unwrap();
            let (s, key) = path.split_once('.').unwrap();
            by_section
                .entry(s.to_owned())
                .or_default()
                .push(format!("{key} = {v}"));
        }
        for (s, lines) in by_section {
            body.push_str(&format!("[{s}]\n{}\n", lines.join("\n")));
        }
        users.push((format!("u{i}"), body));
    }
    let mut rows = Vec::new();
    for i in 0..r.below(4) {
        let third = r.chance(50);
        let pats = ["abc*", "xyz", "abcd"];
        let p = r.pick(&pats);
        let id = format!("r{i}");
        let t = format!(
            "[[profile]]\nid = \"{id}\"\ntriggers = [{{ app = {{ name = [\"{p}\"] }} }}]\n[profile.values]\n{}",
            gen_values(r, true, true)
        );
        rows.push((id, t, third));
    }
    let mut global = String::new();
    let mut by_section: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if r.chance(60) {
        for line in gen_values(r, false, true).lines() {
            let (path, v) = line.split_once(" = ").unwrap();
            let (s, key) = path.split_once('.').unwrap();
            by_section
                .entry(s.to_owned())
                .or_default()
                .push(format!("{key} = {v}"));
        }
    }
    for (s, lines) in by_section {
        global.push_str(&format!("[{s}]\n{}\n", lines.join("\n")));
    }
    // Enable or disable some rows.
    let mut map = Vec::new();
    for (id, _, third) in &rows {
        if r.chance(50) {
            let prefix = if *third { "ext-t" } else { "gorca" };
            map.push(format!("\"{prefix}/{id}\" = {}", r.chance(60)));
        }
    }
    if !map.is_empty() {
        global.push_str(&format!("[profiles]\nrows = {{ {} }}\n", map.join(", ")));
    }
    Gen {
        users,
        rows,
        global,
    }
}

fn build_from(g: &Gen) -> ProfileSnapshot {
    let names = vec![
        Glob::new("abc*").unwrap(),
        Glob::new("xyz").unwrap(),
        Glob::new("abcd").unwrap(),
    ];
    let mut all_rows = Vec::new();
    for (_, text, third) in &g.rows {
        let origin = if *third {
            RowOrigin::ThirdParty {
                ext: "ext-t",
                app_names: &names,
            }
        } else {
            RowOrigin::Product
        };
        all_rows.extend(rows(text, origin));
    }
    let users = g.users.iter().map(|(id, t)| user(id, t)).collect();
    snapshot(users, all_rows, Some(&g.global))
}

/// The specification of the fold written key by key from the highest layer down, sharing no
/// code with `ProfileSnapshot::fold`.
fn oracle(s: &ProfileSnapshot, facts: &FocusFacts<'_>) -> BTreeMap<String, Value> {
    let name = facts.app.as_ref().map_or("", |a| a.name);
    let lname = name.to_ascii_lowercase();
    // Matching, by hand: patterns are `lit`, `lit*` or `*`.
    let m = |p: &Profile| -> Option<(u8, usize)> {
        let mut best: Option<(u8, usize)> = None;
        for t in &p.triggers {
            let hit = match t {
                Trigger::App { app } => {
                    let pat = app.name[0].as_str().to_ascii_lowercase();
                    let (lit, star) = pat
                        .strip_suffix('*')
                        .map_or((pat.as_str(), false), |l| (l, true));
                    let ok = if star {
                        lname.starts_with(lit)
                    } else {
                        lname == lit
                    };
                    ok.then_some((1u8, lit.len()))
                }
                Trigger::Context { ctx } => facts.contexts.contains(*ctx).then_some((0u8, 0usize)),
                Trigger::Window { .. } | Trigger::Site { .. } => None,
            };
            if let Some(h) = hit {
                best = Some(best.map_or(h, |b| b.max(h)));
            }
        }
        best
    };
    let pin = facts
        .app
        .as_ref()
        .and_then(|a| s.pins.0.get(&AppKey::of(a.name)));
    let mut rows: Vec<(u8, (u8, usize), String, Arc<Profile>)> = Vec::new();
    let mut users: Vec<(u8, (u8, usize), String, Arc<Profile>)> = Vec::new();
    for (_, p) in s.set.iter() {
        if p.manual {
            continue;
        }
        if let Some(sp) = m(p) {
            let entry = (p.source.layer_rank(), sp, p.id.clone(), p.clone());
            if p.source == ProfileSource::User {
                users.push(entry);
            } else {
                rows.push(entry);
            }
        }
    }
    let order = |a: &(u8, (u8, usize), String, Arc<Profile>),
                 b: &(u8, (u8, usize), String, Arc<Profile>)| {
        a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(b.2.cmp(&a.2))
    };
    rows.sort_by(order);
    users.sort_by(order);
    match pin {
        Some(Pin::GlobalOnly) => {
            rows.clear();
            users.clear();
        }
        Some(Pin::Profile(id)) => {
            if let Some(p) = s.set.id_of(id).and_then(|i| s.set.get(i)) {
                users = vec![(2, (0, 0), p.id.clone(), p.clone())];
            }
        }
        None => {}
    }
    let mode = s.mode;
    // Highest priority first.
    let mut layers: Vec<BTreeMap<KeyId, Value>> = Vec::new();
    if mode == ProfileMode::Active {
        for (_, _, _, p) in users.iter().rev() {
            if !s.is_enabled(p) {
                continue;
            }
            if let Some(d) = s.deltas.profiles.get(&p.id) {
                layers.push(d.core.iter().map(|(k, v)| (k, v.clone())).collect());
            }
            layers.push(p.values.iter().map(|(k, v)| (k, v.clone())).collect());
        }
    }
    layers.push(
        s.deltas
            .base
            .core
            .iter()
            .map(|(k, v)| (k, v.clone()))
            .collect(),
    );
    let defaults = &s.base.defaults;
    layers.push(
        s.base
            .user
            .file
            .iter()
            .filter(|(k, v)| k.def().privacy || get_value(defaults, *k) != **v)
            .map(|(k, v)| (k, v.clone()))
            .collect(),
    );
    for (_, _, _, p) in rows.iter().rev() {
        let allowed = match &p.source {
            ProfileSource::Product => true,
            ProfileSource::FirstParty { .. } | ProfileSource::ThirdParty { .. } => {
                mode == ProfileMode::Active
            }
            ProfileSource::User => false,
        };
        let on = match &p.source {
            ProfileSource::ThirdParty { .. } => s.rows_map().get(&p.id).copied().unwrap_or(false),
            _ => s.rows_map().get(&p.id).copied().unwrap_or(p.enabled),
        };
        if allowed && on {
            layers.push(p.values.iter().map(|(k, v)| (k, v.clone())).collect());
        }
    }
    let mut out = BTreeMap::new();
    for key in KeyId::all() {
        let v = layers
            .iter()
            .find_map(|l| l.get(&key).cloned())
            .unwrap_or_else(|| get_value(defaults, key));
        out.insert(key.def().path(), v);
    }
    out
}

fn random_facts<'a>(r: &mut Rng, name: &'a str) -> FocusFacts<'a> {
    let mut ctx = ContextSet::EMPTY;
    if r.chance(40) {
        ctx = ctx.with(FocusContext::Terminal);
    }
    if r.chance(40) {
        ctx = ctx.with(FocusContext::Menu);
    }
    FocusFacts {
        app: Some(AppFacts {
            name,
            exe: None,
            desktop_id: None,
        }),
        contexts: ctx,
        ..FocusFacts::default()
    }
}

#[test]
fn the_fold_equals_the_specification_oracle() {
    let mut r = Rng(0x2545_f491_4f6c_dd1d);
    let mut checked = 0;
    let mut non_trivial = 0;
    for round in 0..400 {
        let g = generate(&mut r);
        let mut s = build_from(&g);
        // Random unsaved edits through the real edit path, a random mode and pins.
        for _ in 0..r.below(4) {
            let name = *r.pick(NAMES);
            let f = random_facts(&mut r, name);
            let (path, pool) = FOCUS_KEYS[r.below(FOCUS_KEYS.len())];
            let lit = r.pick(pool);
            let doc: toml_edit::DocumentMut = format!("v = {lit}").parse().unwrap();
            let v = match doc["v"].as_value() {
                Some(toml_edit::Value::Boolean(b)) => Value::Bool(*b.value()),
                Some(toml_edit::Value::Integer(n)) => Value::Int(*n.value()),
                Some(toml_edit::Value::String(t)) => Value::Str(t.value().clone()),
                _ => continue,
            };
            if r.chance(25) {
                s = s.with_target(&AppKey::of(name), EditTargetKind::Global);
            }
            if let Ok(plan) = s.plan_edit(
                &f,
                Edit {
                    key: kr(path),
                    value: v,
                },
            ) {
                s = s.apply_edit(&f, &plan).snapshot;
            }
        }
        match r.below(6) {
            0 => s = s.with_mode(ProfileMode::Suspended),
            1 => s = s.with_mode(ProfileMode::Disabled),
            _ => {}
        }
        if r.chance(25) {
            let name = *r.pick(NAMES);
            let pin = if r.chance(50) || s.set.is_empty() {
                Pin::GlobalOnly
            } else {
                let ids: Vec<_> = s
                    .set
                    .iter()
                    .filter(|(_, p)| p.source == ProfileSource::User)
                    .map(|(_, p)| p.id.clone())
                    .collect();
                if ids.is_empty() {
                    Pin::GlobalOnly
                } else {
                    Pin::Profile(r.pick(&ids).clone())
                }
            };
            s = s.with_pin(&AppKey::of(name), Some(pin));
        }
        for name in NAMES {
            let f = random_facts(&mut r, name);
            let got = s.resolve_for(&f);
            let want = oracle(&s, &f);
            for key in KeyId::all() {
                let p = key.def().path();
                assert_eq!(
                    get_value(&got.settings, key),
                    want[&p],
                    "round {round} app {name} key {p} mode {:?}",
                    s.mode
                );
            }
            if got.settings != Settings::default() {
                non_trivial += 1;
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 1600);
    assert!(
        non_trivial > 800,
        "the generator must exercise the layers: {non_trivial}"
    );
}

// ---- INV-P6 ------------------------------------------------------------------------------------

#[test]
fn app_and_doc_keys_equal_the_focus_fold_for_the_same_facts() {
    let mut r = Rng(0x9e37_79b9_7f4a_7c15);
    for round in 0..200 {
        // Profiles of every trigger kind; the matrix decides which keys each may hold.
        let mut users = Vec::new();
        let kinds = [
            (
                "{ app = { name = [\"abc*\"] } }",
                "[terminal]\nsettle_ms = 77\n[browse]\nsay_all_on_load = false\n[speech]\npitch = 12\n",
            ),
            (
                "{ site = { app = { name = [\"abc*\"] }, host = \"x.org\" } }",
                "[browse]\nsay_all_on_load = false\n[speech]\npitch = 13\n",
            ),
            (
                "{ window = { title = \"*.md\" } }",
                "[speech]\npitch = 14\n",
            ),
            ("{ context = \"terminal\" }", "[speech]\npitch = 15\n"),
        ];
        for (i, (trig, body)) in kinds.iter().enumerate() {
            if r.chance(70) {
                users.push(user(
                    &format!("p{i}"),
                    &format!("[profile]\ntrigger = [{trig}]\n{body}"),
                ));
            }
        }
        let s = snapshot(users, Vec::new(), None);
        for name in ["abc", "xyz"] {
            let a = AppFacts {
                name,
                exe: None,
                desktop_id: None,
            };
            let site = SiteFacts {
                host: "x.org",
                path: None,
            };
            let focus = FocusFacts {
                app: Some(a),
                site: Some(site),
                window: Some(WindowFacts {
                    title: "n.md",
                    kind: WindowKind::Normal,
                }),
                contexts: ContextSet::EMPTY.with(FocusContext::Terminal),
            };
            let f = s.resolve_for(&focus);
            let ap = s.resolve_app(&a);
            let dc = s.resolve_doc(&a, &DocFacts { site: Some(site) });
            for key in KeyId::all() {
                match key.def().scope {
                    crate::KeyScope::App => {
                        assert_eq!(
                            ap.get(key),
                            Some(get_value(&f.settings, key)),
                            "round {round} {}",
                            key.def().path()
                        );
                        assert_eq!(dc.get(key), Some(get_value(&f.settings, key)));
                    }
                    crate::KeyScope::Doc => {
                        assert_eq!(ap.get(key), None);
                        assert_eq!(
                            dc.get(key),
                            Some(get_value(&f.settings, key)),
                            "round {round} {}",
                            key.def().path()
                        );
                    }
                    crate::KeyScope::Focus | crate::KeyScope::Base => {
                        assert_eq!(ap.get(key), None);
                        assert_eq!(dc.get(key), None);
                    }
                }
            }
        }
    }
}

// ---- PE-1 targets and PE-6 names ---------------------------------------------------------------

fn target_id(p: &EditPlan) -> Option<&str> {
    match &p.target {
        Target::Profile { id, .. } => Some(id.as_str()),
        Target::Global => None,
    }
}

fn edit(path: &str, v: i64) -> Edit {
    Edit {
        key: kr(path),
        value: Value::Int(v),
    }
}

fn site_facts<'a>(app: &'a str, host: &'a str) -> FocusFacts<'a> {
    let mut f = app_facts(app);
    f.site = Some(SiteFacts { host, path: None });
    f
}

fn app_facts(name: &str) -> FocusFacts<'_> {
    app(name)
}

fn edit_world() -> ProfileSnapshot {
    let ff = user("ff", &format!("{FF}[speech]\nrate = 45\n"));
    let tg = user(
        "tg",
        "[profile]\ntrigger = [{ site = { app = { name = [\"firefox*\"] }, host = \"web.telegram.org\" } }]\n[speech]\nrate = 66\n",
    );
    let man = user("man", "[profile]\nmanual = true\n[speech]\npitch = 22\n");
    snapshot(vec![ff, tg, man], Vec::new(), None)
}

#[test]
fn pe1_targets_are_exhaustive() {
    let s = edit_world();
    // No focused application: L5.
    let p = s
        .plan_edit(&FocusFacts::default(), edit("speech.rate", 60))
        .unwrap();
    assert!(matches!(p.target, Target::Global) && p.why == TargetWhy::NoApplication);
    // Existing app profile.
    let p = s
        .plan_edit(&app("Firefox"), edit("speech.pitch", 60))
        .unwrap();
    assert_eq!((target_id(&p), p.why), (Some("ff"), TargetWhy::AppProfile));
    assert!(p.announce_new.is_none());
    // Auto-creation with the first edit, announced once.
    let p = s
        .plan_edit(&app("Signal"), edit("speech.rate", 60))
        .unwrap();
    assert_eq!(p.why, TargetWhy::NewAppProfile);
    assert_eq!(p.announce_new.as_deref(), Some("Signal"));
    let Target::Profile {
        id,
        create: Some(created),
    } = &p.target
    else {
        panic!("no profile")
    };
    assert!(id.starts_with("signal-"));
    assert!(created.triggers.len() == 1 && matches!(created.triggers[0], Trigger::App { .. }));
    // PE-1a: a Site profile that supplies the key gets the edit; keys it does not hold go to the app.
    let p = s
        .plan_edit(
            &site_facts("firefox", "web.telegram.org"),
            edit("speech.rate", 60),
        )
        .unwrap();
    assert_eq!(
        (target_id(&p), p.why),
        (Some("tg"), TargetWhy::MoreSpecificSupplier)
    );
    let p = s
        .plan_edit(
            &site_facts("firefox", "web.telegram.org"),
            edit("speech.pitch", 60),
        )
        .unwrap();
    assert_eq!((target_id(&p), p.why), (Some("ff"), TargetWhy::AppProfile));
    // Off the site the same edit goes to the app profile.
    let p = s
        .plan_edit(
            &site_facts("firefox", "example.org"),
            edit("speech.rate", 60),
        )
        .unwrap();
    assert_eq!(target_id(&p), Some("ff"));
    // PE-1b: Base keys and privacy keys are global.
    let p = s
        .plan_edit(&app("firefox"), edit("keyboard.multi_press_ms", 400))
        .unwrap();
    assert!(matches!(p.target, Target::Global) && p.why == TargetWhy::BaseKey);
    let typed = Edit {
        key: kr("echo.typed_words"),
        value: Value::Bool(true),
    };
    let p = s.plan_edit(&app("firefox"), typed.clone()).unwrap();
    assert!(matches!(p.target, Target::Global) && p.why == TargetWhy::PrivacyKey);
    // ... unless a user profile already holds the privacy key (PE-1a logic).
    let ff2 = user("ff", &format!("{FF}[echo]\ntyped_words = false\n"));
    let s2 = snapshot(vec![ff2], Vec::new(), None);
    let p = s2.plan_edit(&app("firefox"), typed).unwrap();
    assert_eq!(target_id(&p), Some("ff"));
    // PE-1c / PE-2: the explicit targets.
    let key = AppKey::of("firefox");
    let sg = s.with_target(&key, EditTargetKind::Global);
    let p = sg
        .plan_edit(&app("firefox"), edit("speech.pitch", 60))
        .unwrap();
    assert!(matches!(p.target, Target::Global) && p.why == TargetWhy::ExplicitGlobal);
    let ss = s.with_target(&key, EditTargetKind::Site);
    let p = ss
        .plan_edit(
            &site_facts("firefox", "web.telegram.org"),
            edit("speech.rate", 60),
        )
        .unwrap();
    assert_eq!((target_id(&p), p.why), (Some("tg"), TargetWhy::SiteProfile));
    let p = ss
        .plan_edit(
            &site_facts("firefox", "example.org"),
            edit("speech.rate", 60),
        )
        .unwrap();
    assert_eq!(p.why, TargetWhy::NewSiteProfile);
    let Target::Profile {
        create: Some(c), ..
    } = &p.target
    else {
        panic!()
    };
    assert!(
        matches!(&c.triggers[0], Trigger::Site { host, .. } if host.literal().as_deref() == Some("example.org"))
    );
    // The site target without a document falls back to the application's profile.
    let p = ss
        .plan_edit(&app("firefox"), edit("speech.pitch", 60))
        .unwrap();
    assert_eq!(target_id(&p), Some("ff"));
    // App-scope keys cannot live in a site profile: they go to the application.
    let p = ss
        .plan_edit(
            &site_facts("firefox", "example.org"),
            edit("terminal.settle_ms", 50),
        )
        .unwrap();
    assert_eq!(target_id(&p), Some("ff"));
    // A pin sends the edit to the pinned profile, manual or not.
    let sp = s.with_pin(&key, Some(Pin::Profile("man".into())));
    let p = sp
        .plan_edit(&app("firefox"), edit("speech.pitch", 61))
        .unwrap();
    assert_eq!((target_id(&p), p.why), (Some("man"), TargetWhy::Pinned));
}

#[test]
fn pe1_refusals_and_the_volume_floor() {
    let s = edit_world();
    assert_eq!(
        s.plan_edit(
            &app("firefox"),
            Edit {
                key: kr("speech.rate"),
                value: Value::Int(500)
            }
        )
        .unwrap_err(),
        EditRefusal::WrongValue
    );
    assert_eq!(
        s.plan_edit(
            &app("firefox"),
            Edit {
                key: KeyRef::Ext("ext.nope.x".into()),
                value: Value::Int(1)
            }
        )
        .unwrap_err(),
        EditRefusal::UnknownKey
    );
    assert_eq!(
        s.plan_edit(
            &app("firefox"),
            Edit {
                key: KeyRef::Ext("ext.ext-foo.level".into()),
                value: Value::Int(99)
            }
        )
        .unwrap_err(),
        EditRefusal::WrongValue
    );
    assert!(
        s.plan_edit(
            &app("firefox"),
            Edit {
                key: KeyRef::Ext("ext.ext-foo.level".into()),
                value: Value::Int(5)
            }
        )
        .is_ok()
    );
    // PC-1: a profile never gets a volume under the floor; the global target does.
    assert_eq!(
        s.plan_edit(&app("firefox"), edit("speech.volume", 10))
            .unwrap_err(),
        EditRefusal::VolumeFloor
    );
    assert!(
        s.plan_edit(&app("firefox"), edit("speech.volume", 20))
            .is_ok()
    );
    let g = s.with_target(&AppKey::of("firefox"), EditTargetKind::Global);
    let p = g
        .plan_edit(&app("firefox"), edit("speech.volume", 10))
        .unwrap();
    assert!(matches!(p.target, Target::Global));
    // Empty or oversized application names cannot become profiles.
    assert_eq!(
        s.plan_edit(&app("   \u{202e}"), edit("speech.pitch", 5))
            .unwrap_err(),
        EditRefusal::UnusableName
    );
}

#[test]
fn p8_a_change_in_one_app_leaves_the_others_alone() {
    let s = snapshot(Vec::new(), Vec::new(), Some("[speech]\nrate = 50\n"));
    let f = app("Firefox");
    let plan = s.plan_edit(&f, edit("speech.rate", 60)).unwrap();
    let a = s.apply_edit(&f, &plan);
    assert!(a.changed);
    let s1 = a.snapshot;
    assert_eq!(int(&s1.resolve_for(&f), "speech.rate"), 60);
    assert_eq!(
        int(&s1.resolve_for(&app("Signal")), "speech.rate"),
        50,
        "Signal is unchanged"
    );
    // A second edit finds the profile the first created.
    let plan2 = s1.plan_edit(&f, edit("speech.pitch", 70)).unwrap();
    assert_eq!(plan2.why, TargetWhy::AppProfile);
    // PE-3: a global edit while the app profile holds the key does not change the focus.
    let g = s1.with_target(&AppKey::of("firefox"), EditTargetKind::Global);
    let pg = g.plan_edit(&f, edit("speech.rate", 55)).unwrap();
    let ag = g.apply_edit(&f, &pg);
    assert!(!ag.changed);
    assert!(
        matches!(ag.held_by, Some(LayerRef::User(_))),
        "{:?}",
        ag.held_by
    );
    assert_eq!(
        int(&ag.snapshot.resolve_for(&app("Signal")), "speech.rate"),
        55
    );
    // Revert clears the deltas and the created profile.
    let r = s1.revert();
    assert_eq!(int(&r.resolve_for(&f), "speech.rate"), 50);
    assert!(r.deltas.is_empty() && r.set.is_empty());
}

#[test]
fn pe6_hostile_names_make_safe_ids_and_literal_triggers() {
    let long = "x".repeat(4096);
    let names = [
        "..",
        "/",
        "a\0b",
        "*",
        "code*",
        "?",
        "\\",
        "\u{202e}gnp",
        "a/../b",
        long.as_str(),
        "Firefox",
    ];
    for n in names {
        let s = snapshot(Vec::new(), Vec::new(), None);
        let f = app(n);
        match s.plan_edit(&f, edit("speech.pitch", 33)) {
            Ok(plan) => {
                let Target::Profile {
                    id,
                    create: Some(p),
                } = &plan.target
                else {
                    continue;
                };
                assert!(
                    crate::profiles::naming::is_valid_file_id(id)
                        && !id.contains('/')
                        && !id.contains(".."),
                    "{n:?} -> {id:?}"
                );
                // The trigger matches that application name and nothing else.
                assert!(p.triggers[0].matches(&f).is_some(), "{n:?}");
                for other in ["anything", "codex", "x", "Firefox2", ""] {
                    if other != n {
                        assert!(
                            p.triggers[0].matches(&app(other)).is_none(),
                            "{n:?} matched {other:?}"
                        );
                    }
                }
                // The new profile survives a round trip through its file text.
                let a = s.apply_edit(&f, &plan);
                let text = crate::save::render_profile(p, &[(kr("speech.pitch"), Value::Int(33))]);
                let back = parse_user_profile(id, &text, &ExtSchema::new());
                assert!(
                    back.diags.iter().all(|d| d.severity != Severity::Error),
                    "{n:?}: {:?}",
                    back.diags
                );
                let back = back.profile.unwrap();
                assert!(back.triggers[0].matches(&f).is_some(), "{n:?}");
                assert_eq!(a.snapshot.resolve_for(&f).settings.speech.pitch, 33);
            }
            Err(e) => assert_eq!(e, EditRefusal::UnusableName, "{n:?}"),
        }
    }
    // Colliding slugs get distinct ids.
    let s = snapshot(Vec::new(), Vec::new(), None);
    let p1 = s.plan_edit(&app("a*"), edit("speech.pitch", 1)).unwrap();
    let s2 = s.apply_edit(&app("a*"), &p1).snapshot;
    let p2 = s2.plan_edit(&app("a?"), edit("speech.pitch", 1)).unwrap();
    assert_ne!(target_id(&p1), target_id(&p2));
}

// ---- saving ------------------------------------------------------------------------------------

fn disk_of(global: Option<&str>, files: &[(&str, &str)]) -> DiskView {
    DiskView {
        global: global.map(str::to_owned),
        profiles: files
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    }
}

const GLOBAL: &str = "# mine\nschema = 1\n[speech]\nrate = 50\npunctuation = \"most\"\n";

fn ff_text() -> String {
    format!("{FF}[speech]\nrate = 45\npitch = 30\n")
}

fn sg_world(extra_users: Vec<Arc<Profile>>) -> (ProfileSnapshot, String) {
    let text = ff_text();
    let ff = user("ff", &text);
    let mut users = vec![ff];
    users.extend(extra_users);
    (snapshot(users, Vec::new(), Some(GLOBAL)), text)
}

fn write_all(plan: &SavePlan, disk: &mut DiskView) {
    for st in &plan.steps {
        match &st.file {
            FileId::Global => disk.global = Some(st.text.clone()),
            FileId::Profile(id) => {
                disk.profiles.insert(id.clone(), st.text.clone());
            }
        }
    }
}

#[test]
fn sg_only_touched_keys_are_copied() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    // Touch pitch only; rate=45 (older, hand-written) is not touched.
    let p = s.plan_edit(&f, edit("speech.pitch", 70)).unwrap();
    let s = s.apply_edit(&f, &p).snapshot;
    assert_eq!(s.touched("ff"), [kr("speech.pitch")].into());
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    assert_eq!(plan.report.copied, [kr("speech.pitch")]);
    assert_eq!(plan.steps.len(), 2);
    assert_eq!(plan.steps[0].file, FileId::Global, "global first (SG-5)");
    assert_eq!(plan.steps[1].file, FileId::Profile("ff".into()));
    let mut d2 = disk.clone();
    write_all(&plan, &mut d2);
    let g = load(d2.global.as_deref(), &Presets::builtin());
    assert!(g.diags.is_empty(), "{:?}", g.diags);
    assert_eq!(g.settings.speech.pitch, 70, "copied");
    assert_eq!(
        g.settings.speech.rate, 50,
        "global rate not overwritten by the untouched 45"
    );
    assert_eq!(
        g.settings.speech.punctuation,
        crate::Punct::Most,
        "nothing else in global changed"
    );
    assert!(
        d2.global.as_deref().unwrap().starts_with("# mine\n"),
        "comments kept"
    );
    let ff = parse_user_profile("ff", &d2.profiles["ff"], &ExtSchema::new())
        .profile
        .unwrap();
    assert_eq!(ff.values.len(), 1, "pitch left the profile, rate stays");
    assert_eq!(ff.values.get(k("speech.rate")), Some(&Value::Int(45)));
    // Completing both steps clears the touched set and the delta.
    let done = s.complete(&plan, 2);
    assert!(
        done.touched("ff").is_empty()
            && done
                .deltas
                .profiles
                .get("ff")
                .is_none_or(LayerDelta::is_empty)
    );
    assert_eq!(int(&done.resolve_for(&f), "speech.pitch"), 70);
    assert_eq!(
        done.base.user.hash,
        fnv1a(d2.global.as_deref().unwrap().as_bytes())
    );
    // Signal (no profile) now sees the global pitch.
    assert_eq!(int(&done.resolve_for(&app("signal")), "speech.pitch"), 70);
}

#[test]
fn sg2_empty_touched_set_writes_nothing() {
    let (s, text) = sg_world(Vec::new());
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let e = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap_err();
    assert!(
        matches!(e, SaveRefusal::NothingChanged(ref n) if n == "Firefox"),
        "{e:?}"
    );
    assert!(matches!(
        s.plan_save_global(&app("firefox"), "nope", false, &disk),
        Err(SaveRefusal::UnknownProfile(_))
    ));
}

#[test]
fn sg3_uninstalled_extension_keys_are_kept_and_named() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    let a = s.apply_edit(
        &f,
        &s.plan_edit(
            &f,
            Edit {
                key: KeyRef::Ext("ext.ext-foo.level".into()),
                value: Value::Int(4),
            },
        )
        .unwrap(),
    );
    let s = a.snapshot;
    let b = s.apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 71)).unwrap());
    let s = b.snapshot;
    // The extension is uninstalled later: reload with a schema that no longer has it.
    let base = BaseLayers {
        ext_schema: ExtSchema::new(),
        ..(*s.base).clone()
    };
    let s = s.reload(s.set.clone(), Arc::new(base));
    assert_eq!(
        s.touched("ff").len(),
        2,
        "the touched set survives a reload that changed nothing"
    );
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    assert_eq!(plan.report.copied, [kr("speech.pitch")]);
    assert_eq!(
        plan.report.kept_not_installed,
        [KeyRef::Ext("ext.ext-foo.level".into())]
    );
    let done = s.complete(&plan, plan.steps.len());
    assert_eq!(
        done.touched("ff"),
        [KeyRef::Ext("ext.ext-foo.level".into())].into(),
        "still touched, still for Firefox only"
    );
    // Only an uninstalled key touched: nothing to do.
    let only = {
        let s0 = snapshot(vec![user("ff", &ff_text())], Vec::new(), Some(GLOBAL));
        let a = s0.apply_edit(
            &f,
            &s0.plan_edit(
                &f,
                Edit {
                    key: KeyRef::Ext("ext.ext-foo.on".into()),
                    value: Value::Bool(true),
                },
            )
            .unwrap(),
        );
        let base = BaseLayers {
            ext_schema: ExtSchema::new(),
            ..(*a.snapshot.base).clone()
        };
        a.snapshot.reload(a.snapshot.set.clone(), Arc::new(base))
    };
    assert!(matches!(
        only.plan_save_global(&app("firefox"), "ff", false, &disk),
        Err(SaveRefusal::NothingChanged(_))
    ));
}

#[test]
fn sg4_equal_values_leave_the_profile_and_nothing_is_reversed() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    // Global punctuation is "most"; the user sets exactly that in Firefox.
    let e = Edit {
        key: kr("speech.punctuation"),
        value: Value::Str("most".into()),
    };
    let s = s.apply_edit(&f, &s.plan_edit(&f, e).unwrap()).snapshot;
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    assert_eq!(plan.report.already_global, [kr("speech.punctuation")]);
    assert!(plan.report.copied.is_empty());
    assert_eq!(plan.steps.len(), 1, "global is not rewritten");
    assert_eq!(plan.steps[0].file, FileId::Profile("ff".into()));
    // A file that only has the default value in global: the default counts as global's value.
    let p2 = s.plan_edit(&f, edit("speech.pitch", 50)).unwrap();
    let s2 = s.apply_edit(&f, &p2).snapshot;
    let plan2 = s2
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    assert_eq!(
        plan2.report.already_global.len(),
        1,
        "only the explicit global value counts"
    );
    assert_eq!(
        plan2.report.copied,
        [kr("speech.pitch")],
        "an explicit default is copied (F-1)"
    );
    // Nothing is ever deleted or reset in global: every key of the old file is still there.
    let s3 = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 66)).unwrap())
        .snapshot;
    let plan3 = s3
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    let g = &plan3.steps[0].text;
    assert!(
        g.contains("rate = 50") && g.contains("punctuation = \"most\"") && g.contains("pitch = 66"),
        "{g}"
    );
}

#[test]
fn sg4_a_changed_global_file_refuses_both_writes() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    let s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 70)).unwrap())
        .snapshot;
    let changed = disk_of(Some("[speech]\nrate = 99\n"), &[("ff", &text)]);
    assert_eq!(
        s.plan_save_global(&app("firefox"), "ff", false, &changed)
            .unwrap_err(),
        SaveRefusal::ChangedOnDisk(FileId::Global)
    );
    let changed_profile = disk_of(
        Some(GLOBAL),
        &[("ff", &format!("{}# hand edit\n", ff_text()))],
    );
    assert_eq!(
        s.plan_save_global(&app("firefox"), "ff", false, &changed_profile)
            .unwrap_err(),
        SaveRefusal::ChangedOnDisk(FileId::Profile("ff".into()))
    );
    assert_eq!(
        s.plan_save_global(&app("firefox"), "ff", false, &disk_of(Some(GLOBAL), &[]))
            .unwrap_err(),
        SaveRefusal::MissingOnDisk("ff".into())
    );
}

#[test]
fn sg5_a_failed_second_write_leaves_both_files_consistent() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    let s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 70)).unwrap())
        .snapshot;
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    // The global write succeeded, the profile write failed.
    let part = s.complete(&plan, 1);
    assert_eq!(
        part.touched("ff"),
        [kr("speech.pitch")].into(),
        "touched kept so the command can be repeated"
    );
    assert_eq!(
        int(&part.resolve_for(&f), "speech.pitch"),
        70,
        "effective settings unchanged"
    );
    assert_eq!(int(&part.resolve_for(&app("signal")), "speech.pitch"), 70);
    assert!(
        part.set
            .get(part.set.id_of("ff").unwrap())
            .unwrap()
            .values
            .contains(k("speech.pitch")),
        "the key is in both"
    );
    // Repeating the command now finds the key already global and only removes it from the profile.
    let mut d2 = disk.clone();
    d2.global = Some(plan.steps[0].text.clone());
    let again = part
        .plan_save_global(&app("firefox"), "ff", false, &d2)
        .unwrap();
    assert_eq!(again.report.already_global, [kr("speech.pitch")]);
    assert_eq!(again.steps.len(), 1);
    // Done 0: nothing changed at all.
    let none = s.complete(&plan, 0);
    assert_eq!(none.touched("ff").len(), 1);
    assert_eq!(none.base.user.hash, s.base.user.hash);
}

#[test]
fn sg_apply_everywhere_removes_only_touched_keys_from_other_profiles() {
    let other = user(
        "other",
        "[profile]\ntrigger = [{ app = { name = [\"signal\"] } }]\n[speech]\npitch = 10\nrate = 11\n",
    );
    let third = user(
        "third",
        "[profile]\ntrigger = [{ app = { name = [\"zed\"] } }]\n[speech]\nrate = 12\n",
    );
    let (s, text) = sg_world(vec![other.clone(), third.clone()]);
    let f = app("firefox");
    let s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 70)).unwrap())
        .snapshot;
    let ot = "[profile]\ntrigger = [{ app = { name = [\"signal\"] } }]\n[speech]\npitch = 10\nrate = 11\n";
    let tt = "[profile]\ntrigger = [{ app = { name = [\"zed\"] } }]\n[speech]\nrate = 12\n";
    let disk = disk_of(Some(GLOBAL), &[("ff", &text), ("other", ot), ("third", tt)]);
    let one = s
        .plan_save_global(&app("firefox"), "ff", false, &disk)
        .unwrap();
    assert_eq!(
        one.steps.len(),
        2,
        "without apply-everywhere other profiles are untouched"
    );
    let plan = s
        .plan_save_global(&app("firefox"), "ff", true, &disk)
        .unwrap();
    assert_eq!(plan.report.other_profiles_changed, ["other"]);
    assert_eq!(plan.steps.len(), 3);
    let mut d2 = disk.clone();
    write_all(&plan, &mut d2);
    let o = parse_user_profile("other", &d2.profiles["other"], &ExtSchema::new())
        .profile
        .unwrap();
    assert!(
        !o.values.contains(k("speech.pitch")) && o.values.contains(k("speech.rate")),
        "rate is not a touched key"
    );
    assert_eq!(d2.profiles["third"], tt);
    let done = s.complete(&plan, 3);
    assert_eq!(
        int(&done.resolve_for(&app("signal")), "speech.pitch"),
        70,
        "now global applies there"
    );
    assert_eq!(int(&done.resolve_for(&app("signal")), "speech.rate"), 11);
}

#[test]
fn plan_save_writes_each_dirty_delta_to_its_own_file() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    // One edit to the app profile, one to global, one new profile.
    let mut s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 70)).unwrap())
        .snapshot;
    s = s.with_target(&AppKey::of("signal"), EditTargetKind::Global);
    s = s
        .apply_edit(
            &app("signal"),
            &s.plan_edit(&app("signal"), edit("speech.rate", 55))
                .unwrap(),
        )
        .snapshot;
    s = s
        .apply_edit(
            &app("zed"),
            &s.plan_edit(&app("zed"), edit("speech.punctuation", 0))
                .unwrap_or_else(|_| s.plan_edit(&app("zed"), edit("speech.pitch", 5)).unwrap()),
        )
        .snapshot;
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s.plan_save(&disk).unwrap();
    let files: Vec<_> = plan.steps.iter().map(|st| st.file.clone()).collect();
    assert!(files.contains(&FileId::Global) && files.contains(&FileId::Profile("ff".into())));
    assert_eq!(plan.steps.len(), 3, "{files:?}");
    let mut d2 = disk.clone();
    write_all(&plan, &mut d2);
    // The new profile's file loads back with its trigger and value.
    let (zid, ztext) = d2
        .profiles
        .iter()
        .find(|(id, _)| id.starts_with("zed-"))
        .unwrap();
    let z = parse_user_profile(zid, ztext, &ExtSchema::new());
    assert!(z.diags.is_empty(), "{:?}", z.diags);
    let z = z.profile.unwrap();
    assert!(
        z.triggers[0].matches(&app("zed")).is_some()
            && z.triggers[0].matches(&app("zedd")).is_none()
    );
    assert_eq!(z.values.get(k("speech.pitch")), Some(&Value::Int(5)));
    assert_eq!(z.name, "zed");
    // After the writes the snapshot has no unsaved edits and the same effective values.
    let before = s.resolve_for(&f);
    let done = s.complete(&plan, plan.steps.len());
    assert!(done.deltas.is_empty(), "{:?}", done.deltas);
    for key in KeyId::all() {
        assert_eq!(
            get_value(&done.resolve_for(&f).settings, key),
            get_value(&before.settings, key)
        );
    }
    assert_eq!(int(&done.resolve_for(&app("zed")), "speech.pitch"), 5);
    assert_eq!(int(&done.resolve_for(&app("anyone")), "speech.rate"), 55);
    // A second plan has nothing to do; a file changed on disk is refused.
    assert_eq!(done.plan_save(&d2).unwrap_err(), SaveRefusal::NothingToSave);
    let bad = disk_of(Some(GLOBAL), &[("ff", &format!("{text}# x\n"))]);
    assert_eq!(
        s.plan_save(&bad).unwrap_err(),
        SaveRefusal::ChangedOnDisk(FileId::Profile("ff".into()))
    );
}

#[test]
fn enabled_toggles_are_saved_as_file_fields_and_row_entries() {
    let (s, text) = sg_world(Vec::new());
    let s = s.toggle_enabled("ff").unwrap();
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s.plan_save(&disk).unwrap();
    let ff = parse_user_profile("ff", &plan.steps[0].text, &ExtSchema::new())
        .profile
        .unwrap();
    assert!(!ff.enabled);
    assert_eq!(ff.values.len(), 2, "values untouched");
    let done = s.complete(&plan, 1);
    assert!(!done.set.get(done.set.id_of("ff").unwrap()).unwrap().enabled);
    assert!(done.deltas.is_empty());
}

#[test]
fn reload_drops_edits_of_keys_that_changed_on_disk() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    let mut s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.pitch", 70)).unwrap())
        .snapshot;
    s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.rate", 61)).unwrap())
        .snapshot;
    // The file changes `pitch` on disk (HR-4): that edit goes, `rate` stays.
    let new_text = text.replace("pitch = 30", "pitch = 31");
    let new_ff = user("ff", &new_text);
    let (set, _) = ProfileSet::build(Vec::new(), vec![new_ff]);
    let r = s.reload(Arc::new(set), s.base.clone());
    let d = r.deltas.profiles.get("ff").unwrap();
    assert!(!d.core.contains(k("speech.pitch")) && d.core.contains(k("speech.rate")));
    assert_eq!(
        r.touched("ff"),
        [kr("speech.rate")].into(),
        "SG-1: a reload that changes a key clears it"
    );
    assert_eq!(int(&r.resolve_for(&f), "speech.pitch"), 31);
    assert_eq!(int(&r.resolve_for(&f), "speech.rate"), 61);
    // A profile whose file vanished loses its edits, but an unsaved created profile survives.
    let signal = r
        .apply_edit(
            &app("signal"),
            &r.plan_edit(&app("signal"), edit("speech.pitch", 3))
                .unwrap(),
        )
        .snapshot;
    let (empty, _) = ProfileSet::build(Vec::new(), Vec::new());
    let gone = signal.reload(Arc::new(empty), signal.base.clone());
    assert!(!gone.deltas.profiles.contains_key("ff"));
    assert_eq!(
        gone.set.len(),
        1,
        "the created Signal profile is still there"
    );
    assert_eq!(int(&gone.resolve_for(&app("signal")), "speech.pitch"), 3);
}

#[test]
fn global_deltas_are_saved_and_reload_hr4_applies_to_l5() {
    let s = snapshot(Vec::new(), Vec::new(), Some(GLOBAL));
    let g = s.with_target(&AppKey::of("x"), EditTargetKind::Global);
    let s1 = g
        .apply_edit(
            &app("x"),
            &g.plan_edit(&app("x"), edit("speech.pitch", 12)).unwrap(),
        )
        .snapshot;
    let plan = s1.plan_save(&disk_of(Some(GLOBAL), &[])).unwrap();
    assert_eq!(plan.steps.len(), 1);
    let l = load(Some(&plan.steps[0].text), &Presets::builtin());
    assert_eq!(l.settings.speech.pitch, 12);
    assert_eq!(l.settings.speech.rate, 50);
    // The file now has a different rate: an unsaved rate edit is dropped, pitch survives.
    let s2 = s1
        .apply_edit(
            &app("x"),
            &s1.plan_edit(&app("x"), edit("speech.rate", 9)).unwrap(),
        )
        .snapshot;
    let l2 = load(Some("[speech]\nrate = 51\n"), &Presets::builtin());
    let base = BaseLayers {
        user: UserBase {
            file: l2.patch,
            ext: BTreeMap::new(),
            hash: l2.hash,
        },
        ..(*s2.base).clone()
    };
    let r = s2.reload(s2.set.clone(), Arc::new(base));
    assert!(
        !r.deltas.base.core.contains(k("speech.rate"))
            && r.deltas.base.core.contains(k("speech.pitch"))
    );
    assert_eq!(int(&r.resolve_for(&app("x")), "speech.rate"), 51);
}

#[test]
fn unsaved_edits_reach_app_and_doc_keys() {
    // SS-1: a pending edit is visible through the App and Doc resolvers exactly as in the Reader.
    let u = user(
        "t",
        "[profile]\ntrigger = [{ app = { name = [\"term\"] } }]\n[terminal]\nsettle_ms = 60\n",
    );
    let s = snapshot(vec![u], Vec::new(), None);
    let a = AppFacts {
        name: "term",
        exe: None,
        desktop_id: None,
    };
    assert_eq!(s.resolve_app(&a).terminal().settle_ms, 60);
    let f = app("term");
    let s = s
        .apply_edit(
            &f,
            &s.plan_edit(&f, edit("terminal.settle_ms", 90)).unwrap(),
        )
        .snapshot;
    assert_eq!(s.resolve_app(&a).terminal().settle_ms, 90);
    assert_eq!(
        s.with_mode(ProfileMode::Suspended)
            .resolve_app(&a)
            .terminal()
            .settle_ms,
        30
    );
    assert_eq!(
        s.resolve_app(&a).get(k("terminal.settle_ms")),
        Some(Value::Int(90))
    );
}

#[test]
fn diagnostics_of_the_fixtures_are_clean() {
    // Guards the fixtures themselves: profile texts used above parse without a diagnostic.
    for t in [ff_text(), FF.to_owned()] {
        let r = parse_user_profile("x", &t, &ext_schema());
        assert!(r.diags.is_empty(), "{:?}", r.diags);
    }
    assert!(matches!(
        DiagMsg::VolumeFloor {
            value: 1,
            floor: 20
        },
        DiagMsg::VolumeFloor { .. }
    ));
}

#[test]
fn f1_a_row_cannot_take_over_after_save_to_global() {
    let row = product_row("r", "firefox*", "speech.punctuation = \"all\"");
    let text = ff_text();
    let s0 = snapshot(vec![user("ff", &text)], row, Some("schema = 1\n"));
    let f = app("firefox");
    let mut s = s0.clone();
    for e in [
        Edit {
            key: kr("speech.punctuation"),
            value: Value::Str("some".into()),
        },
        edit("speech.pitch", 70),
    ] {
        s = s.apply_edit(&f, &s.plan_edit(&f, e).unwrap()).snapshot;
    }
    assert_eq!(text_of(&s, "speech.punctuation"), "some");
    let disk = disk_of(Some("schema = 1\n"), &[("ff", &text)]);
    let plan = s.plan_save_global(&f, "ff", false, &disk).unwrap();
    assert_eq!(plan.report.kept_row_sets, [kr("speech.punctuation")]);
    assert_eq!(plan.report.copied, [kr("speech.pitch")]);
    let done = s.complete(&plan, plan.steps.len());
    assert_eq!(
        text_of(&done, "speech.punctuation"),
        "some",
        "the row did not take over"
    );
    assert_eq!(done.touched("ff"), [kr("speech.punctuation")].into());
}

fn text_of(s: &ProfileSnapshot, path: &str) -> String {
    text(&s.resolve_for(&app("firefox")), path)
}

#[test]
fn f2_an_unsaved_global_edit_is_not_already_global() {
    let (s, text) = sg_world(Vec::new());
    let f = app("firefox");
    let g = s.with_target(&AppKey::of("firefox"), EditTargetKind::Global);
    let g = g
        .apply_edit(&f, &g.plan_edit(&f, edit("speech.rate", 70)).unwrap())
        .snapshot;
    let mut s = g.with_target(&AppKey::of("firefox"), EditTargetKind::App);
    s = s
        .apply_edit(&f, &s.plan_edit(&f, edit("speech.rate", 50)).unwrap())
        .snapshot;
    assert_eq!(int(&s.resolve_for(&f), "speech.rate"), 50);
    let disk = disk_of(Some(GLOBAL), &[("ff", &text)]);
    let plan = s.plan_save_global(&f, "ff", false, &disk).unwrap();
    assert!(plan.report.already_global.is_empty());
    assert_eq!(plan.report.copied, [kr("speech.rate")]);
    let done = s.complete(&plan, plan.steps.len());
    assert_eq!(
        int(&done.resolve_for(&f), "speech.rate"),
        50,
        "Firefox keeps 50"
    );
    assert!(!done.deltas.base.core.contains(k("speech.rate")));
}

#[test]
fn f4_a_new_profile_avoids_names_of_unparsable_files() {
    let s = snapshot(Vec::new(), Vec::new(), None);
    let taken = crate::profiles::naming::file_id("Firefox");
    let mut base = (*s.base).clone();
    base.reserved_ids.insert(taken.clone());
    let s = s.reload(s.set.clone(), Arc::new(base));
    let f = app("Firefox");
    let plan = s.plan_edit(&f, edit("speech.pitch", 3)).unwrap();
    assert_ne!(target_id(&plan), Some(taken.as_str()));
    let s = s.apply_edit(&f, &plan).snapshot;
    let disk = disk_of(None, &[(taken.as_str(), "[broken")]);
    assert!(s.plan_save(&disk).is_ok());
    // The loader reports the stems that did not parse.
    let mut l = ProfileLoader::new();
    l.reload(&[("bad".into(), "[x".into())], &ExtSchema::new());
    assert!(l.unparsed_ids().contains("bad"));
}

#[test]
fn f5_a_mixed_trigger_app_profile_refuses_instead_of_creating_a_twin() {
    let u = user(
        "ff",
        "[profile]\ntrigger = [{ app = { name = [\"firefox*\"] } }, { context = \"menu\" }]\n[speech]\npitch = 5\n",
    );
    let s = snapshot(vec![u], Vec::new(), None);
    let e = s
        .plan_edit(&app("firefox"), edit("terminal.settle_ms", 50))
        .unwrap_err();
    assert!(matches!(e, EditRefusal::NotAllowedHere(_)));
    // Focus keys still go to it.
    assert_eq!(
        target_id(
            &s.plan_edit(&app("firefox"), edit("speech.rate", 50))
                .unwrap()
        ),
        Some("ff")
    );
}

#[test]
fn f7_rows_entries_naming_no_row_are_reported() {
    let row = product_row("vs", "code", "speech.punctuation = \"all\"");
    let s = snapshot(Vec::new(), row, None);
    let map: BTreeMap<String, bool> = [
        ("gorca/vs".to_owned(), false),
        ("gorca/gone".to_owned(), true),
        ("ext-x/y".to_owned(), true),
    ]
    .into();
    let d = s.set.unknown_row_entries(&map);
    assert_eq!(d.len(), 2, "{d:?}");
    assert!(d.iter().all(|x| x.path.starts_with("profiles.rows.")));
}
