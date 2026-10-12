//! Extension host: manifests, matching, limits and instance health. The wasmtime embedding is
//! not scaffolded yet.
//!
//! Responsibility (ADR-0001 sections 10 and 11): "wasmtime embedding, limits, manifests".
//!
//! > "The manifest declares a matcher (`app_name`, `toolkit`, `connector`, `document-role`
//! > globs), hooks, keymap and capabilities (`speak`, `fetch:prefetch`, `fetch:background`,
//! > `settings`). Order when several match: app-specific, then toolkit-specific, then
//! > connector-specific; `present` takes the first `Some`, and `on-event` verdicts compose left
//! > to right."
//! > "Limits: 2 ms CPU for `on-event` and `present`, 20 ms for `command`, 32 MiB per instance;
//! > three resets in 60 s disable the extension for that app, and the user is told once."
//!
//! TBD (`wasm-engineer`): wasmtime `Engine`/`Store` per (extension, app), fuel or epoch
//! interruption, `ResourceLimiter`, component bindings from `gorca-ext-api`'s WIT, and the
//! manifest file format. `wasmtime` is pinned in the workspace but not yet a dependency, because
//! it dominates build time.
//!
//! Allowed dependencies: `gorca-ext-api`, `gorca-model`, `gorca-rt`. The core must never depend
//! on this crate (section 11, rule 3); only `gorca-daemon` wires it in.

mod glob;
mod health;
mod manifest;

pub use glob::glob_match;
pub use health::{InstanceGuard, InstanceHealth};
pub use manifest::{Capabilities, HookSet, Manifest, Matcher, Specificity, order_matching};
