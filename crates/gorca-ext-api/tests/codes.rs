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
        "check" => model::check(),
        "press" => model::press(),
        "expand" => model::expand(),
        "select" => model::select(),
        "current" => model::current(),
        "invalid" => model::invalid(),
        "popup" => model::popup(),
        "avail" => model::avail(),
        "name_from" => model::name_from(),
        "orientation" => model::orientation(),
        "sort" => model::sort(),
        "pos_src" => model::pos_src(),
        "sem_flags" => model::sem_flags(),
        "relevant" => model::relevant(),
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
    assert_eq!(REGISTRIES.len(), 22);
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

/// Run `git` in the repository; `None` when git or the ref is unavailable (source tarball,
/// shallow clone), in which case the in-tree tests above remain the check.
fn git(args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

#[test]
fn snapshot_only_grows_relative_to_main() {
    for base_ref in ["main", "origin/main"] {
        let Some(base) = git(&["merge-base", "HEAD", base_ref]) else {
            continue;
        };
        let spec = format!("{}:crates/gorca-ext-api/codes.snapshot", base.trim());
        let Some(old) = git(&["show", &spec]) else {
            // The snapshot did not exist at the merge base: nothing to protect yet.
            return;
        };
        let now = include_str!("../codes.snapshot");
        for l in old
            .lines()
            .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        {
            assert!(
                now.lines().any(|n| n == l),
                "snapshot row `{l}` exists on {base_ref} but was changed or removed here"
            );
        }
        return;
    }
}
