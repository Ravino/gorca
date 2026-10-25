// SPDX-License-Identifier: GPL-3.0-or-later
//! The production WASM plugin runtime (ADR-0026, milestone M18a-1).
//!
//! Plugin failure never affects the core: every guest call runs on a plugin actor thread under a
//! fuel grant, a memory cap and size caps; a fault drops the instance and becomes a strike in the
//! writer-owned [`slot::Slot`] state machine.
//!
//! Layout:
//! * [`call`] - the call-model boundary: messages, [`call::WLaneRunner`], the wasmtime executor;
//! * [`actor`] - the B' implementation (owner decision 2026-10-25): actors and a bounded futex wait;
//! * [`slot`] - strike / backoff / disable state machine;
//! * [`instance`], [`host`], [`view`], [`limits`] - engine, imports, snapshot reads, budgets;
//! * [`cache`], [`mac`] - the MAC-verified `.cwasm` cache and keyed primitives;
//! * [`package`], [`trust`], [`handles`] - packages, tiers, permissions, system scopes, handles.
//!
//! Not wired into the daemon yet. Nothing here names a connector type.

pub mod actor;
pub mod cache;
pub mod call;
pub mod fsx;
pub mod handles;
pub mod host;
pub mod instance;
pub mod limits;
pub mod mac;
pub mod package;
pub mod slot;
pub mod trust;
pub mod view;
pub mod wasi;
