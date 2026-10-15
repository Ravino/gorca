//! Extension host: the wasmtime embedding, limits, manifests, matching and keymap merge.
//!
//! Responsibility (ADR-0001 sections 10 and 11, ADR-0008 section 3.3): load extensions, run their
//! hooks under limits, contain every fault, and hand the core the hook points it declares in
//! `gorca_core::ext` (`ExtSource`, `AppExt`).
//!
//! > "The manifest declares a matcher (`app_name`, `toolkit`, `connector`, `document-role`
//! > globs), hooks, keymap and capabilities (`speak`, `fetch:prefetch`, `fetch:background`,
//! > `settings`). Order when several match: app-specific, then toolkit-specific, then
//! > connector-specific; `present` takes the first `Some`, and `on-event` verdicts compose left
//! > to right."
//! > "Limits: 2 ms CPU for `on-event` and `present`, 20 ms for `command`, 32 MiB per instance;
//! > three resets in 60 s disable the extension for that app, and the user is told once."
//!
//! # Shape
//!
//! ```text
//! ExtHost (ExtSource)           one per process, built by the daemon
//!   LoadedExt                   manifest + factory (WASM component or native constructor)
//!   AppExts (AppExt)            the extensions that matched one application
//!     Slot                      one extension x one application: instance + InstanceGuard
//!       WasmInstance            one wasmtime Store (epoch deadline, 32 MiB, pooling allocator)
//!       NativeInstance          catch_unwind + post-hoc deadline (no hang protection)
//!   worker thread               commands (20 ms limit) and settings flushes, off every hot path
//! ```
//!
//! Hooks run in the caller's thread: `on-event` in the app actor, `present` in the Reader (a
//! `try_lock`: a busy extension is "no opinion"), `command` in the worker. A fault never leaves
//! the slot: the instance is dropped, a fresh one is built from the `InstancePre`, and the third
//! reset in 60 seconds disables the extension for that application with one spoken notice.
//!
//! The core must never depend on this crate (rule 3); only `gorca-daemon` wires it in.

mod convert;
mod glob;
mod health;
mod host;
mod instance;
mod kv;
mod manifest;
mod manifest_file;
mod native;
mod slot;
mod wasi_pure;
mod wasm;

pub use convert::{
    MAX_ANSWER_CHARS, MAX_NODE_TEXT_CHARS, MAX_SEGMENT_CHARS, MAX_SEGMENTS, app_from_ext,
    app_to_ext, event_to_ext, key_from_ext, key_to_ext, node_to_ext, role_to_ext,
    segments_from_ext,
};
pub use glob::glob_match;
pub use health::{InstanceGuard, InstanceHealth};
pub use host::{AppExts, ExtHost, ExtHostBuilder, LoadReport, MAX_EXTENSIONS};
pub use instance::{ExtStats, ExtStatsSnapshot, Fault, HookInstance, HookKind, InstShared};
pub use kv::{MAX_KEY_BYTES, MAX_KEYS, MAX_VALUE_BYTES, SettingsKv};
pub use manifest::{
    Capabilities, ExtBackend, HookSet, KeymapEntry, Manifest, Matcher, Specificity, order_matching,
};
pub use manifest_file::{MAX_MANIFEST_BYTES, ManifestError};
pub use native::{NativeFactory, NativeInstance};
pub use slot::{Factory, LoadedExt, Slot};
pub use wasm::{ExtEngine, POOL_INSTANCES, TICK, WasmInstance, WasmModule};
