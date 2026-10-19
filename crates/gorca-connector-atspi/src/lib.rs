// SPDX-License-Identifier: GPL-3.0-or-later
//! AT-SPI connector: toolkit normalisation tables, `AtspiDocSource`, `org.a11y.Status`
//! enablement.
//!
//! Responsibility (ADR-0001 sections 5 and 11): "AT-SPI connector, toolkit normalisation tables,
//! `AtspiDocSource`, `org.a11y.Status` enablement (ID-5)". The only crate allowed to depend on
//! `atspi*` (section 11, rule 2).
//!
//! > "C-4: Raw roles and states map to core `Role`/`StateSet` through tables. Unknown values map
//! > to `Role::Unknown` plus a raw attribute."
//! > "C-6: All app-supplied data is untrusted. Decoding errors and panics are contained in the
//! > connector."
//! > "D-6: We do not use `atspi::AccessibilityConnection`, which cannot be configured and whose
//! > `p2p` startup calls every app in sequence with no timeout." We use `atspi` for its types
//! > and proxies only, over our own connections from `gorca-dbus`.
//!
//! Phase 1 (first vertical slice): the connector connects to the a11y bus (`org.a11y.Bus`),
//! applies ID-5 enablement, registers its event listeners, runs the ADR-0004 raw drain with
//! an [`AtspiRouter`] that announces each application on its first event, and serves the
//! core's `Fetch`/`Probe` requests (name, role, state, value) over the call connection with a
//! deadline on every call. Applications are retired on `NameOwnerChanged`.
//!
//! Not yet: P2P calls (D-7; requests use the bus call connection, which D-7 allows),
//! `GetItems` cache fill (B-1), `AtspiDocSource` build (section 9.4), D-5 per-app
//! deregistration, and `atspi::Role` completeness.
//!
//! Allowed dependencies: `gorca-connector-api`, `gorca-doc-source-api`, `gorca-dbus`,
//! `gorca-model`, `gorca-rt`, `atspi`, `zbus`.

mod connector;
mod decode;
mod doc_source;
mod enablement;
mod events;
mod fetch;
#[cfg(test)]
mod legacy_agreement_tests;
mod proc;
mod role_map;
mod sem_profile;
mod text;
mod toolkit;

// Kept at the crate root so intra-crate paths (`crate::router`, ...) stay stable.
use events::{paths, router, session, subscription};

pub use connector::{AtspiConnector, AtspiLabels, AtspiOptions, AtspiState};
pub use decode::{role_from_raw, states_from_raw};
pub use doc_source::AtspiDocSource;
pub use enablement::{Enablement, StatusValues};
pub use fetch::items::{
    Decoded, DumpStats, ItemsError, MAX_DUMP_BYTES, MAX_DUMP_NODES, decode_get_items,
};
pub use fetch::locate::locate_object;
pub use fetch::{FetchCounters, FetchCountersSnapshot};
pub use paths::{GLOBAL_BYTE_CAP as PATH_TABLE_BYTE_CAP, Owner, PathTable};
pub use role_map::map_role;
pub use router::{AtspiRouter, RouterStats};
pub use session::REGISTERED_EVENTS;
pub use subscription::Subscription;
pub use toolkit::Toolkit;
