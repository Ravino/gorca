//! H-6/H-12: host metadata recorded with every result, and the quiet-host check.
//!
//! A number from a loaded host is informative only. The recipe for authoritative runs (load < 1,
//! `performance` governor, pinned cores) is in `bench/b1/README.md`; this module records what
//! the host actually looked like so a reviewer can tell the difference.

use std::collections::BTreeMap;
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

/// Governor of every CPU, collapsed ("powersave x16").
pub fn governors() -> String {
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    if let Ok(dir) = std::fs::read_dir("/sys/devices/system/cpu") {
        for e in dir.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.len() > 3 && n.starts_with("cpu") && n[3..].bytes().all(|c| c.is_ascii_digit()) {
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

/// `Cpus_allowed_list` of this process (affinity), e.g. `0-15` or `2-5`.
pub fn cpus_allowed() -> String {
    read("/proc/self/status")
        .lines()
        .find_map(|l| l.strip_prefix("Cpus_allowed_list:"))
        .map(|s| s.trim().to_owned())
        .unwrap_or_default()
}

pub fn loadavg() -> String {
    read("/proc/loadavg")
}

/// One-minute load average as a number.
pub fn load1() -> f64 {
    loadavg()
        .split_whitespace()
        .next()
        .and_then(|x| x.parse().ok())
        .unwrap_or(f64::NAN)
}

fn cpu_model() -> String {
    read("/proc/cpuinfo")
        .lines()
        .find_map(|l| l.strip_prefix("model name"))
        .and_then(|l| l.split_once(':'))
        .map(|x| x.1.trim().to_owned())
        .unwrap_or_default()
}

/// `YYYYMMDDThhmmssZ` for file names.
pub fn stamp() -> String {
    sh("date", &["-u", "+%Y%m%dT%H%M%SZ"])
}

/// Is this host quiet enough for an authoritative run (H-12)? Returns the list of reasons it is
/// not; empty means quiet.
pub fn quiet_problems(load1: f64, governors: &str) -> Vec<String> {
    let mut v = Vec::new();
    if load1.is_nan() || load1 >= 1.0 {
        v.push(format!("load average {load1:.2} >= 1"));
    }
    if !governors.is_empty() && !governors.split(", ").all(|g| g.starts_with("performance")) {
        v.push(format!("governor is not `performance` ({governors})"));
    }
    v
}

/// Key/value metadata for result files. `arms` is `(label, git rev)` per arm; `extra` is appended.
pub fn meta(arms: &[(String, String)], extra: &[(&str, String)]) -> Vec<(String, String)> {
    let gov = governors();
    let load = load1();
    let quiet = quiet_problems(load, &gov);
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
        ("cpus_allowed".into(), cpus_allowed()),
        ("governor".into(), gov),
        ("loadavg_start".into(), loadavg()),
        (
            "quiet_host".into(),
            if quiet.is_empty() {
                "yes".to_owned()
            } else {
                format!("no: {}", quiet.join("; "))
            },
        ),
        (
            "harness_git_rev".into(),
            sh("git", &["rev-parse", "--short", "HEAD"]),
        ),
        (
            "harness_git_dirty".into(),
            (!sh("git", &["status", "--porcelain", "--untracked-files=no"]).is_empty()).to_string(),
        ),
        (
            "bus_impl".into(),
            "dbus-daemon (private, tests/harness/run-isolated.sh)".into(),
        ),
        (
            "dbus_daemon".into(),
            sh("dbus-daemon", &["--version"])
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned(),
        ),
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
    for (label, rev) in arms {
        m.push((format!("arm_rev.{label}"), rev.clone()));
    }
    for (k, v) in extra {
        m.push(((*k).to_owned(), v.clone()));
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quiet_host_rules() {
        assert!(quiet_problems(0.4, "performance x16").is_empty());
        assert_eq!(quiet_problems(11.0, "performance x16").len(), 1);
        assert_eq!(quiet_problems(0.2, "powersave x16").len(), 1);
        assert_eq!(quiet_problems(f64::NAN, "").len(), 1);
    }
}
