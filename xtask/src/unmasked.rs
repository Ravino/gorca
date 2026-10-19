// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 11 (ADR-0024 review, M22a wave C): speech and every other reader get a fused entry only
//! through `Ivm::entry_view`, which applies the silence mask of revoked, paused and quarantined
//! sources at once. `Ivm::entry_unmasked` returns the published entry as the last re-fusion left
//! it, which may still carry a silenced source's value; it exists for the engine's own tests.
//!
//! Scope: non-test sources under `crates/*/src` and `extensions/*/src`, outside `gorca-fuse`
//! (which defines it). Text after the first `#[cfg(test)]` line of a file is test code and is
//! skipped; `tests/`, `benches/` and `examples/` directories are not scanned.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// The call that bypasses the silence mask.
const FORBIDDEN: &str = "entry_unmasked(";

/// Repo-relative path prefixes that may call it.
const EXEMPT: &[&str] = &["crates/gorca-fuse/", "xtask/"];

#[derive(Debug, Eq, PartialEq)]
pub struct Hit {
    pub file: String,
    pub line: usize,
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
        if line.contains(FORBIDDEN) {
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
        println!("masked-entry rule: ok (only gorca-fuse calls entry_unmasked)");
        return Ok(true);
    }
    for h in &hits {
        eprintln!(
            "rule 11: {}:{} calls `entry_unmasked`; read fused entries through `Ivm::entry_view`",
            h.file, h.line
        );
    }
    eprintln!("{} masked-entry rule violation(s)", hits.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_outside_tests_and_comments_is_flagged() {
        let src = "fn a(i: &Ivm) { i.entry_unmasked(n); }\n\
                   // i.entry_unmasked(n)\n\
                   /// docs mention entry_unmasked(n)\n\
                   fn b(i: &Ivm) { i.entry_view(n); }\n";
        let hits = scan("crates/x/src/lib.rs", src);
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].line, 1);
    }

    #[test]
    fn test_modules_are_skipped() {
        let src = "fn a() {}\n#[cfg(test)]\nmod tests { fn t(i: &Ivm) { i.entry_unmasked(n); } }\n";
        assert!(scan("crates/x/src/lib.rs", src).is_empty());
    }

    #[test]
    fn only_the_engine_is_exempt() {
        assert!(exempt("crates/gorca-fuse/src/ivm.rs"));
        assert!(!exempt("crates/gorca-core/src/app_actor/fuse.rs"));
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
