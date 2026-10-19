// SPDX-License-Identifier: GPL-3.0-or-later
//! The one normaliser from raw AT-SPI data (role `u32`, state bits, attributes) to semantic
//! source records (ADR-0022 SM-4, ADR-0024 section 2.1, M22a-B4).
//!
//! Pure and tokio-free; no `zbus`, no `atspi` crate (it takes plain values). Only
//! `gorca-connector-atspi` and `gorca-doc-source-atspi` may depend on it; the normaliser never
//! runs inside `gorca-core` (H-7, `xtask deps` rule 10).
//!
//! * [`roles`]: raw role table, `xml-roles` tokens (multi-token fallback, invalid tokens), the
//!   engine-vs-ARIA rule, the ARIA 1.2 role coverage list.
//! * [`states`]: the one state mapping (INDETERMINATE becomes `Check::Mixed` on checkables) and
//!   the AT-SPI state coverage list.
//! * [`attrs`]: attribute parsers and the ARIA property coverage list.
//! * [`record`]: [`RawNode`] in, [`SrcRecord`] out, with Unknown vs Absent taken from the fetched
//!   mask.

pub mod attrs;
pub mod record;
pub mod roles;
pub mod states;
pub mod tables;

pub use record::{Notes, RawNode, SrcRecord, TagName, normalise};
pub use roles::{EngineRole, RoleSource, TokenKind};
pub use states::StateBits;
