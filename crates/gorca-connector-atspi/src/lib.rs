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
//! Phase 0 contains the [`Connector`](gorca_connector_api::Connector) and
//! [`DocSource`](gorca_doc_source_api::DocSource) shells (they run until shutdown and announce
//! nothing), the role table seed, the toolkit table with its text units (TU-2), and the pure
//! enablement plan (ID-5). TBD: registry watching, event connections and drains (B-2), P2P
//! calls (D-7), `GetItems` fill (B-1), `AtspiDocSource` build (section 9.4), and
//! `atspi::Role` completeness.
//!
//! Allowed dependencies: `gorca-connector-api`, `gorca-doc-source-api`, `gorca-dbus`,
//! `gorca-model`, `gorca-rt`, `atspi`, `zbus`.

mod connector;
mod doc_source;
mod enablement;
mod role_map;
mod toolkit;

pub use connector::AtspiConnector;
pub use doc_source::AtspiDocSource;
pub use enablement::{Enablement, StatusValues};
pub use role_map::map_role;
pub use toolkit::Toolkit;
