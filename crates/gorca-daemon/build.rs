// SPDX-License-Identifier: GPL-3.0-or-later
//! Stamps the binary with the git revision it was built from (`GORCA_GIT_REV`), so a test
//! harness can refuse to measure a binary that does not match the tree it claims to measure.

use std::process::Command;

fn git(args: &[&str]) -> String {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn main() {
    let rev = git(&["rev-parse", "--short", "HEAD"]);
    let rev = if rev.is_empty() {
        "unknown".to_owned()
    } else {
        rev
    };
    println!("cargo:rustc-env=GORCA_GIT_REV={rev}");
    // Rebuild the stamp when the checked-out commit moves (the reflog of this worktree).
    let gitdir = git(&["rev-parse", "--git-dir"]);
    if !gitdir.is_empty() {
        println!("cargo:rerun-if-changed={gitdir}/logs/HEAD");
        println!("cargo:rerun-if-changed={gitdir}/HEAD");
    }
    println!("cargo:rerun-if-changed=build.rs");
}
