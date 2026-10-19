// SPDX-License-Identifier: GPL-3.0-or-later
//! The fusion point (ADR-0022 section 2.2, ADR-0024 section 2.2): a pure function of registers,
//! static tables and an epoch snapshot, evaluated in strata S-1, S0..S4 and R.
//!
//! Empty until M22a-B1a. Dependency rules (`cargo xtask deps`): tokio-free and transport-neutral;
//! depends on `gorca-model` only.
