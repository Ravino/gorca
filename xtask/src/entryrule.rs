// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 11: speech and every other reader of fused semantics reads entries only through
//! `Ivm::entry_view`; `Ivm::entry_unmasked` exists for the fusion engine's own tests.
//!
//! The unmasked entry may still carry the value of a silenced (revoked, quarantined, paused)
//! source until the bounded re-fusion reaches the node; `entry_view` hides such values at read
//! time (M22a-B2 review, decision of 2026-10-19). Calling the unmasked accessor from product code
//! would reopen the hole the mask closes.
//!
//! Scope: non-test sources under `crates/*/src` and `extensions/*/src` outside `gorca-fuse`.
//! Text after the first `#[cfg(test)]` line of a file is test code and is skipped; `tests/` and
//! `benches/` directories are not scanned. Exempt: the fusion engine itself and the bench crates
//! (harnesses that inspect engine state, never linked into the daemon).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The accessor product code must not call.
const BANNED: &str = "entry_unmasked";

/// Repo-relative path prefixes that may use it.
const EXEMPT: &[&str] = &[
    "crates/gorca-fuse/",
    "crates/gorca-bench/",
    "crates/gorca-bench-b1/",
    "crates/gorca-bench-b2/",
    "crates/gorca-bench-web/",
    "xtask/",
];

#[derive(Debug, Eq, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
}

/// Uses of the banned accessor in the non-test part of `source` (comments are skipped).
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
        if line.contains(BANNED) {
            hits.push(Hit {
                file: file.to_owned(),
                line: i + 1,
            });
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

/// Run rule 11 over the repository this crate lives in. `true` when clean.
pub fn run() -> Result<bool> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let hits = scan_workspace(&root)?;
    if hits.is_empty() {
        println!("entry rule: ok (only gorca-fuse uses `entry_unmasked`)");
        return Ok(true);
    }
    for h in &hits {
        eprintln!(
            "rule 11: {}:{} uses `{BANNED}`; read fused entries through `Ivm::entry_view`",
            h.file, h.line
        );
    }
    eprintln!("{} entry rule violation(s)", hits.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unmasked_accessor_is_flagged_outside_comments_and_tests() {
        let src = "fn a(i: &Ivm) { i.entry_unmasked(n); }\n\
                   // entry_unmasked is test-only\n\
                   fn b(i: &Ivm) { i.entry_view(n); }\n\
                   #[cfg(test)]\n\
                   mod tests { fn t(i: &Ivm) { i.entry_unmasked(n); } }\n";
        let hits = scan("crates/x/src/lib.rs", src);
        assert_eq!(
            hits,
            vec![Hit {
                file: "crates/x/src/lib.rs".to_owned(),
                line: 1
            }]
        );
    }

    #[test]
    fn only_the_engine_and_the_bench_harnesses_are_exempt() {
        assert!(exempt("crates/gorca-fuse/src/ivm.rs"));
        assert!(exempt("crates/gorca-bench/src/main.rs"));
        assert!(!exempt("crates/gorca-core/src/doc_actor/fuse.rs"));
        assert!(!exempt("crates/gorca-fuse-extra/src/lib.rs"));
    }

    /// The real workspace is clean: this is the check `cargo xtask deps` runs.
    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let hits = scan_workspace(&root).unwrap();
        assert!(hits.is_empty(), "{hits:?}");
    }
}
