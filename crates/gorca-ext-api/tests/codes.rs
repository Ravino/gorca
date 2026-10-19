// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generated registry copy in `src/codes.rs` must equal the `gorca-model` registries, and
//! the checked-in `codes.snapshot` must only ever grow (codes are append-only, ADR-0019 V-3).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gorca_ext_api::codes::{REGISTRIES, Row};
use gorca_model::sem::registry as model;

fn model_rows(name: &str) -> Vec<Row> {
    match name {
        "groups" => model::groups(),
        "evidence" => model::evidence(),
        "prov" => model::prov_codes(),
        "conf" => model::conf_classes(),
        "bind" => model::bind_qualities(),
        "source_class" => model::source_classes(),
        "tier" => model::trust_tiers(),
        "role" => model::roles(),
        other => panic!("registry `{other}` has no model counterpart"),
    }
}

fn snapshot() -> Vec<(String, String, u32)> {
    include_str!("../codes.snapshot")
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| {
            let mut it = l.split_whitespace();
            let (Some(r), Some(n), Some(c), None) = (it.next(), it.next(), it.next(), it.next())
            else {
                panic!("malformed snapshot line `{l}`");
            };
            (r.to_owned(), n.to_owned(), c.parse().unwrap())
        })
        .collect()
}

#[test]
fn copy_equals_the_model_registries() {
    assert_eq!(REGISTRIES.len(), 8);
    for (name, rows) in REGISTRIES {
        assert_eq!(*rows, model_rows(name).as_slice(), "registry `{name}`");
    }
}

#[test]
fn snapshot_is_append_only() {
    // Every snapshot row must still exist, unchanged: no rename, renumbering or removal.
    for (reg, name, code) in snapshot() {
        let rows = REGISTRIES
            .iter()
            .find(|(r, _)| *r == reg)
            .unwrap_or_else(|| panic!("registry `{reg}` was removed"))
            .1;
        assert!(
            rows.contains(&(name.as_str(), code)),
            "snapshot row `{reg} {name} {code}` was changed or removed (codes are append-only)"
        );
    }
}

#[test]
fn snapshot_lists_every_current_row() {
    // A new code must be appended to codes.snapshot in the same change.
    let snap = snapshot();
    for (reg, rows) in REGISTRIES {
        for (name, code) in *rows {
            assert!(
                snap.iter()
                    .any(|(r, n, c)| r == reg && n == name && c == code),
                "`{reg} {name} {code}` is missing from codes.snapshot (append it)"
            );
        }
    }
}

#[test]
fn codes_and_names_are_unique_within_a_registry() {
    for (name, rows) in REGISTRIES {
        let mut codes: Vec<u32> = rows.iter().map(|r| r.1).collect();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), rows.len(), "duplicate code in `{name}`");
        let mut names: Vec<&str> = rows.iter().map(|r| r.0).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), rows.len(), "duplicate name in `{name}`");
    }
}
