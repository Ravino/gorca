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

/// Reader configuration for the result header: the daemon arguments, the extension directory
/// (`--ext-dir` argument or `GORCA_EXT_DIR` in the reader environment) and the extensions found
/// in it, so a run with an extension loaded can never be mistaken for one without.
pub fn reader_meta(
    reader: &super::apps::ReaderSpec,
    reader_env: &[(String, String)],
) -> Vec<(&'static str, String)> {
    let args: &[String] = match reader {
        super::apps::ReaderSpec::Daemon { args, .. } => args,
        super::apps::ReaderSpec::Stub => &[],
    };
    let mut ext_dir = reader_env
        .iter()
        .find(|(k, _)| k == "GORCA_EXT_DIR")
        .map(|(_, v)| v.clone());
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--ext-dir=") {
            ext_dir = Some(v.to_owned());
        } else if a == "--ext-dir" {
            ext_dir = it.next().cloned();
        }
    }
    let extensions = ext_dir.as_deref().map_or_else(
        || "none".to_owned(),
        |d| {
            let mut names: Vec<String> = std::fs::read_dir(d)
                .into_iter()
                .flatten()
                .flatten()
                .filter(|e| e.path().join("ext.toml").is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            if names.is_empty() {
                "none found".to_owned()
            } else {
                names.join(",")
            }
        },
    );
    vec![
        ("reader_args", args.join(" ")),
        ("ext_dir", ext_dir.unwrap_or_else(|| "none".to_owned())),
        ("extensions", extensions),
    ]
}

/// `YYYYMMDDThhmmssZ`, used in result file names.
pub fn stamp() -> String {
    sh("date", &["-u", "+%Y%m%dT%H%M%SZ"])
}

/// Pattern of the topology reports a daemon reader writes (`%p` is the daemon's pid).
pub const TOPOLOGY_REPORT_VAR: &str = "GORCA_TOPOLOGY_REPORT";

/// Make sure every daemon reader writes its topology report under `dir`, unless the caller set
/// `GORCA_TOPOLOGY_REPORT` itself. Returns the report pattern in force.
pub fn ensure_topology_report(
    reader_env: &mut Vec<(String, String)>,
    dir: &std::path::Path,
) -> String {
    if let Some((_, v)) = reader_env.iter().find(|(k, _)| k == TOPOLOGY_REPORT_VAR) {
        return v.clone();
    }
    let _ = std::fs::create_dir_all(dir);
    let pattern = format!("{}/topology.%p.json", dir.display());
    reader_env.push((TOPOLOGY_REPORT_VAR.to_owned(), pattern.clone()));
    pattern
}

/// The `"arm"` value of a topology report (written by the daemon as `{"arm":"single",...`).
#[must_use]
pub fn report_arm(text: &str) -> Option<&str> {
    let rest = text.split_once("\"arm\":\"")?.1;
    rest.split_once('"').map(|(arm, _)| arm)
}

/// Arms the daemon readers of a run actually resolved to, from their topology reports:
/// `(arm, count)` sorted by name. Reports that cannot be read or parsed count as `unreadable`.
#[must_use]
pub fn topology_arms(pattern: &str) -> Vec<(String, usize)> {
    let path = std::path::Path::new(pattern);
    let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
        return Vec::new();
    };
    let (prefix, suffix) = name.split_once("%p").unwrap_or((name, ""));
    let mut counts: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().into_owned();
        if n.starts_with(prefix) && n.ends_with(suffix) && n.len() > prefix.len() + suffix.len() {
            let arm = std::fs::read_to_string(e.path())
                .ok()
                .and_then(|t| report_arm(&t).map(str::to_owned))
                .unwrap_or_else(|| "unreadable".to_owned());
            *counts.entry(arm).or_default() += 1;
        }
    }
    counts.into_iter().collect()
}

/// Strict topology gate: `Err` when `expected` is set and any reader resolved to another arm,
/// or no report was found at all (the check must never pass vacuously).
pub fn check_topology(arms: &[(String, usize)], expected: Option<&str>) -> Result<(), String> {
    let Some(want) = expected else {
        return Ok(());
    };
    if arms.is_empty() {
        return Err(format!(
            "expected topology {want}, but no topology report was written"
        ));
    }
    match arms.iter().find(|(a, _)| a != want) {
        Some((a, n)) => Err(format!(
            "expected topology {want}, but {n} reader(s) resolved to {a}"
        )),
        None => Ok(()),
    }
}

#[cfg(test)]
mod topology_tests {
    use super::*;

    #[test]
    fn arm_is_read_from_the_report() {
        assert_eq!(
            report_arm("{\"arm\":\"lanes-rt\",\"pid\":1}"),
            Some("lanes-rt")
        );
        assert_eq!(report_arm("{}"), None);
    }

    #[test]
    fn strict_check_fails_on_mismatch_and_on_nothing() {
        let a = |s: &str, n| vec![(s.to_owned(), n)];
        assert!(check_topology(&a("single", 3), None).is_ok());
        assert!(check_topology(&a("lanes-rt", 3), Some("lanes-rt")).is_ok());
        assert!(check_topology(&a("single", 3), Some("lanes-rt")).is_err());
        assert!(check_topology(&[], Some("lanes-rt")).is_err());
        let mixed = vec![("lanes-rt".to_owned(), 2), ("single".to_owned(), 1)];
        assert!(check_topology(&mixed, Some("lanes-rt")).is_err());
    }

    #[test]
    fn reports_are_collected_by_pattern() {
        let dir = std::env::temp_dir().join(format!("gorca-topo-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("topology.11.json"), "{\"arm\":\"single\"}").unwrap();
        std::fs::write(dir.join("topology.12.json"), "{\"arm\":\"single\"}").unwrap();
        std::fs::write(dir.join("other.json"), "{\"arm\":\"lanes\"}").unwrap();
        let pat = format!("{}/topology.%p.json", dir.display());
        assert_eq!(topology_arms(&pat), vec![("single".to_owned(), 2)]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
