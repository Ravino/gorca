// SPDX-License-Identifier: GPL-3.0-or-later
//! `/proc` readers: per-thread CPU time, memory. Safe std only.

use std::collections::BTreeMap;
use std::fs;

/// Name prefix of every harness thread. CPU of these is excluded from candidate accounting.
pub const HARNESS_THREAD_PREFIX: &str = "hx-";

#[derive(Clone, Debug, Default)]
pub struct CpuSnapshot {
    /// `tid -> (comm, on-cpu nanoseconds)` from `/proc/self/task/<tid>/schedstat`.
    pub threads: BTreeMap<u32, (String, u64)>,
}

impl CpuSnapshot {
    pub fn take() -> Self {
        let mut threads = BTreeMap::new();
        let Ok(dir) = fs::read_dir("/proc/self/task") else {
            return Self { threads };
        };
        for e in dir.flatten() {
            let Some(tid) = e.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
                continue;
            };
            let base = e.path();
            let comm = fs::read_to_string(base.join("comm"))
                .map(|s| s.trim().to_owned())
                .unwrap_or_default();
            let ns = fs::read_to_string(base.join("schedstat"))
                .ok()
                .and_then(|s| {
                    s.split_whitespace()
                        .next()
                        .and_then(|x| x.parse::<u64>().ok())
                })
                .unwrap_or(0);
            threads.insert(tid, (comm, ns));
        }
        Self { threads }
    }

    /// CPU nanoseconds used between two snapshots by threads that are not harness threads and
    /// not the main thread. Threads born after `self` count in full.
    pub fn candidate_ns_since(&self, earlier: &CpuSnapshot) -> u64 {
        self.sum_since(earlier, |name| {
            !name.starts_with(HARNESS_THREAD_PREFIX) && name != "gorca-bench-b2" && name != "b2"
        })
    }

    /// Same, restricted to threads whose name starts with `prefix`.
    pub fn ns_since_by_prefix(&self, earlier: &CpuSnapshot, prefix: &str) -> u64 {
        self.sum_since(earlier, |name| name.starts_with(prefix))
    }

    /// Per-thread CPU ns between two snapshots, largest first, harness threads excluded.
    pub fn by_thread_since(&self, earlier: &CpuSnapshot) -> Vec<(String, u64)> {
        let mut v: Vec<(String, u64)> = self
            .threads
            .iter()
            .filter(|(_, (name, _))| !name.starts_with(HARNESS_THREAD_PREFIX) && name != "b2")
            .map(|(tid, (name, ns))| {
                (
                    name.clone(),
                    ns.saturating_sub(earlier.threads.get(tid).map_or(0, |t| t.1)),
                )
            })
            .filter(|(_, ns)| *ns > 0)
            .collect();
        v.sort_by_key(|x| std::cmp::Reverse(x.1));
        v
    }

    fn sum_since(&self, earlier: &CpuSnapshot, keep: impl Fn(&str) -> bool) -> u64 {
        self.threads
            .iter()
            .filter(|(_, (name, _))| keep(name))
            .map(|(tid, (_, ns))| ns.saturating_sub(earlier.threads.get(tid).map_or(0, |t| t.1)))
            .sum()
    }
}

fn status_kib(key: &str) -> u64 {
    fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| l.strip_prefix(key)).and_then(|r| {
                r.split_whitespace()
                    .next()
                    .and_then(|x| x.parse::<u64>().ok())
            })
        })
        .unwrap_or(0)
}

/// Current resident set size in bytes.
pub fn rss_bytes() -> u64 {
    status_kib("VmRSS:") * 1024
}

/// Peak resident set size in bytes (`VmHWM`).
pub fn rss_peak_bytes() -> u64 {
    status_kib("VmHWM:") * 1024
}

/// Reset the kernel's peak RSS counter (`echo 5 > /proc/self/clear_refs`). Best effort.
pub fn reset_rss_peak() {
    let _ = fs::write("/proc/self/clear_refs", "5");
}
