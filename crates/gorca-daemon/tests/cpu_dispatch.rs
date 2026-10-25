// SPDX-License-Identifier: GPL-3.0-or-later
//! ADR-0027 section 6.1: `gorca_cpu::init` is the first statement of `main`, and both the
//! `--cpu-disable` flag and `GORCA_CPU_DISABLE` reach it. The binary's `--version --verbose`
//! prints the resolved report, so the ordering and both paths are asserted, not assumed.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::Command;

fn report(args: &[&str], env: Option<&str>) -> String {
    let mut c = Command::new(env!("CARGO_BIN_EXE_gorca"));
    c.args(args).env_remove("GORCA_CPU_DISABLE");
    if let Some(v) = env {
        c.env("GORCA_CPU_DISABLE", v);
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout).unwrap()
}

fn kernel_lines(text: &str) -> Vec<&str> {
    text.lines().filter(|l| l.starts_with("kernel ")).collect()
}

#[test]
fn init_runs_first_and_resolves_the_table() {
    let t = report(&["--version", "--verbose"], None);
    assert!(t.starts_with("gorca "), "{t}");
    assert!(t.contains("table resolved by: init"), "{t}");
    assert!(t.contains("disabled by list: none"), "{t}");
    assert!(!kernel_lines(&t).is_empty(), "{t}");
}

#[test]
fn flag_and_env_both_reach_init() {
    for (args, env) in [
        (
            vec!["--version", "--verbose", "--cpu-disable", "avx2"],
            None,
        ),
        (vec!["--version", "--verbose", "--cpu-disable=avx2"], None),
        (vec!["--version", "--verbose"], Some("avx2")),
    ] {
        let t = report(&args, env);
        assert!(
            t.contains("disabled by list: avx2"),
            "{args:?} {env:?}\n{t}"
        );
        assert!(t.contains("table resolved by: init"), "{t}");
        for l in kernel_lines(&t) {
            assert!(!l.contains("avx2"), "avx2 impl still active: {l}\n{t}");
        }
    }
}

#[test]
fn flag_wins_over_env_and_all_is_portable() {
    let t = report(
        &["--version", "--verbose", "--cpu-disable=all"],
        Some("avx2"),
    );
    assert!(t.contains("disabled by list: all"), "{t}");
    for l in kernel_lines(&t) {
        assert!(l.ends_with(": portable"), "{l}\n{t}");
    }
}
