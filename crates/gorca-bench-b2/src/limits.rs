//! Pass thresholds of bake-off B-2. Single source of truth: SPEC.md section 6 quotes these.
//! A candidate passes a metric when its **median over the runs** satisfies the threshold; the
//! min/max over runs are reported so variance is visible.

/// T1: sustained events/s with no disconnect and the backlog bounds below (ADR M-3, M-11).
pub const T1_SUSTAINED_EPS_MIN: f64 = 100_000.0;
/// T1: broker-side backlog p99 while sustaining (M-11: SIOCINQ p99 < 1 MiB).
pub const T1_BACKLOG_P99_MAX_BYTES: f64 = 1024.0 * 1024.0;
/// T1: probe lag p99 while sustaining (M-3: Reader lag < 20 ms, all of it ours).
pub const T1_LAG_P99_MAX_US: f64 = 20_000.0;

/// T2: socket to coalescer output, microseconds.
pub const T2_P50_MAX_US: f64 = 300.0;
pub const T2_IDLE_P99_MAX_US: f64 = 500.0;
pub const T2_10K_P99_MAX_US: f64 = 1_000.0;
pub const T2_10K_P999_MAX_US: f64 = 3_000.0;
pub const T2_100K_P99_MAX_US: f64 = 3_000.0;
pub const T2_100K_P999_MAX_US: f64 = 10_000.0;

/// T3: focus event latency under a 40k ev/s storm, microseconds. The keypress budget is 15 ms
/// p99 for all of gorca; ingress may take at most a seventh of it.
pub const T3_P99_MAX_US: f64 = 2_000.0;
pub const T3_P999_MAX_US: f64 = 8_000.0;

/// SPEC 3.6 / AU-3: per-app reserve R held outside the shared pool. B-3 bound =
/// `global_budget + apps * (R + 1)`. Equals `IngressConfig::default().reserved_slots`.
pub const APP_RESERVE: usize = gorca_ingress::RESERVED_SLOTS;
/// T4: CPU milliseconds per 10k events (all non-harness threads).
pub const T4_CPU_MS_PER_10K_MAX: f64 = 25.0;
/// T4: drain thread share of one core at 100k ev/s (M-11: < 60 %).
pub const T4_DRAIN_CORE_PCT_MAX: f64 = 60.0;

/// T5: peak RSS growth over the pre-run baseline, MiB (M-8 / A5: a guard rail, not frugality).
pub const T5_RSS_DELTA_MAX_MIB: f64 = 256.0;
/// T5: pending slots after the hard cap (SPEC 4.4 rule H) plus one batch of slack.
pub const T5_PENDING_MAX: f64 = 65_536.0 + 4_096.0;

/// Mock broker quota for T1 (bytes a reader may be behind before it is disconnected).
pub const MOCK_QUOTA_BYTES: usize = 64 * 1024 * 1024;
/// `dbus-broker` per-uid pool for the real-broker runs.
pub const BROKER_POOL_BYTES: u64 = 64 * 1024 * 1024;

/// T7: cost of one `push`, nanoseconds (R-4: allocation-free in steady state, O(1) amortised).
pub const T7_PUSH_P99_MAX_NS: f64 = 1_000.0;
pub const T7_PUSH_P999_MAX_NS: f64 = 5_000.0;
/// T7 with four producers contending on one app.
pub const T7_PUSH4_P99_MAX_NS: f64 = 5_000.0;
pub const T7_PUSH4_P999_MAX_NS: f64 = 50_000.0;
