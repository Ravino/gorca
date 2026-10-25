// SPDX-License-Identifier: GPL-3.0-or-later
//! Hybrid-CPU topology and soft thread placement (M18a-5).
//!
//! On a hybrid CPU (Intel Core Ultra: P, E and LP-E cores) a plugin worker that wakes on an idle
//! E or LP-E core runs a guest call 1.3 to 3 times slower than on a P core, and a call after the
//! core idled costs 4 to 6 times a warm one (`docs/reports/m18a-4-plugin-costs.md`). This module
//! finds the classes at run time and gives the interactive plugin path a *mask* of the P cores
//! (never a single-core pin, so the kernel still balances the threads inside it).
//!
//! Detection follows the ADR-0027 style: resolve once, from the real machine, with a portable
//! fallback and one informational log line (a fact, not an error):
//!
//! 1. `/sys/devices/cpu_core/cpus` and `cpu_atom/cpus` (Intel hybrid PMUs); an atom core with no L3
//!    while the P cores have one is a low-power (LP-E) core;
//! 2. else `cpu_capacity` of each CPU;
//! 3. else `cpufreq/cpuinfo_max_freq`;
//! 4. else (or when all CPUs look alike) no hybrid split: [`Placement`] is `None` and nothing is
//!    pinned.
//!
//! The P cores are further reduced to the biggest group sharing one L3 (the cluster).

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use crate::sched;

/// Where the classes came from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// `cpu_core` / `cpu_atom` PMU lists.
    PmuLists,
    /// `cpu_capacity`.
    Capacity,
    /// `cpuinfo_max_freq`.
    MaxFreq,
    /// Every CPU looks alike.
    Uniform,
    /// Nothing readable.
    Unknown,
}

impl Source {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Source::PmuLists => "cpu_core/cpu_atom",
            Source::Capacity => "cpu_capacity",
            Source::MaxFreq => "cpuinfo_max_freq",
            Source::Uniform => "uniform",
            Source::Unknown => "unreadable",
        }
    }
}

/// CPUs of each class (only CPUs the process may use).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Topology {
    /// Performance cores of the biggest L3 cluster.
    pub perf: Vec<usize>,
    /// Efficiency cores.
    pub eff: Vec<usize>,
    /// Low-power cores (no L3, or far below the others).
    pub low: Vec<usize>,
    pub source: Source,
}

/// Where the interactive plugin path runs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placement {
    /// Mask for the writer's wait and the plugin workers it may wait for: the P cores.
    pub fast: Vec<usize>,
    /// P and E cores together, never the low-power ones: the widest mask the wait path may use.
    pub wide: Vec<usize>,
    /// Mask for refinement and third-party workers: the E cores. `None` when there are none (they
    /// then stay unpinned; they are never put on the low-power cores by this module either).
    pub bulk: Option<Vec<usize>>,
}

impl Topology {
    /// The placement for this topology, `None` unless the CPU is hybrid.
    #[must_use]
    pub fn placement(&self) -> Option<Placement> {
        if self.perf.is_empty() || (self.eff.is_empty() && self.low.is_empty()) {
            return None;
        }
        let mut wide: Vec<usize> = self.perf.iter().chain(&self.eff).copied().collect();
        wide.sort_unstable();
        Some(Placement {
            fast: self.perf.clone(),
            wide,
            bulk: (!self.eff.is_empty()).then(|| self.eff.clone()),
        })
    }

    fn flat(source: Source) -> Self {
        Self {
            perf: Vec::new(),
            eff: Vec::new(),
            low: Vec::new(),
            source,
        }
    }
}

/// Parse a kernel cpu list (`0-5,8,10-11`). Malformed items are skipped.
#[must_use]
pub fn parse_cpulist(text: &str) -> Vec<usize> {
    let mut out = Vec::new();
    for item in text.trim().split(',') {
        let item = item.trim();
        if item.is_empty() {
            continue;
        }
        match item.split_once('-') {
            Some((a, b)) => {
                if let (Ok(a), Ok(b)) = (a.trim().parse::<usize>(), b.trim().parse::<usize>())
                    && a <= b
                    && b < 4096
                {
                    out.extend(a..=b);
                }
            }
            None => {
                if let Ok(c) = item.parse::<usize>() {
                    out.push(c);
                }
            }
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn read_num(path: &Path) -> Option<u64> {
    read(path)?.trim().parse().ok()
}

type Classes = (Vec<usize>, Vec<usize>, Vec<usize>, Source);

/// Detect from `devices` (normally `/sys/devices`), keeping only CPUs in `allowed`.
#[must_use]
pub fn detect_at(devices: &Path, allowed: &[usize]) -> Topology {
    let cpu = |c: usize| devices.join(format!("system/cpu/cpu{c}"));
    let l3 = |c: usize| -> Option<String> {
        let t = read(&cpu(c).join("cache/index3/shared_cpu_list"))?;
        let t = t.trim();
        (!t.is_empty()).then(|| t.to_owned())
    };
    let keep =
        |v: Vec<usize>| -> Vec<usize> { v.into_iter().filter(|c| allowed.contains(c)).collect() };

    let core = read(&devices.join("cpu_core/cpus")).map(|t| keep(parse_cpulist(&t)));
    let atom = read(&devices.join("cpu_atom/cpus")).map(|t| keep(parse_cpulist(&t)));
    let (perf, eff, low, source): Classes = match (core, atom) {
        (Some(core), Some(atom)) if !core.is_empty() && !atom.is_empty() => {
            let p_has_l3 = core.iter().any(|&c| l3(c).is_some());
            let (low, eff): (Vec<usize>, Vec<usize>) =
                atom.into_iter().partition(|&c| p_has_l3 && l3(c).is_none());
            (core, eff, low, Source::PmuLists)
        }
        _ => {
            let figure = |file: &str| -> Vec<(usize, u64)> {
                allowed
                    .iter()
                    .filter_map(|&c| Some((c, read_num(&cpu(c).join(file))?)))
                    .collect()
            };
            let cap = figure("cpu_capacity");
            let freq = figure("cpufreq/cpuinfo_max_freq");
            if !allowed.is_empty() && cap.len() == allowed.len() {
                classify(&cap, 40, Source::Capacity)
            } else if !allowed.is_empty() && freq.len() == allowed.len() {
                classify(&freq, 50, Source::MaxFreq)
            } else {
                (Vec::new(), Vec::new(), Vec::new(), Source::Unknown)
            }
        }
    };
    if perf.is_empty() {
        return Topology::flat(if source == Source::Unknown {
            Source::Unknown
        } else {
            Source::Uniform
        });
    }
    // The cluster: the biggest group of P cores sharing one L3 (all of them when unreadable).
    let mut groups: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for &c in &perf {
        let key = l3(c);
        match groups.iter_mut().find(|(k, _)| *k == key) {
            Some((_, v)) => v.push(c),
            None => groups.push((key, vec![c])),
        }
    }
    let mut perf = groups
        .into_iter()
        .map(|(_, v)| v)
        .max_by_key(Vec::len)
        .unwrap_or_default();
    perf.sort_unstable();
    Topology {
        perf,
        eff,
        low,
        source,
    }
}

/// Split by a per-CPU figure (capacity or frequency): within 10 % of the top is P, below
/// `low_pct` percent of the top is low-power, the rest efficiency. All alike gives no P.
fn classify(vals: &[(usize, u64)], low_pct: u64, source: Source) -> Classes {
    let max = vals.iter().map(|(_, v)| *v).max().unwrap_or(0);
    let min = vals.iter().map(|(_, v)| *v).min().unwrap_or(0);
    if max == 0 || min * 10 >= max * 9 {
        return (Vec::new(), Vec::new(), Vec::new(), Source::Uniform);
    }
    let mut p = Vec::new();
    let mut e = Vec::new();
    let mut l = Vec::new();
    for &(c, v) in vals {
        if v * 10 >= max * 9 {
            p.push(c);
        } else if v * 100 < max * low_pct {
            l.push(c);
        } else {
            e.push(c);
        }
    }
    (p, e, l, source)
}

/// Detect on this machine.
#[must_use]
pub fn detect() -> Topology {
    detect_at(Path::new("/sys/devices"), &sched::allowed_cpus())
}

fn list(v: &[usize]) -> String {
    let mut s = String::new();
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        if !s.is_empty() {
            s.push(',');
        }
        let _ = if j > i {
            write!(s, "{}-{}", v[i], v[j])
        } else {
            write!(s, "{}", v[i])
        };
        i = j + 1;
    }
    s
}

/// The process-wide placement, detected once; `None` (and one info line) when the CPU is not
/// hybrid or cannot be read.
#[must_use]
pub fn auto() -> Option<Arc<Placement>> {
    static ONCE: OnceLock<Option<Arc<Placement>>> = OnceLock::new();
    ONCE.get_or_init(|| {
        let t = detect();
        let p = t.placement();
        if let Some(p) = &p {
            tracing::info!(
                source = t.source.label(),
                interactive = %list(&p.fast),
                background = %p.bulk.as_deref().map_or_else(|| "unpinned".to_owned(), list),
                low_power = %list(&t.low),
                "cpu placement: hybrid CPU, interactive plugin path on the performance cores"
            );
        } else {
            tracing::info!(
                source = t.source.label(),
                "cpu placement: no hybrid CPU topology found, plugin threads are not pinned"
            );
        }
        p.map(Arc::new)
    })
    .clone()
}

/// Restrict the calling thread to `cpus` (intersected with what it may use). Returns false when
/// nothing changed (empty intersection, already that wide, or the kernel refused).
pub fn pin_current(cpus: &[usize]) -> bool {
    let allowed = sched::allowed_cpus();
    let set: Vec<usize> = cpus
        .iter()
        .copied()
        .filter(|c| allowed.contains(c))
        .collect();
    if set.is_empty() || set.len() == allowed.len() {
        return false;
    }
    sched::set_affinity(sched::current_tid(), &set).is_ok()
}

static DISPLACED: AtomicU64 = AtomicU64::new(0);
static ON_FAST: AtomicU64 = AtomicU64::new(0);

/// How often a writer turn found its thread off the fast cores and moved it / already on them.
#[must_use]
pub fn turn_counts() -> (u64, u64) {
    (
        DISPLACED.load(Ordering::Relaxed),
        ON_FAST.load(Ordering::Relaxed),
    )
}

/// Keeps a writer thread on the fast cores for the length of one waited turn. If the thread is
/// already on one, nothing is done (no system call). Otherwise its mask is narrowed to the fast
/// cores until [`TurnPin::leave`], which puts the old mask back (also from another thread, by tid).
#[derive(Debug, Default)]
pub struct TurnPin {
    saved: Option<(u32, Vec<usize>)>,
}

impl TurnPin {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Narrow the calling thread to `allow` unless it already runs on one of those CPUs.
    pub fn enter(&mut self, allow: &[usize]) {
        if self.saved.is_some() {
            return;
        }
        if sched::current_cpu().is_some_and(|c| allow.contains(&c)) {
            ON_FAST.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let prev = sched::allowed_cpus();
        let mask: Vec<usize> = allow.iter().copied().filter(|c| prev.contains(c)).collect();
        if mask.is_empty() || prev.is_empty() {
            return;
        }
        let tid = sched::current_tid();
        if sched::set_affinity(tid, &mask).is_ok() {
            DISPLACED.fetch_add(1, Ordering::Relaxed);
            self.saved = Some((tid, prev));
        }
    }

    /// Put the thread's old mask back.
    pub fn leave(&mut self) {
        if let Some((tid, prev)) = self.saved.take()
            && let Err(e) = sched::set_affinity(tid, &prev)
        {
            tracing::debug!(error = %e, "could not restore the writer's cpu mask");
        }
    }

    /// The mask is currently narrowed.
    #[must_use]
    pub fn active(&self) -> bool {
        self.saved.is_some()
    }
}

impl Drop for TurnPin {
    fn drop(&mut self) {
        self.leave();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    struct Tree(PathBuf);
    impl Tree {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("gorca-topo-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Tree(p)
        }
        fn put(&self, rel: &str, text: &str) {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, text).unwrap();
        }
    }
    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn all(n: usize) -> Vec<usize> {
        (0..n).collect()
    }

    #[test]
    fn cpulist_parses() {
        assert_eq!(parse_cpulist("0-2,5,7-8\n"), [0, 1, 2, 5, 7, 8]);
        assert_eq!(parse_cpulist(""), Vec::<usize>::new());
        assert_eq!(parse_cpulist("3-1,x,2"), [2]);
        assert_eq!(list(&[0, 1, 2, 5, 7, 8]), "0-2,5,7-8");
    }

    #[test]
    fn pmu_lists_with_lp_e_without_l3() {
        let t = Tree::new("pmu");
        t.put("cpu_core/cpus", "0-5\n");
        t.put("cpu_atom/cpus", "6-15\n");
        for c in 0..14 {
            t.put(
                &format!("system/cpu/cpu{c}/cache/index3/shared_cpu_list"),
                "0-13\n",
            );
        }
        let top = detect_at(&t.0, &all(16));
        assert_eq!(top.perf, [0, 1, 2, 3, 4, 5]);
        assert_eq!(top.eff, (6..14).collect::<Vec<_>>());
        assert_eq!(top.low, [14, 15]);
        assert_eq!(top.source, Source::PmuLists);
        let p = top.placement().unwrap();
        assert_eq!(p.fast, top.perf);
        assert_eq!(p.bulk.as_deref(), Some(&top.eff[..]));
    }

    #[test]
    fn allowed_mask_is_respected() {
        let t = Tree::new("allowed");
        t.put("cpu_core/cpus", "0-5\n");
        t.put("cpu_atom/cpus", "6-13\n");
        let top = detect_at(&t.0, &[2, 3, 7]);
        assert_eq!(top.perf, [2, 3]);
        assert_eq!(top.eff, [7]);
        // No P core allowed: not hybrid for us.
        let top = detect_at(&t.0, &[7, 8]);
        assert!(top.placement().is_none());
    }

    #[test]
    fn capacity_fallback() {
        let t = Tree::new("cap");
        for (c, v) in [(0, 1024), (1, 1024), (2, 741), (3, 741), (4, 294)] {
            t.put(
                &format!("system/cpu/cpu{c}/cpu_capacity"),
                &format!("{v}\n"),
            );
        }
        let top = detect_at(&t.0, &all(5));
        assert_eq!(top.perf, [0, 1]);
        assert_eq!(top.eff, [2, 3]);
        assert_eq!(top.low, [4]);
        assert_eq!(top.source, Source::Capacity);
    }

    #[test]
    fn max_freq_fallback_and_uniform() {
        let t = Tree::new("freq");
        for (c, v) in [
            (0, 5_400_000),
            (1, 5_400_000),
            (2, 4_500_000),
            (3, 2_500_000),
        ] {
            t.put(
                &format!("system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"),
                &format!("{v}\n"),
            );
        }
        let top = detect_at(&t.0, &all(4));
        assert_eq!(top.perf, [0, 1]);
        assert_eq!(top.eff, [2]);
        assert_eq!(top.low, [3]);
        assert_eq!(top.source, Source::MaxFreq);

        let u = Tree::new("uniform");
        for c in 0..4 {
            u.put(
                &format!("system/cpu/cpu{c}/cpufreq/cpuinfo_max_freq"),
                "3000000\n",
            );
        }
        let top = detect_at(&u.0, &all(4));
        assert_eq!(top.source, Source::Uniform);
        assert!(top.placement().is_none());
    }

    #[test]
    fn nothing_readable_is_a_noop() {
        let t = Tree::new("none");
        let top = detect_at(&t.0, &all(4));
        assert_eq!(top.source, Source::Unknown);
        assert!(top.placement().is_none());
    }

    #[test]
    fn p_cluster_is_the_biggest_l3_group() {
        let t = Tree::new("cluster");
        t.put("cpu_core/cpus", "0-3\n");
        t.put("cpu_atom/cpus", "4-5\n");
        for c in 0..2 {
            t.put(
                &format!("system/cpu/cpu{c}/cache/index3/shared_cpu_list"),
                "0-1\n",
            );
        }
        for c in 2..6 {
            t.put(
                &format!("system/cpu/cpu{c}/cache/index3/shared_cpu_list"),
                "2-5\n",
            );
        }
        let top = detect_at(&t.0, &all(6));
        assert_eq!(top.perf, [2, 3]);
    }

    #[test]
    fn turn_pin_round_trip_restores_the_mask() {
        // Narrow only this test's own thread; the test thread is its own kernel thread.
        let before = sched::allowed_cpus();
        if before.len() < 2 {
            return;
        }
        let here = sched::current_cpu().unwrap_or(before[0]);
        let other: Vec<usize> = before
            .iter()
            .copied()
            .filter(|&c| c != here)
            .take(1)
            .collect();
        let mut pin = TurnPin::new();
        pin.enter(&other);
        assert!(pin.active());
        assert_eq!(sched::allowed_cpus(), other);
        pin.leave();
        assert!(!pin.active());
        assert_eq!(sched::allowed_cpus(), before);
        // Already on a fast CPU: no change.
        let mut pin = TurnPin::new();
        pin.enter(&before);
        assert!(!pin.active());
    }
}
