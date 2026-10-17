// SPDX-License-Identifier: GPL-3.0-or-later
//! Event side of the connector: the raw-drain router, event registration and the per-owner
//! object-path table. Milestone M2 owns this module and `fetch/`.

pub(crate) mod paths;
pub(crate) mod router;
pub(crate) mod session;
pub(crate) mod subscription;
