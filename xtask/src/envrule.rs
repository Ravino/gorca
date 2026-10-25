// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 8: only the daemon's `main.rs` reads the process environment or opens a bus by looking
//! around (`Builder::session()`, `Connection::session()`, ...).
//!
//! Library crates take fully resolved addresses and paths; `None` is an error, never an
//! environment lookup. That is what makes test isolation a property of the code instead of a
//! convention: a test that leaves a field unset fails, it does not reach the user's real session.
//!
//! Scope: non-test sources under `crates/*/src` and `extensions/*/src`. Text after the first
//! `#[cfg(test)]` line of a file is test code and is skipped; `tests/` directories are skipped.
//! Exempt: `crates/gorca-daemon/src/main.rs` (the one reader), the bench crates (B-6 harness included) and the testkit
//! (tools that deliberately inspect their environment, never linked into the daemon), and this
//! crate.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Calls that look at the ambient environment.
const FORBIDDEN: &[&str] = &[
    "env::var(",
    "env::var_os(",
    "env::vars(",
    "env::vars_os(",
    "Builder::session(",
    "Builder::system(",
    "Connection::session(",
    "Connection::system(",
];

/// Repo-relative path prefixes that may read the environment.
const EXEMPT: &[&str] = &[
    "crates/gorca-daemon/src/main.rs",
    "crates/gorca-bench/",
    "crates/gorca-bench-b1/",
    "crates/gorca-bench-b2/",
    "crates/gorca-bench-web/",
    "crates/gorca-testkit/",
    // Test-only module (`#[cfg(test)] mod statemodel` in lib.rs): seed and case-count knobs.
    "crates/gorca-core/src/statemodel/",
    // Test-only module (`#[cfg(test)] mod cutover_gate_tests`): the gate passes surface, output
    // path and shuffle seed in by environment, because `xtask cutover` runs the test binary.
    "crates/gorca-connector-atspi/src/cutover_gate_tests.rs",
    // Test-only module (`#[cfg(test)] mod golden_tests`): `GORCA_BLESS=1` rewrites the golden files.
    "crates/gorca-connector-atspi/src/golden_tests.rs",
    // Test-only module (`#[cfg(test)] mod desktop_golden_tests`): `GORCA_BLESS=1` rewrites the golden files.
    "crates/gorca-connector-atspi/src/desktop_golden_tests.rs",
    // Test-only module (`#[cfg(test)] mod info_golden_tests`): `GORCA_BLESS=1` rewrites the golden files.
    "crates/gorca-connector-atspi/src/info_golden_tests.rs",
    // Test-only module (`#[cfg(test)] mod golden_tests` of browse): `GORCA_BLESS=1` rewrites the files.
    "crates/gorca-core/src/browse/golden_tests.rs",
    "xtask/",
];

#[derive(Debug, Eq, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
    pub call: &'static str,
}

/// Forbidden calls in the non-test part of `source`.
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

fn exempt(rel: &str) -> bool {
    EXEMPT.iter().any(|p| rel.starts_with(p))
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
                if exempt(&rel) {
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

/// Run rule 8 over the repository this crate lives in. `true` when clean.
pub fn run() -> Result<bool> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let hits = scan_workspace(&root)?;
    if hits.is_empty() {
        println!("environment rule: ok (only gorca-daemon/src/main.rs reads the environment)");
        return Ok(true);
    }
    for h in &hits {
        eprintln!(
            "rule 8: {}:{} calls `{}`; only the daemon's main.rs may read the environment",
            h.file, h.line, h.call
        );
    }
    eprintln!("{} environment rule violation(s)", hits.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_reads_and_ambient_buses_are_flagged() {
        let src = "fn a() { let _ = std::env::var(\"X\"); }\n\
                   fn b() { zbus::connection::Builder::session(); }\n\
                   // std::env::var(\"commented\")\n\
                   fn c() { zbus::Connection::system(); }\n";
        let hits = scan("crates/x/src/lib.rs", src);
        assert_eq!(hits.len(), 3, "{hits:?}");
        assert_eq!(hits[0].line, 1);
        assert_eq!(hits[1].call, "Builder::session(");
    }

    #[test]
    fn test_modules_are_skipped() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests { fn t() { std::env::var(\"X\"); } }\n";
        assert!(scan("crates/x/src/lib.rs", src).is_empty());
    }

    #[test]
    fn only_the_daemon_main_is_exempt_among_product_code() {
        assert!(exempt("crates/gorca-daemon/src/main.rs"));
        assert!(!exempt("crates/gorca-daemon/src/lib.rs"));
        assert!(!exempt("crates/gorca-connector-atspi/src/session.rs"));
        assert!(!exempt("crates/gorca-speech-ssip/src/lib.rs"));
    }

    /// The real workspace is clean: this is the check `cargo xtask deps` runs.
    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let hits = scan_workspace(&root).unwrap();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
