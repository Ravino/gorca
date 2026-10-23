// SPDX-License-Identifier: GPL-3.0-or-later
//! Client of the GNOME Shell helper `org.gorca.ShellHelper` v2 (ADR-0025 sections 2.5, 2.7, 2.9).
//!
//! Responsibility: the helper's D-Bus protocol (`wire`), the pure availability machine (`avail`),
//! and the pointer actor with its [`ShellHelperClient`] handle, which implements
//! `gorca_output_api::PointerSink`. Capture (ADR-0002) moves here later.
//!
//! > "EX-1 The Reader never does pointer I/O ... One pointer actor owns the helper proxy, the
//! > `PointerAvail` machine and the window cache; it runs on its own task and connection-call
//! > budget (ADR-0001 R3), never on the input lane."
//! > "MI-7 gorca calls the helper only if its owner equals the owner of `org.gnome.Shell`."
//!
//! Not here: the window matcher (WM-1..4, MO-5a), the runtime-gated key grabs and the hold
//! machines (MO-3, MO-4), which consume [`PointerAvail`] and the sink in `gorca-core`.
//!
//! Allowed dependencies: `gorca-output-api`, `gorca-rt`, `gorca-dbus`, `zbus` (rule 2 of
//! ADR-0001 section 11 lists this crate).
#![deny(clippy::wildcard_enum_match_arm)]

pub mod avail;
mod client;
pub mod matcher;
pub mod wire;

pub use avail::{AvailEffect, AvailEvent, AvailMachine, ProbeResult};
pub use client::{LATE_CAP, MAX_CALL_DEADLINE, PENDING, PROBE_STEP, ShellHelperClient};
pub use gorca_output_api::PointerAvail;
pub use matcher::{MatchError, MonitorRow, WindowRow, resolve};
pub use wire::{HELPER_INTERFACE, HELPER_NAME, HELPER_PATH, Origin, SHELL_NAME, WireTarget};
