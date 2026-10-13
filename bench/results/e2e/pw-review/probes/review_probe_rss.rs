//! Reviewer probe: repeated invalidations of one key that is not in the mirror (a text-changed
//! storm on an uncached terminal node) must not grow memory without bound.
#![allow(clippy::unwrap_used)]
use std::time::Instant;
use gorca_mirror::mirror;
use gorca_model::{AppId, ConnectorId, LocalId, NodeKey, PropMask};

fn rss_kib() -> u64 {
    std::fs::read_to_string("/proc/self/status").ok().and_then(|s| s.lines()
        .find_map(|l| l.strip_prefix("VmRSS:")).and_then(|v| v.split_whitespace().next()?.parse().ok())).unwrap_or(0)
}

#[test]
fn probe_recent_order_is_bounded() {
    let (mut w, _r) = mirror();
    let k = NodeKey { app: AppId { connector: ConnectorId(0), serial: 1 }, local: LocalId(7) };
    let now = Instant::now();
    let before = rss_kib();
    for _ in 0..10_000_000u32 {
        w.invalidate(k, PropMask::NAME | PropMask::VALUE, now);
        w.sweep(now);
    }
    let grown = rss_kib().saturating_sub(before);
    eprintln!("RSS grew {grown} KiB after 10M invalidations of one unknown key");
    assert!(grown < 8 * 1024, "unbounded growth: {grown} KiB");
}
