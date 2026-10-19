// SPDX-License-Identifier: GPL-3.0-or-later
//! The one normaliser from raw AT-SPI data (role `u32`, state bits, attributes, relations) to
//! semantic records (ADR-0022 SM-4, ADR-0024 section 2.1).
//!
//! Empty until M22a-B2. Pure and tokio-free; no `zbus`, no `atspi` crate (it takes plain
//! values). Only `gorca-connector-atspi` and `gorca-doc-source-atspi` may depend on it; the
//! normaliser never runs inside `gorca-core` (H-7).
