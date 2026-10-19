// SPDX-License-Identifier: GPL-3.0-or-later
//! Golden test of the normaliser on the A2 corpus (`tests/fixtures/sem/{firefox,chromium,gtk4}`
//! driven by `expect.toml`).
//!
//! Every (pattern, engine) pair is normalised from the recorded raw node and compared with the
//! ARIA-correct expectation. A mismatch is
//!
//! * a **gap** when the row is an engine disagreement (`disagree = true`), the engine's evidence
//!   status is not `ok`, or the row is a known fixture artifact: the engine does not export what
//!   a correct result needs, so no normaliser can get it right. Gaps are reported, not failed;
//! * a **failure** otherwise: the normaliser disagrees with the expectation although the engine
//!   exported everything needed.
//!
//! Run with `--nocapture` to see the per-engine pass/gap table and every gap.

mod common;

use std::collections::BTreeMap;
use std::fmt::Write as _;

use common::{Rec, find, load, root, run, status_ok, status_text, str_of};
use gorca_model::sem::{Check, Expand, NameFrom, Press, Select, SemRole, Semantics};
use toml_edit::{DocumentMut, Item};

fn tri(sem: &Semantics, field: &str) -> String {
    match field {
        "check" => match sem.check {
            Check::NotCheckable => "absent",
            Check::Off => "false",
            Check::On => "true",
            Check::Mixed => "mixed",
        },
        "press" => match sem.press {
            Press::NotToggle => "absent",
            Press::Off => "false",
            Press::On => "true",
            Press::Mixed => "mixed",
        },
        "expand" => match sem.expand {
            Expand::NotExpandable => "absent",
            Expand::Collapsed => "false",
            Expand::Expanded => "true",
        },
        "select" => match sem.select {
            Select::NotSelectable => "absent",
            Select::Off => "false",
            Select::On => "true",
        },
        _ => "?",
    }
    .to_owned()
}

/// Rows whose fixture is not evidence about the wire (annotated as suspect in the A2 review).
const FIXTURE_ARTIFACTS: &[(&str, &str)] = &[
    (
        "gtk4.accprop.checked-mixed",
        "GtkCheckButton re-syncs CHECKED from active/inconsistent; the manual MIXED update is overwritten",
    ),
    (
        "gtk4.accprop.expanded",
        "EXPANDABLE is exported but not EXPANDED for a manual EXPANDED=true on a GtkButton",
    ),
];

/// (pattern, engine, field, reason): the engine does not put the needed fact on the wire, although
/// the row's evidence statuses say `ok` (the A2 generator checks states and attributes, not the
/// raw role). Each reason is from the A2 report or read from the recorded node.
const ENGINE_GAPS: &[(&str, &str, &str, &str)] = &[
    (
        "textbox.placeholder-only",
        "firefox",
        "name_from",
        "Gecko exports neither a name-from hint nor the placeholder attribute when the placeholder is the name",
    ),
    (
        "name.input-placeholder",
        "firefox",
        "name_from",
        "same: Gecko cannot say the name came from the placeholder",
    ),
    (
        "gtk4.switch",
        "gtk4",
        "role",
        "GtkSwitch is exported as check-box: the switch role is lost on the wire (A2 report)",
    ),
    (
        "gtk4.searchentry",
        "gtk4",
        "role",
        "GtkSearchEntry is an entry on the wire; nothing says it is a search box",
    ),
    (
        "gtk4.columnview",
        "gtk4",
        "role",
        "GtkColumnView is exported as tree-table, indistinguishable from GtkTreeView",
    ),
    (
        "gtk4.role.navigation",
        "gtk4",
        "role",
        "GTK exports the navigation accessible role as filler (A2 report)",
    ),
    (
        "gtk4.role.main",
        "gtk4",
        "role",
        "GTK exports the main accessible role as filler (A2 report)",
    ),
    (
        "gtk4.role.feed",
        "gtk4",
        "role",
        "GTK exports the feed accessible role as filler (A2 report)",
    ),
    (
        "gtk4.role.search",
        "gtk4",
        "role",
        "GTK exports the search accessible role as form (A2 report)",
    ),
];

/// Expectation rows drafted against ARIA only; the model carries a finer role, so the normalised
/// result is the model role. (pattern, expected role name in the model).
const MODEL_ROLE_OVERRIDES: &[(&str, &str)] = &[
    ("textbox.password", "passwordbox"),
    ("gtk4.entry.password", "passwordbox"),
];

#[derive(Default)]
struct Tally {
    rows: u32,
    pass: u32,
    gap_rows: u32,
    fail_rows: u32,
    missing_node: u32,
    fields_checked: u32,
}

struct Mismatch {
    pattern: String,
    engine: String,
    field: &'static str,
    expected: String,
    got: String,
    gap: Option<String>,
}

#[test]
fn the_normaliser_reproduces_the_expectations_on_the_corpus() {
    let text = std::fs::read_to_string(root().join("expect.toml")).expect("expect.toml");
    let doc: DocumentMut = text.parse().expect("expect.toml parses");
    let patterns = doc["pattern"].as_table().expect("pattern table");

    let mut cache: BTreeMap<(String, String), Option<Vec<Rec>>> = BTreeMap::new();
    let mut tally: BTreeMap<String, Tally> = BTreeMap::new();
    let mut mismatches: Vec<Mismatch> = Vec::new();
    let mut deferred_roles: Vec<(String, String)> = Vec::new();

    for (key, item) in patterns {
        let Some(p) = item.as_table() else { continue };
        let page = str_of(p, "page").expect("page");
        let fixture = str_of(p, "fixture").expect("fixture");
        let disagree = p.get("disagree").and_then(Item::as_bool).unwrap_or(false);
        let artifact = FIXTURE_ARTIFACTS
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, why)| (*why).to_owned());

        for engine in ["firefox", "chromium", "gtk4"] {
            let Some(e) = p.get(engine).and_then(Item::as_table) else {
                continue;
            };
            let t = tally.entry(engine.to_owned()).or_default();
            t.rows += 1;
            let recs = cache
                .entry((engine.to_owned(), page.to_owned()))
                .or_insert_with(|| load(engine, page));
            let Some(rec) = recs.as_deref().and_then(|r| find(r, engine, fixture)) else {
                // The engine does not export the node (status missing-node): nothing to check.
                assert!(
                    status_text(e).contains("missing-node") || recs.is_none(),
                    "{key}/{engine}: fixture {fixture} not in the corpus but status is {}",
                    status_text(e)
                );
                t.missing_node += 1;
                continue;
            };
            let got = run(engine, rec);
            let gap_reason = if let Some(a) = &artifact {
                Some(format!("fixture artifact: {a}"))
            } else if disagree {
                Some("engines disagree".to_owned())
            } else if !status_ok(e) {
                Some(format!("engine status {}", status_text(e)))
            } else {
                None
            };

            let first = mismatches.len();
            let mut row_bad = false;
            let mut push = |field: &'static str, expected: String, gotv: String, t: &mut Tally| {
                t.fields_checked += 1;
                if expected != gotv {
                    row_bad = true;
                    let gap = gap_reason.clone().or_else(|| {
                        ENGINE_GAPS
                            .iter()
                            .find(|(p, e, f, _)| *p == key && *e == engine && *f == field)
                            .map(|(_, _, _, why)| format!("engine gap: {why}"))
                    });
                    mismatches.push(Mismatch {
                        pattern: key.to_owned(),
                        engine: engine.to_owned(),
                        field,
                        expected,
                        got: gotv,
                        gap,
                    });
                }
            };

            // role
            if let Some(role) = str_of(p, "role") {
                let override_role = MODEL_ROLE_OVERRIDES
                    .iter()
                    .find(|(p, _)| *p == key)
                    .map(|(_, r)| *r);
                let want = match override_role.unwrap_or(role) {
                    "label" => Some(SemRole::StaticText),
                    other => SemRole::from_name(other),
                };
                match want {
                    Some(w) => push(
                        "role",
                        w.name().to_owned(),
                        got.sem.role.name().to_owned(),
                        t,
                    ),
                    None => deferred_roles.push((key.to_owned(), role.to_owned())),
                }
            }
            for f in ["check", "press", "expand", "select"] {
                if let Some(want) = str_of(p, f) {
                    push(
                        match f {
                            "check" => "check",
                            "press" => "press",
                            "expand" => "expand",
                            _ => "select",
                        },
                        want.to_owned(),
                        tri(&got.sem, f),
                        t,
                    );
                }
            }
            if let Some(l) = p.get("level").and_then(Item::as_integer) {
                push("level", l.to_string(), got.sem.level.to_string(), t);
            }
            if let Some(pos) = p.get("pos").and_then(Item::as_array) {
                let v: Vec<i64> = pos
                    .iter()
                    .filter_map(toml_edit::Value::as_integer)
                    .collect();
                let want = format!(
                    "{}/{}",
                    v.first().copied().unwrap_or(0),
                    v.get(1).copied().unwrap_or(0)
                );
                let gotv = got.sem.pos.map_or("none".to_owned(), |p| {
                    format!("{}/{}", p.pos, p.size.map_or(0, std::num::NonZeroU32::get))
                });
                push("pos", want, gotv, t);
            }
            if let Some(n) = str_of(p, "name") {
                let gotv = got.name.as_deref().unwrap_or("").to_owned();
                push("name", n.to_owned(), gotv, t);
            }
            if let Some(nf) = str_of(p, "name_from") {
                let want = match nf {
                    "author" => Some(NameFrom::Author),
                    "content" => Some(NameFrom::Content),
                    "placeholder" => Some(NameFrom::Placeholder),
                    "none" => Some(NameFrom::None),
                    // `native` and `tooltip` have no class in the model.
                    _ => None,
                };
                if let Some(w) = want {
                    push(
                        "name_from",
                        w.name().to_owned(),
                        got.sem.name_from.name().to_owned(),
                        t,
                    );
                }
            }

            if !row_bad {
                t.pass += 1;
            } else if mismatches[first..].iter().all(|m| m.gap.is_some()) {
                t.gap_rows += 1;
            } else {
                t.fail_rows += 1;
            }
        }
    }

    // ---- report -------------------------------------------------------------------------------
    let mut out = String::new();
    let _ = writeln!(out, "\nA2 corpus, normaliser golden run");
    for (engine, t) in &tally {
        let _ = writeln!(
            out,
            "  {engine:8} rows {:3}  pass {:3}  gap {:3}  FAIL {:3}  missing-node {:3}  fields {:4}",
            t.rows, t.pass, t.gap_rows, t.fail_rows, t.missing_node, t.fields_checked
        );
    }
    let (gaps, fails): (Vec<_>, Vec<_>) = mismatches.iter().partition(|m| m.gap.is_some());
    let _ = writeln!(out, "  expected gaps ({} field mismatches):", gaps.len());
    for m in &gaps {
        let _ = writeln!(
            out,
            "    GAP  {}/{} {}: want {:?} got {:?}  [{}]",
            m.pattern,
            m.engine,
            m.field,
            m.expected,
            m.got,
            m.gap.as_deref().unwrap_or("")
        );
    }
    for (k, r) in &deferred_roles {
        let _ = writeln!(
            out,
            "    DEFERRED role {r} in {k}: DPUB/graphics roles are not in SemRole yet"
        );
    }
    for m in &fails {
        let _ = writeln!(
            out,
            "    FAIL {}/{} {}: want {:?} got {:?}",
            m.pattern, m.engine, m.field, m.expected, m.got
        );
    }
    eprintln!("{out}");

    assert!(
        tally.values().map(|t| t.rows).sum::<u32>() >= 300,
        "the corpus shrank: {tally:?}",
        tally = tally
            .iter()
            .map(|(k, t)| (k.clone(), t.rows))
            .collect::<Vec<_>>()
    );
    assert!(
        fails.is_empty(),
        "{} unexplained mismatches:\n{out}",
        fails.len()
    );
}
