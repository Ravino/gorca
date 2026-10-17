// SPDX-License-Identifier: GPL-3.0-or-later
//! `b1 run --rundirs` end to end on synthetic run directories: interleaving bookkeeping, median
//! gating, the exit code and the result files. No daemon, no bus.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::Path;
use std::process::Command;

fn write_step(root: &Path, arm: &str, repeat: usize, max_us: f64) {
    let d = root.join(arm).join("step").join(repeat.to_string());
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("step.json"),
        format!("{{\"rounds\": [{{\"max_us\": {max_us}}}]}}"),
    )
    .unwrap();
}

fn run(root: &Path, out: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_b1"))
        .args([
            "run", "--label", "t", "--arm", "good:/x", "--arm", "slow:/y",
        ])
        .args(["--plan", "step=3", "--rundirs"])
        .arg(root)
        .arg("--out")
        .arg(out)
        .output()
        .unwrap()
}

#[test]
fn slow_step_arm_is_not_rankable_and_exit_code_says_so() {
    let base = std::env::temp_dir().join(format!("b1-cli-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, out) = (base.join("runs"), base.join("out"));
    for r in 0..3u8 {
        write_step(&root, "good", usize::from(r), 40.0 + f64::from(r));
        write_step(&root, "slow", usize::from(r), 900.0);
    }
    let o = run(&root, &out);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(
        !o.status.success(),
        "a failing gate must fail the run:\n{text}"
    );
    assert!(text.contains("| good | yes | - |"), "{text}");
    assert!(text.contains("| slow | no | B1-STEP.max_us |"), "{text}");
    let json = std::fs::read_dir(out.join("t"))
        .unwrap()
        .flatten()
        .find(|e| e.path().extension().is_some_and(|x| x == "json"))
        .expect("result json written");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(json.path()).unwrap()).unwrap();
    assert!(v["meta"]["cpus_allowed"].is_string());
    assert!(v["meta"]["governor"].is_string());
    assert!(v["meta"]["loadavg_start"].is_string());
    assert_eq!(v["meta"]["arm_rev.good"], "unknown");
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn a_missing_run_directory_invalidates_the_arm() {
    let base = std::env::temp_dir().join(format!("b1-cli-missing-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let (root, out) = (base.join("runs"), base.join("out"));
    for r in 0..3u8 {
        write_step(&root, "good", usize::from(r), 40.0);
    }
    write_step(&root, "slow", 0, 40.0);
    let o = run(&root, &out);
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    assert!(!o.status.success(), "{text}");
    assert!(text.contains("| slow | no | B1-RUNS.invalid |"), "{text}");
    let _ = std::fs::remove_dir_all(&base);
}
