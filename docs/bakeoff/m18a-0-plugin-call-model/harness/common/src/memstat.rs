// SPDX-License-Identifier: MIT OR Apache-2.0
//! Process memory accounting for M7: `/proc/self/smaps_rollup` (Rss, Pss, Anon) and
//! `/proc/self/status` (VmRSS, VmHWM, VmSize), all in kB.

use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct MemSnap {
    pub rss_kb: u64,
    pub pss_kb: u64,
    pub anon_kb: u64,
    pub vm_rss_kb: u64,
    pub vm_hwm_kb: u64,
    pub vm_size_kb: u64,
    pub threads: u64,
}

fn kv(path: &str) -> BTreeMap<String, u64> {
    let mut m = BTreeMap::new();
    for l in std::fs::read_to_string(path).unwrap_or_default().lines() {
        if let Some((k, v)) = l.split_once(':') {
            let n: u64 = v
                .split_whitespace()
                .next()
                .and_then(|x| x.parse().ok())
                .unwrap_or(0);
            m.insert(k.trim().to_string(), n);
        }
    }
    m
}

pub fn snap() -> MemSnap {
    let sm = kv("/proc/self/smaps_rollup");
    let st = kv("/proc/self/status");
    let g = |m: &BTreeMap<String, u64>, k: &str| m.get(k).copied().unwrap_or(0);
    MemSnap {
        rss_kb: g(&sm, "Rss"),
        pss_kb: g(&sm, "Pss"),
        anon_kb: g(&sm, "Anonymous"),
        vm_rss_kb: g(&st, "VmRSS"),
        vm_hwm_kb: g(&st, "VmHWM"),
        vm_size_kb: g(&st, "VmSize"),
        threads: g(&st, "Threads"),
    }
}

/// Least-squares slope of y over x (kB per plugin for the three-point fit of M7).
pub fn slope(points: &[(f64, f64)]) -> f64 {
    let n = points.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let (sx, sy): (f64, f64) = points
        .iter()
        .fold((0.0, 0.0), |a, p| (a.0 + p.0, a.1 + p.1));
    let (mx, my) = (sx / n, sy / n);
    let num: f64 = points.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let den: f64 = points.iter().map(|p| (p.0 - mx).powi(2)).sum();
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_reads_something() {
        let s = snap();
        assert!(s.vm_rss_kb > 0 && s.threads >= 1);
    }

    #[test]
    fn slope_fit() {
        assert!((slope(&[(1.0, 10.0), (10.0, 100.0), (50.0, 500.0)]) - 10.0).abs() < 1e-9);
    }
}
