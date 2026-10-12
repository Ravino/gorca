//! Host metadata recorded with every result (the host is loaded, so numbers need context).

use std::process::Command;

fn read(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

fn sh(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

/// Governor of every CPU, collapsed ("performance x16").
fn governors() -> String {
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
        for e in dir.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.starts_with("cpu") && n[3..].chars().all(|c| c.is_ascii_digit()) && n.len() > 3 {
                let g = read(&format!(
                    "/sys/devices/system/cpu/{n}/cpufreq/scaling_governor"
                ));
                *counts
                    .entry(if g.is_empty() { "n/a".to_owned() } else { g })
                    .or_default() += 1;
            }
        }
    }
    counts
        .iter()
        .map(|(g, n)| format!("{g} x{n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn cpu_model() -> String {
    read("/proc/cpuinfo")
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|x| x.1.trim().to_owned())
        .unwrap_or_default()
}

pub fn loadavg() -> String {
    read("/proc/loadavg")
}

/// Key/value metadata for the result files. `extra` is appended (reader, label, ...).
pub fn meta(extra: &[(&str, String)]) -> Vec<(String, String)> {
    let mut m: Vec<(String, String)> = vec![
        ("utc".into(), sh("date", &["-u", "+%Y-%m-%dT%H:%M:%SZ"])),
        ("host".into(), sh("uname", &["-n"])),
        ("kernel".into(), sh("uname", &["-r"])),
        ("cpu_model".into(), cpu_model()),
        (
            "cpus".into(),
            std::thread::available_parallelism()
                .map_or(1, usize::from)
                .to_string(),
        ),
        ("governor".into(), governors()),
        ("loadavg_start".into(), loadavg()),
        (
            "git_rev".into(),
            sh("git", &["rev-parse", "--short", "HEAD"]),
        ),
        (
            "git_dirty".into(),
            (!sh("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty()).to_string(),
        ),
        (
            "bus_impl".into(),
            "dbus-daemon (private, tests/harness/run-isolated.sh)".into(),
        ),
        ("python".into(), sh("/usr/bin/python3", &["--version"])),
        (
            "build".into(),
            if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }
            .into(),
        ),
    ];
    for (k, v) in extra {
        m.push(((*k).to_owned(), v.clone()));
    }
    m
}

/// `YYYYMMDDThhmmssZ`, used in result file names.
pub fn stamp() -> String {
    sh("date", &["-u", "+%Y%m%dT%H%M%SZ"])
}
