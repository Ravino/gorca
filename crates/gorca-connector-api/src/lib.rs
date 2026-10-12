//! The connector boundary (ADR-0001 section 5.1).
//!
//! Responsibility (section 11): `Connector`, `ConnectorCtx`, `AppPort`,
//! `BackendRequest`/`Op`/`Reply`/`Error`.
//!
//! > "The boundary is message-based, not a callback trait: connectors keep their own
//! > concurrency, the core cannot be re-entered, and an out-of-process connector keeps the same
//! > contract."
//!
//! Connector obligations (conformance suite in `gorca-testkit`):
//! - **C-1** `push` never blocks; per-call cost < 1 us p99.
//! - **C-2** Every `BackendRequest` gets exactly one reply, or the oneshot is dropped, by
//!   `deadline + 5 ms`. A dropped sender counts as `Timeout`.
//! - **C-3** The connector does its own `Busy` throttling (in-flight limit per app).
//! - **C-4** Raw roles and states map to core `Role`/`StateSet` through tables; unknown values
//!   map to `Role::Unknown` plus a raw attribute.
//! - **C-5 / C-5a** Each app scope is owned by exactly one connector; a `DocSource` from another
//!   transport may attach to a document but never owns the app, its focus or its windows.
//! - **C-6** All app-supplied data is untrusted; decoding errors and panics stay inside the
//!   connector.
//!
//! Allowed dependencies (section 11): "depends only on `gorca-model`, `gorca-rt`,
//! `gorca-ingress`" (plus `thiserror` and `smallvec`). The core depends on this crate, never on a
//! concrete connector (rule 3).

mod backend;
mod call;
mod connector;
mod port;
mod registry;

pub use backend::{
    ActionReq, BackendError, BackendOp, BackendReply, BackendRequest, MatchRule, TextAttrRun,
};
pub use call::{REPLY_GRACE, request};
pub use connector::{Connector, ConnectorCtx, ConnectorError};
pub use port::{AppChannel, AppControl, AppPort, PortError, app_channel};
pub use registry::{
    AppRegistrar, RegistrarError, RegistrarRequest, ScopeKey, ScopeRegistry, registrar_channel,
};
