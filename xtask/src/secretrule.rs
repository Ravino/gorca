// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 14: one question decides whether a node is a secret.
//!
//! A password field is recognised three ways: the fused role (`SemRole::PasswordBox`), the native
//! role of the snapshot (`Role::PasswordEntry`) and the role of its source record. A sink that
//! asks only one of them leaks when the others disagree (an engine reporting a password over a
//! text-box token). So the question is asked in two places only: `NodeSnapshot::is_sensitive` /
//! `is_secret_node` (`gorca-model`, all three signals) and `gorca_present::is_secret_entry`
//! (the entry). Every speak and log sink (focus, where-am-I, explain, the bench log, the text
//! layer, browse copy and mirror, the extension boundary) calls those.
//!
//! This rule fails on a hand-written comparison with either role in non-test source. Text after the
//! first `#[cfg(test)]` line of a file is skipped, as in rule 8.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Comparisons that decide secrecy by one signal.
const FORBIDDEN: &[&str] = &[
    "== Role::PasswordEntry",
    "!= Role::PasswordEntry",
    "== gorca_model::Role::PasswordEntry",
    "!= gorca_model::Role::PasswordEntry",
    "== SemRole::PasswordBox",
    "!= SemRole::PasswordBox",
    "== gorca_model::sem::SemRole::PasswordBox",
    "!= gorca_model::sem::SemRole::PasswordBox",
];

/// Files that define the question or are waiting for their slice (named, with the reason).
const EXEMPT: &[&str] = &[
    // The definition: all three signals.
    "crates/gorca-model/src/redact.rs",
    // The role resolution of the fusion (an engine password role is never relabelled): it is the
    // source of the fused role, not a sink.
    "crates/gorca-sem-atspi/src/roles.rs",
    // The definition of `is_secret_role` (one comparison); every sink in the file asks
    // `is_secret_entry`.
    "crates/gorca-present/src/compose.rs",
    "xtask/",
];

#[derive(Debug, Eq, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub call: &'static str,
}

/// Forbidden comparisons in the non-test part of `source`.
pub fn scan(file: &str, source: &str) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (i, line) in source.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("#[cfg(test)]") {
            break;
        }
        if t.starts_with("//") {
            continue;
        }
        for call in FORBIDDEN {
            if line.contains(call) {
                hits.push(Hit {
                    file: file.to_owned(),
                    line: i + 1,
                    call,
                });
            }
        }
    }
    hits
}

/// Files that are test code as a whole (`tests.rs`, `*_tests.rs`, a `tests/` directory).
fn is_test_file(rel: &str) -> bool {
    rel.ends_with("tests.rs") || rel.contains("/tests/")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            rust_files(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    Ok(())
}

/// Scan the workspace under `root`.
pub fn scan_workspace(root: &Path) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for group in ["crates", "extensions"] {
        let dir = root.join(group);
        if !dir.exists() {
            continue;
        }
        for krate in std::fs::read_dir(&dir)? {
            let src = krate?.path().join("src");
            if !src.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            rust_files(&src, &mut files)?;
            for f in files {
                let rel = f
                    .strip_prefix(root)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .into_owned();
                if EXEMPT.iter().any(|p| rel.starts_with(p)) || is_test_file(&rel) {
                    continue;
                }
                let text = std::fs::read_to_string(&f)
                    .with_context(|| format!("reading {}", f.display()))?;
                hits.extend(scan(&rel, &text));
            }
        }
    }
    Ok(hits)
}

/// Run rule 14 over the repository this crate lives in. `true` when clean.
pub fn run() -> Result<bool> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let hits = scan_workspace(&root)?;
    if hits.is_empty() {
        println!(
            "secret rule: ok (secrecy is asked through NodeSnapshot::is_sensitive / is_secret_entry only)"
        );
        return Ok(true);
    }
    for h in &hits {
        eprintln!(
            "rule 14: {}:{} decides secrecy with `{}`; ask `NodeSnapshot::is_sensitive`, `gorca_model::is_secret_node` or `gorca_present::is_secret_entry`",
            h.file, h.line, h.call
        );
    }
    eprintln!("{} secret rule violation(s)", hits.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hand_written_comparison_is_flagged_and_test_code_is_not() {
        let src = "fn a(n: &N) -> bool { n.role == Role::PasswordEntry }\n\
                   // n.role == Role::PasswordEntry\n\
                   fn b(e: &E) -> bool { e.sem.role != SemRole::PasswordBox }\n\
                   fn c(r: Role) -> bool { matches!(r, Role::PasswordEntry) }\n\
                   #[cfg(test)]\nmod t { fn x(n: &N) -> bool { n.role == Role::PasswordEntry } }\n";
        let hits = scan("crates/x/src/lib.rs", src);
        assert_eq!(hits.len(), 2, "{hits:?}");
        assert_eq!(hits[0].line, 1);
    }

    /// The real workspace is clean: this is the check `cargo xtask deps` runs.
    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let hits = scan_workspace(&root).unwrap();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
