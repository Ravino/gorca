// SPDX-License-Identifier: MIT OR Apache-2.0
//! `loaded` condition generator (SPEC 4.1): `m18a0-load [--spin 8] [--memwalk 2] [--mib 64] [--seconds 3600]`.
//! 8 busy threads at nice 0 plus 2 threads walking a 64 MiB buffer; total memory stays far below
//! 2 GiB and the process refuses to start when it would not. Kill it by its own PID only.
#![allow(
    clippy::needless_range_loop,
    clippy::unusual_byte_groupings,
    clippy::should_implement_trait,
    clippy::if_same_then_else,
    clippy::chunks_exact_to_as_chunks,
    clippy::manual_is_multiple_of,
    clippy::disallowed_methods,
    clippy::too_many_arguments,
    clippy::type_complexity
)] // throwaway prototype harness: style lints only

fn arg(name: &str, d: u64) -> u64 {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

fn main() {
    let spin = arg("--spin", 8);
    let walkers = arg("--memwalk", 2);
    let mib = arg("--mib", 64);
    let secs = arg("--seconds", 3600);
    if walkers * mib > 1024 {
        eprintln!(
            "refusing: memwalk total {} MiB exceeds 1 GiB",
            walkers * mib
        );
        std::process::exit(2);
    }
    let end = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut hs = Vec::new();
    for _ in 0..spin {
        hs.push(std::thread::spawn(move || {
            let mut x = 1u64;
            while std::time::Instant::now() < end {
                for _ in 0..100_000 {
                    x = x.wrapping_mul(6364136223846793005).wrapping_add(1);
                }
                std::hint::black_box(x);
            }
        }));
    }
    for w in 0..walkers {
        hs.push(std::thread::spawn(move || {
            let n = (mib as usize) << 20;
            let mut buf = vec![1u8; n];
            let mut i = w as usize * 4099;
            while std::time::Instant::now() < end {
                for _ in 0..1_000_000 {
                    i = (i * 1_103_515_245 + 12_345) % n;
                    buf[i] = buf[i].wrapping_add(1);
                }
            }
            std::hint::black_box(&buf);
        }));
    }
    for h in hs {
        let _ = h.join();
    }
}
