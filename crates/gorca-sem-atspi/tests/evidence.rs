// SPDX-License-Identifier: GPL-3.0-or-later
//! The `need_states`, `forbid_states` and `need_attrs` evidence of `expect.toml` must reach the
//! normalised record: when an engine exports the evidence (status `ok`), the record carries the
//! matching flag, orientation, `aria-current`, popup, live-region or rare-string value.
//!
//! Rows whose status is not `ok` are skipped: the engine did not export the evidence, there is
//! nothing to normalise (they are the `inexpressible` candidates of the A2 report).

mod common;

use common::{find, load, root, run, status_ok, str_of};
use gorca_model::event::Politeness;
use gorca_model::sem::{Orientation, Popup, Relevant, SemFlags, Sort};
use gorca_sem_atspi::SrcRecord;
use std::collections::BTreeMap;
use toml_edit::{DocumentMut, Item, Table};

fn strings(t: &Table, key: &str) -> Vec<String> {
    t.get(key)
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn need_attrs(t: &Table) -> Vec<(String, String)> {
    t.get("need_attrs")
        .and_then(Item::as_inline_table)
        .map(|it| {
            it.iter()
                .map(|(k, v)| (k.to_owned(), v.as_str().unwrap_or("").to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

/// Check one state nick; `None` when the nick has no counterpart in the record.
fn state_present(r: &SrcRecord, nick: &str) -> Option<bool> {
    let f = r.sem.flags;
    Some(match nick {
        "required" => f.contains(SemFlags::REQUIRED),
        "modal" => f.contains(SemFlags::MODAL),
        "multi-line" => f.contains(SemFlags::MULTILINE),
        "busy" => f.contains(SemFlags::BUSY),
        "multiselectable" => f.contains(SemFlags::MULTISELECTABLE),
        "editable" => f.contains(SemFlags::EDITABLE),
        "read-only" => f.contains(SemFlags::READ_ONLY),
        "focusable" => f.contains(SemFlags::FOCUSABLE),
        "vertical" => r.sem.orientation == Orientation::Vertical,
        "horizontal" => r.sem.orientation == Orientation::Horizontal,
        _ => return None,
    })
}

/// Check one attribute expectation; `None` when it has no counterpart in the record, otherwise
/// the failure text (empty = holds).
fn attr_holds(r: &SrcRecord, key: &str, want: &str) -> Option<String> {
    let text = r.sem.text.as_deref();
    let mut bad = String::new();
    let mut eq = |what: &str, got: String, want: &str| {
        if want != "*" && got != want {
            bad = format!("{what}: want {want:?} got {got:?}");
        } else if want == "*" && got.is_empty() {
            bad = format!("{what}: want any value, got none");
        }
    };
    match key {
        "current" => eq("current", r.sem.current.name().to_owned(), want),
        "haspopup" => {
            let w = if want == "true" { "menu" } else { want };
            if w == "*" {
                if r.sem.popup == Popup::No {
                    bad = String::from("haspopup: want any popup, got none");
                }
            } else {
                eq("haspopup", r.sem.popup.name().to_owned(), w);
            }
        }
        "keyshortcuts" => eq(
            "keyshortcuts",
            text.and_then(|t| t.key_shortcuts.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "valuetext" => eq(
            "valuetext",
            text.and_then(|t| t.value_text.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "roledescription" => eq(
            "roledescription",
            text.and_then(|t| t.role_description.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "placeholder-text" | "placeholder-text|placeholder" => eq(
            "placeholder",
            text.and_then(|t| t.placeholder.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "live" => eq(
            "live",
            match r.sem.live.map(|l| l.politeness) {
                Some(Politeness::Polite) => "polite",
                Some(Politeness::Assertive) => "assertive",
                None => "",
            }
            .to_owned(),
            want,
        ),
        "atomic" => eq(
            "atomic",
            r.sem.live.map_or(String::new(), |l| l.atomic.to_string()),
            want,
        ),
        "relevant" => {
            let got = r.sem.live.map_or(Relevant::empty(), |l| l.relevant);
            let mut w = Relevant::empty();
            for t in want.split_whitespace() {
                match t {
                    "additions" => w |= Relevant::ADDITIONS,
                    "removals" => w |= Relevant::REMOVALS,
                    "text" => w |= Relevant::TEXT,
                    "all" => w = Relevant::all(),
                    _ => {}
                }
            }
            if got != w {
                bad = format!("relevant: want {w:?} got {got:?}");
            }
        }
        "sort" => {
            let w = match want {
                "ascending" => Sort::Ascending,
                "descending" => Sort::Descending,
                "other" => Sort::Other,
                _ => Sort::None,
            };
            if r.sem.sort != w {
                bad = format!("sort: want {w:?} got {:?}", r.sem.sort);
            }
        }
        "colindextext" => eq(
            "colindextext",
            text.and_then(|t| t.col_index_text.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "rowindextext" => eq(
            "rowindextext",
            text.and_then(|t| t.row_index_text.as_deref())
                .unwrap_or("")
                .to_owned(),
            want,
        ),
        "colindex" => eq(
            "colindex",
            r.sem.grid.map_or(String::new(), |g| g.col.to_string()),
            want,
        ),
        "colspan" => eq(
            "colspan",
            r.sem.grid.map_or(String::new(), |g| g.col_span.to_string()),
            want,
        ),
        "orientation" => eq("orientation", r.sem.orientation.name().to_owned(), want),
        _ => return None,
    }
    Some(bad)
}

/// (pattern, engine, evidence key, reason): evidence the engine exports as `ok` but that the
/// record cannot carry.
const EVIDENCE_GAPS: &[(&str, &str, &str, &str)] = &[];

#[test]
fn exported_evidence_reaches_the_record() {
    let text = std::fs::read_to_string(root().join("expect.toml")).expect("expect.toml");
    let doc: DocumentMut = text.parse().expect("parses");
    let patterns = doc["pattern"].as_table().expect("pattern table");

    let mut cache = BTreeMap::new();
    let (mut checked, mut skipped_status, mut unmapped_keys) = (0u32, 0u32, Vec::new());
    let mut failures = Vec::new();

    for (key, item) in patterns {
        let Some(p) = item.as_table() else { continue };
        let page = str_of(p, "page").expect("page");
        let fixture = str_of(p, "fixture").expect("fixture");
        for engine in ["firefox", "chromium", "gtk4"] {
            let Some(e) = p.get(engine).and_then(Item::as_table) else {
                continue;
            };
            let recs = cache
                .entry((engine, page))
                .or_insert_with(|| load(engine, page));
            let Some(rec) = recs.as_deref().and_then(|r| find(r, engine, fixture)) else {
                continue;
            };
            if !status_ok(e) {
                skipped_status += 1;
                continue;
            }
            let got = run(engine, rec);
            for nick in strings(p, "need_states") {
                if let Some(false) = state_present(&got, &nick) {
                    failures.push(format!("{key}/{engine}: state {nick} not carried"));
                }
                checked += u32::from(state_present(&got, &nick).is_some());
            }
            for nick in strings(p, "forbid_states") {
                if let Some(true) = state_present(&got, &nick) {
                    failures.push(format!("{key}/{engine}: forbidden state {nick} carried"));
                }
                checked += u32::from(state_present(&got, &nick).is_some());
            }
            for (k, want) in need_attrs(p) {
                match attr_holds(&got, &k, &want) {
                    Some(bad) if !bad.is_empty() => {
                        let known = EVIDENCE_GAPS
                            .iter()
                            .any(|(pp, ee, kk, _)| *pp == key && *ee == engine && *kk == k);
                        if !known {
                            failures.push(format!("{key}/{engine}: {bad}"));
                        }
                        checked += 1;
                    }
                    Some(_) => checked += 1,
                    None => unmapped_keys.push(k),
                }
            }
        }
    }
    unmapped_keys.sort();
    unmapped_keys.dedup();
    eprintln!(
        "evidence: {checked} checks, {skipped_status} engine rows skipped (evidence not exported), \
         attribute keys read elsewhere or listed unmapped: {unmapped_keys:?}"
    );
    assert!(checked >= 80, "only {checked} evidence checks ran");
    assert!(
        failures.is_empty(),
        "evidence not carried:\n{}",
        failures.join("\n")
    );
}
