//! Hard resource bounds (B-1 SPEC section 7). Every bound is a requirement for every fill
//! strategy: overflow degrades to "less cached" (a miss), never to unbounded growth, a wrong
//! presentation or starvation of the Interactive class.

use std::time::Duration;

/// Live nodes per app mirror.
pub const MAX_MIRROR_NODES: usize = 65_536;
/// Mirror bytes per app (names and descriptions clipped at [`MAX_TEXT_BYTES`]).
pub const MAX_MIRROR_BYTES: u64 = 64 * 1024 * 1024;
/// Mirror bytes over all apps.
pub const GLOBAL_MIRROR_BYTES: u64 = 256 * 1024 * 1024;
/// The focused app and the Shell keep at least this many nodes under global pressure.
pub const MIN_PROTECTED_NODES: usize = 4_096;
/// Tombstones per app are bounded by `2 * live + TOMBSTONE_SLACK`.
pub const TOMBSTONE_SLACK: usize = 4_096;
pub const TOMBSTONE_TTL: Duration = Duration::from_secs(10);
/// Entries one `sweep` call looks at.
pub const SWEEP_BUDGET: usize = 256;
/// Invalidations of keys not in the store that are remembered individually (late-reply rule
/// LR-2 for fetches that were in flight for a miss); beyond it a conservative floor applies.
pub const MAX_RECENT_INVALIDATIONS: usize = 4_096;

pub const MAX_TEXT_BYTES: usize = 2_048;
pub const MAX_CHILD_COUNT: u32 = 65_536;
pub const MAX_ANCESTRY_DEPTH: u8 = 64;
pub const MAX_SUBTREE_NODES: u32 = 4_096;
pub const MAX_CHILD_RANGE: u32 = 1_024;
pub const MAX_CACHE_DUMP_NODES: u32 = 65_536;
pub const MAX_CACHE_DUMP_BYTES: usize = 16 * 1024 * 1024;
pub const CACHE_DUMP_INTERVAL: Duration = Duration::from_secs(30);

/// In-flight fill requests per app.
pub const MAX_IN_FLIGHT: usize = 8;
/// Slots only Interactive requests may use.
pub const INTERACTIVE_RESERVED: usize = 2;
/// Background requests (including `CacheDump`) in flight per app.
pub const MAX_BACKGROUND_IN_FLIGHT: usize = 2;
/// Queued policy requests per app.
pub const MAX_PENDING_EFFECTS: usize = 256;
pub const FILL_RATE_NON_FOCUSED: u32 = 500;
pub const FILL_RATE_FOCUSED: u32 = 2_000;
pub const FILL_RATE_GLOBAL: u32 = 5_000;
/// Policy-owned memory per app.
pub const FILL_STATE_MAX_BYTES: usize = 4 * 1024 * 1024;

/// The section 7 values as a value, so tests can shrink them.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FillLimits {
    pub max_nodes: usize,
    pub max_bytes: u64,
    pub max_in_flight: usize,
    pub interactive_reserved: usize,
    pub max_background: usize,
    pub max_pending: usize,
    pub rate_non_focused: u32,
    pub rate_focused: u32,
    pub dump_interval: Duration,
    pub max_dump_nodes: u32,
    pub max_subtree_nodes: u32,
    pub max_child_range: u32,
    pub max_ancestry_depth: u8,
    pub state_max_bytes: usize,
}

impl Default for FillLimits {
    fn default() -> Self {
        Self {
            max_nodes: MAX_MIRROR_NODES,
            max_bytes: MAX_MIRROR_BYTES,
            max_in_flight: MAX_IN_FLIGHT,
            interactive_reserved: INTERACTIVE_RESERVED,
            max_background: MAX_BACKGROUND_IN_FLIGHT,
            max_pending: MAX_PENDING_EFFECTS,
            rate_non_focused: FILL_RATE_NON_FOCUSED,
            rate_focused: FILL_RATE_FOCUSED,
            dump_interval: CACHE_DUMP_INTERVAL,
            max_dump_nodes: MAX_CACHE_DUMP_NODES,
            max_subtree_nodes: MAX_SUBTREE_NODES,
            max_child_range: MAX_CHILD_RANGE,
            max_ancestry_depth: MAX_ANCESTRY_DEPTH,
            state_max_bytes: FILL_STATE_MAX_BYTES,
        }
    }
}
