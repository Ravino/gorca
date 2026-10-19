// SPDX-License-Identifier: GPL-3.0-or-later
//! Test kit: fake connector, fake `DocSource`, virtual clock, conformance suites, storm
//! generator.
//!
//! Responsibility (ADR-0001 sections 5.1, 9.2 and 11): "fake connector, fake DocSource,
//! scripted apps, virtual clock, conformance suites (C-*, SL-*), storm generator, private-bus
//! spawner".
//!
//! > "Connector obligations (conformance suite in `gorca-testkit`): C-1 ... C-6."
//!
//! Phase 0 has [`FakeConnector`] (scripted apps, selectable request behaviour), [`FakeDocSource`],
//! the conformance checks [`conformance::c1_push_cost`], [`conformance::c2_requests_are_answered`]
//! and [`conformance::sl1_sink_never_blocks`], and the deterministic [`storm`] generator.
//!
//! Not here: the private D-Bus session/a11y bus spawner and fake AT-SPI applications. They live
//! in `tests/harness/` (owned by test-engineer) and will be wrapped here once that harness has
//! a stable Rust-facing API.
//!
//! This is a dev-support crate: it may depend on anything in the neutral layer and on tokio's
//! `test-util`, but never on a transport crate, so conformance runs without a bus.

pub mod b1;
pub mod conformance;
pub mod cpu;
pub mod fact_conformance;
mod fake_connector;
mod fake_doc_source;
mod fake_source;
pub mod harness;
mod scripted;
mod speech_server;
mod storm;

pub use fake_connector::{Behavior, FakeConnector, FakeScript};
pub use fake_doc_source::{FakeDocFactory, ScriptHandle, ScriptedDocSource, Seen, WarmScript};
pub use fake_source::{FakeDocSource, FakeFactSource, SourceOutput};
pub use gorca_rt::VirtualClock;
pub use scripted::{ScriptedApp, ScriptedConnector, ScriptedHandle, ScriptedNode};
pub use speech_server::{MockProto, MockSpeechServer, Spoken};
pub use storm::{StormGen, storm};
