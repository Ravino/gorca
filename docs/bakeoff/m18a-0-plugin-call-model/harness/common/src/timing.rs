// SPDX-License-Identifier: MIT OR Apache-2.0
//! Clock, pinning, timer slack, per-thread counters, raw sample files and percentiles.
//!
//! One clock for every stamp: `CLOCK_MONOTONIC` in nanoseconds (SPEC 7, requirement 2).

use hdrhistogram::Histogram;
use std::io::Write;
use std::path::Path;

/// `CLOCK_MONOTONIC` in nanoseconds. About 20 ns per read (reported by [`clock_overhead_ns`]).
#[inline]
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out pointer; CLOCK_MONOTONIC always exists on Linux.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000_000 + ts.tv_nsec as u64
}

/// Mean cost of one [`now_ns`] read, measured over `n` back-to-back reads.
pub fn clock_overhead_ns(n: u32) -> f64 {
    let a = now_ns();
    let mut x = 0u64;
    for _ in 0..n {
        x = x.wrapping_add(now_ns());
    }
    std::hint::black_box(x);
    (now_ns() - a) as f64 / n as f64
}

/// `prctl(PR_SET_TIMERSLACK, ns)`. 0 means "reset to default" (50 us), so callers pass 1 for 1 ns.
pub fn set_timerslack(ns: u64) -> std::io::Result<()> {
    // SAFETY: plain prctl on the calling thread.
    let r = unsafe { libc::prctl(libc::PR_SET_TIMERSLACK, ns as libc::c_ulong, 0, 0, 0) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Kernel thread id of the caller.
pub fn gettid() -> i64 {
    // SAFETY: gettid has no arguments and cannot fail.
    unsafe { libc::syscall(libc::SYS_gettid) }
}

/// Timer slack of the calling thread in ns. `PR_GET_TIMERSLACK` is used because this kernel
/// exposes `timerslack_ns` only for the process leader (`/proc/self/timerslack_ns`), not per task.
pub fn get_timerslack() -> Option<u64> {
    // SAFETY: PR_GET_TIMERSLACK takes no pointer arguments and returns the value.
    let r = unsafe { libc::prctl(libc::PR_GET_TIMERSLACK, 0, 0, 0, 0) };
    if r < 0 {
        None
    } else {
        Some(r as u64)
    }
}

/// (voluntary, involuntary) context switches of the calling thread so far.
pub fn thread_ctx_switches() -> (u64, u64) {
    let s =
        std::fs::read_to_string(format!("/proc/self/task/{}/status", gettid())).unwrap_or_default();
    let mut v = (0, 0);
    for l in s.lines() {
        if let Some(r) = l.strip_prefix("voluntary_ctxt_switches:") {
            v.0 = r.trim().parse().unwrap_or(0);
        } else if let Some(r) = l.strip_prefix("nonvoluntary_ctxt_switches:") {
            v.1 = r.trim().parse().unwrap_or(0);
        }
    }
    v
}

/// (minor, major) page faults of the calling thread so far (`getrusage(RUSAGE_THREAD)`).
#[inline]
pub fn thread_faults() -> (u64, u64) {
    // SAFETY: zeroed rusage is a valid out value for getrusage.
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: valid pointer, RUSAGE_THREAD is supported on Linux.
    unsafe { libc::getrusage(libc::RUSAGE_THREAD, &mut ru) };
    (ru.ru_minflt as u64, ru.ru_majflt as u64)
}

/// Pin the calling thread to one CPU. Used only by the labelled diagnostic variants.
pub fn pin_to_cpu(cpu: usize) -> std::io::Result<()> {
    // SAFETY: cpu_set_t is plain data; sched_setaffinity(0, ..) targets the calling thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
}

/// Spin until `deadline_ns`, sleeping (clock_nanosleep, absolute) while more than `spin_ns` away.
pub fn wait_until(deadline_ns: u64, spin_ns: u64) {
    loop {
        let now = now_ns();
        if now >= deadline_ns {
            return;
        }
        let left = deadline_ns - now;
        if left > spin_ns + 20_000 {
            let target = deadline_ns - spin_ns;
            let ts = libc::timespec {
                tv_sec: (target / 1_000_000_000) as libc::time_t,
                tv_nsec: (target % 1_000_000_000) as libc::c_long,
            };
            // SAFETY: valid timespec; absolute CLOCK_MONOTONIC sleep, remainder pointer unused.
            unsafe {
                libc::clock_nanosleep(
                    libc::CLOCK_MONOTONIC,
                    libc::TIMER_ABSTIME,
                    &ts,
                    std::ptr::null_mut(),
                )
            };
        } else {
            std::hint::spin_loop();
        }
    }
}

/// Percentiles of one series, in the unit of the samples (ns).
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Pcts {
    pub n: u64,
    pub min: u64,
    pub p50: u64,
    pub p90: u64,
    pub p99: u64,
    pub p999: u64,
    pub max: u64,
    pub mean: f64,
    pub stddev: f64,
}

/// Percentiles from the **raw** samples (sorted copy), nearest-rank. Not from a histogram.
pub fn percentiles(samples: &[u32]) -> Pcts {
    if samples.is_empty() {
        return Pcts::default();
    }
    let mut v: Vec<u32> = samples.to_vec();
    v.sort_unstable();
    let n = v.len();
    let at = |q: f64| -> u64 {
        let rank = ((q * n as f64).ceil() as usize).clamp(1, n);
        v[rank - 1] as u64
    };
    let mean = v.iter().map(|x| *x as f64).sum::<f64>() / n as f64;
    let var = v.iter().map(|x| (*x as f64 - mean).powi(2)).sum::<f64>() / n as f64;
    Pcts {
        n: n as u64,
        min: v[0] as u64,
        p50: at(0.50),
        p90: at(0.90),
        p99: at(0.99),
        p999: at(0.999),
        max: v[n - 1] as u64,
        mean,
        stddev: var.sqrt(),
    }
}

/// HDR histogram (3 significant digits, 1 ns resolution) stored beside the raw data.
pub fn hdr(samples: &[u32]) -> Histogram<u64> {
    let mut h = Histogram::<u64>::new_with_bounds(1, 60_000_000_000, 3)
        .unwrap_or_else(|_| Histogram::new(3).expect("hdr"));
    for s in samples {
        let _ = h.record((*s as u64).max(1));
    }
    h
}

/// Write the raw samples as little-endian u32 ns, zstd-compressed.
pub fn write_u32_zst(path: &Path, samples: &[u32]) -> std::io::Result<()> {
    let mut raw = Vec::with_capacity(samples.len() * 4);
    for s in samples {
        raw.extend_from_slice(&s.to_le_bytes());
    }
    let f = std::fs::File::create(path)?;
    let mut enc = zstd::Encoder::new(f, 3)?;
    enc.write_all(&raw)?;
    enc.finish()?;
    Ok(())
}

/// Read a file written by [`write_u32_zst`].
pub fn read_u32_zst(path: &Path) -> std::io::Result<Vec<u32>> {
    let f = std::fs::File::open(path)?;
    let raw = zstd::decode_all(f)?;
    Ok(raw
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Saturating ns -> u32 (a 4.29 s stall is already a failed run).
#[inline]
pub fn sat32(ns: u64) -> u32 {
    ns.min(u32::MAX as u64) as u32
}

/// `/proc/<what>` helpers for host facts.
pub fn read_trim(path: &str) -> String {
    std::fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// PSI "some avg10" of a resource (`cpu`, `memory`), 0.0 when unavailable.
pub fn psi_some_avg10(res: &str) -> f64 {
    let s = read_trim(&format!("/proc/pressure/{res}"));
    for l in s.lines() {
        if l.starts_with("some") {
            for part in l.split_whitespace() {
                if let Some(v) = part.strip_prefix("avg10=") {
                    return v.parse().unwrap_or(0.0);
                }
            }
        }
    }
    0.0
}

/// 1-minute load average.
pub fn loadavg1() -> f64 {
    read_trim("/proc/loadavg")
        .split_whitespace()
        .next()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0)
}

/// Sorted unique governors of all CPUs.
pub fn governors() -> String {
    let mut g: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir("/sys/devices/system/cpu") {
        for e in rd.flatten() {
            let p = e.path().join("cpufreq/scaling_governor");
            if let Ok(s) = std::fs::read_to_string(p) {
                g.push(s.trim().to_string());
            }
        }
    }
    g.sort();
    g.dedup();
    g.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_nearest_rank() {
        let v: Vec<u32> = (1..=100).collect();
        let p = percentiles(&v);
        assert_eq!((p.p50, p.p90, p.p99, p.max), (50, 90, 99, 100));
        assert_eq!(percentiles(&[]).n, 0);
    }

    #[test]
    fn zst_roundtrip() {
        let dir = std::env::temp_dir().join(format!("m18a0-zst-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p = dir.join("x.u32.zst");
        let v: Vec<u32> = (0..1000).map(|i| i * 7).collect();
        write_u32_zst(&p, &v).expect("write");
        assert_eq!(read_u32_zst(&p).expect("read"), v);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn timerslack_one_ns_sticks() {
        std::thread::spawn(|| {
            set_timerslack(1).expect("prctl");
            assert_eq!(get_timerslack(), Some(1));
        })
        .join()
        .expect("thread");
    }

    #[test]
    fn clock_is_cheap_and_monotonic() {
        let a = now_ns();
        let b = now_ns();
        assert!(b >= a);
        assert!(clock_overhead_ns(10_000) < 400.0);
    }
}
