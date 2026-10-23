// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 13: the `host` feature of `gorca-present-ir` is enabled by `gorca-present` only.
//!
//! `host` gates the host-side PresIr pieces (`ExtKinds`, the `ExtNs` binding, `LangTags`). A
//! plugin must never be able to build them, so no other crate (above all the plugin API crates
//! and extensions) may enable it, directly or through a feature forward, and the crate's own
//! `default` features must not include it (feature unification would turn it on everywhere).
//! The check reads the `Cargo.toml` files, so it also works before the feature exists.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

const IR: &str = "gorca-present-ir";
const ALLOWED: &str = "crates/gorca-present/Cargo.toml";

fn has_host(s: &str) -> bool {
    s.contains("\"host\"") || s.contains("/host")
}

/// Violations in one manifest, as `line: text` strings.
pub fn scan(rel: &str, text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut in_ir_table = false;
    let mut in_ir_own_features = false;
    // The IR crate may enable `host` on itself for its own tests: dev-dependencies never reach dependents.
    let own = rel == format!("crates/{IR}/Cargo.toml");
    let mut in_own_dev_deps = false;
    for (i, raw) in text.lines().enumerate() {
        let line = raw.split('#').next().unwrap_or("").trim();
        if line.starts_with('[') {
            in_ir_table = line.contains(IR);
            in_ir_own_features = own && line == "[features]";
            in_own_dev_deps = own && line == "[dev-dependencies]";
            continue;
        }
        if in_own_dev_deps {
            continue;
        }
        let bad = (line.contains(IR) && has_host(line))
            || (in_ir_table && line.starts_with("features") && has_host(line))
            || (in_ir_own_features && line.starts_with("default") && has_host(line));
        if bad && rel != ALLOWED {
            out.push(format!("{rel}:{}: enables `{IR}/host`", i + 1));
        }
    }
    out
}

fn manifests(root: &Path) -> Result<Vec<PathBuf>> {
    let mut out = vec![root.join("Cargo.toml")];
    for group in ["crates", "extensions"] {
        let dir = root.join(group);
        if !dir.exists() {
            continue;
        }
        for e in std::fs::read_dir(&dir)? {
            let p = e?.path().join("Cargo.toml");
            if p.is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    Ok(out)
}

/// Violations over the workspace under `root`.
pub fn scan_workspace(root: &Path) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for m in manifests(root)? {
        let rel = m
            .strip_prefix(root)
            .unwrap_or(&m)
            .to_string_lossy()
            .into_owned();
        let text =
            std::fs::read_to_string(&m).with_context(|| format!("reading {}", m.display()))?;
        out.extend(scan(&rel, &text));
    }
    Ok(out)
}

/// Run rule 13. `true` when clean.
pub fn run() -> Result<bool> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let v = scan_workspace(&root)?;
    if v.is_empty() {
        println!("host feature rule: ok (only gorca-present may enable `{IR}/host`)");
        return Ok(true);
    }
    for p in &v {
        eprintln!("rule 13: {p}");
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_gorca_present_may_enable_host() {
        let inline =
            "[dependencies]\ngorca-present-ir = { workspace = true, features = [\"host\"] }\n";
        assert!(scan(ALLOWED, inline).is_empty());
        assert_eq!(scan("crates/gorca-ext-api/Cargo.toml", inline).len(), 1);
        assert_eq!(scan("extensions/ext-x/Cargo.toml", inline).len(), 1);
    }

    #[test]
    fn table_form_and_feature_forwards_are_caught() {
        let table = "[dependencies.gorca-present-ir]\nworkspace = true\nfeatures = [\"host\"]\n";
        assert_eq!(scan("crates/a/Cargo.toml", table).len(), 1);
        let fwd = "[features]\nhosted = [\"gorca-present-ir/host\"]\n";
        assert_eq!(scan("crates/a/Cargo.toml", fwd).len(), 1);
    }

    #[test]
    fn the_ir_crate_may_not_default_to_host() {
        let own = "[features]\ndefault = [\"host\"]\nhost = []\n";
        assert_eq!(scan("crates/gorca-present-ir/Cargo.toml", own).len(), 1);
        let ok = "[features]\nhost = []\n";
        assert!(scan("crates/gorca-present-ir/Cargo.toml", ok).is_empty());
    }

    #[test]
    fn the_ir_crate_may_enable_host_for_its_own_tests() {
        let own = "[features]\nhost = []\n\n[dev-dependencies]\ngorca-present-ir = { path = \".\", features = [\"host\"] }\n";
        assert!(scan("crates/gorca-present-ir/Cargo.toml", own).is_empty());
        let other =
            "[dev-dependencies]\ngorca-present-ir = { workspace = true, features = [\"host\"] }\n";
        assert_eq!(scan("crates/gorca-x/Cargo.toml", other).len(), 1);
    }

    #[test]
    fn a_plain_dependency_is_fine() {
        let plain = "[dependencies]\ngorca-present-ir = { workspace = true }\n";
        assert!(scan("crates/gorca-ext-api/Cargo.toml", plain).is_empty());
    }

    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert_eq!(scan_workspace(&root).unwrap(), Vec::<String>::new());
    }
}
