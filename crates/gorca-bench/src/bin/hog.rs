// SPDX-License-Identifier: GPL-3.0-or-later
//! `gorca-hog`: the synthetic M-15 load of bake-off B-5. Saturates every core with the kind of
//! work a parallel tree build does (decode, sort, hash a table of node records), at the default
//! scheduling class, so the scheduler has to choose between it and the reader under test.
//!
//! ```text
//! gorca-hog [--threads N] [--secs S]
//! ```
//!
//! `N` defaults to the number of available CPUs, `S` to 60. It exits by itself after `S`
//! seconds and on SIGTERM; the judge script records its PID and stops it by PID only.

use std::time::{Duration, Instant};

/// One round of "build": fill a node table pseudo-randomly, sort it, fold a hash over it.
fn build_round(seed: &mut u64, table: &mut Vec<u64>) -> u64 {
    table.clear();
    for _ in 0..(1 << 18) {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        table.push(*seed);
    }
    table.sort_unstable();
    table.iter().fold(0u64, |h, v| {
        h.rotate_left(5) ^ v.wrapping_mul(0x9E37_79B9_7F4A_7C15)
    })
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<u64>().ok())
    };
    let threads = value("--threads").map_or_else(
        || std::thread::available_parallelism().map_or(1, usize::from),
        |n| usize::try_from(n).unwrap_or(1),
    );
    let secs = value("--secs").unwrap_or(60);
    let end = Instant::now() + Duration::from_secs(secs);
    let handles: Vec<_> = (0..threads)
        .map(|i| {
            std::thread::Builder::new()
                .name(format!("hog-{i}"))
                .spawn(move || {
                    let mut seed = 0x1234_5678_9ABC_DEF1 ^ (i as u64 + 1);
                    let mut table = Vec::with_capacity(1 << 18);
                    let mut sink = 0u64;
                    while Instant::now() < end {
                        sink ^= build_round(&mut seed, &mut table);
                    }
                    sink
                })
                .ok()
        })
        .collect();
    let mut acc = 0u64;
    for h in handles.into_iter().flatten() {
        acc ^= h.join().unwrap_or(0);
    }
    println!("hog done threads={threads} secs={secs} checksum={acc:x}");
}
