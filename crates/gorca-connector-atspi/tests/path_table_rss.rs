// SPDX-License-Identifier: GPL-3.0-or-later
//! The path table's byte cap must bound real memory, not just its own accounting.
//!
//! Review finding (phase 1): with 52k live entries RSS grew 279 MiB, because `HashMap::clear`
//! keeps its allocation and the buckets of evicted owners stayed behind with that capacity.
//! This test runs in its own process (an integration test), so the RSS reading is not polluted
//! by other tests.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use gorca_connector_atspi::{Owner, PathTable};
use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};

fn rss_bytes() -> Option<usize> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let pages: usize = statm.split_whitespace().nth(1)?.parse().ok()?;
    Some(pages * 4096)
}

#[test]
fn rss_stays_near_the_byte_cap_under_owner_churn() {
    let Some(before) = rss_bytes() else {
        eprintln!("no /proc/self/statm; RSS test skipped");
        return;
    };
    let table = PathTable::new();
    // 4096 processes, each filling its quota: far more than the cap can hold, so buckets are
    // evicted over and over.
    for pid in 0..4096u32 {
        let app = AppId {
            connector: ConnectorId(1),
            serial: pid,
        };
        table.bind_app(app, Owner::Pid(pid));
        for n in 0..1024u64 {
            table.insert(
                NodeKey {
                    app,
                    local: LocalId(n),
                },
                "/org/a11y/atspi/accessible/1234567",
            );
        }
    }
    let after = rss_bytes().expect("statm readable a second time");
    let grown = after.saturating_sub(before);
    assert!(
        table.bytes() <= gorca_connector_atspi::PATH_TABLE_BYTE_CAP,
        "accounted {} bytes",
        table.bytes()
    );
    // The accounted cap is 64 MiB (B-1 PW-7); allow allocator slack and map load factor (three
    // times the cap), but nothing near the 17x seen before the fix.
    assert!(
        grown < 3 * gorca_connector_atspi::PATH_TABLE_BYTE_CAP,
        "RSS grew {} MiB for {} live entries",
        grown / (1024 * 1024),
        table.len()
    );
}
