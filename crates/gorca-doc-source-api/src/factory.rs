// SPDX-License-Identifier: GPL-3.0-or-later
//! Per-application source factory (ADR-0003 AM-10, browse-mode design section 6).
//!
//! The core keeps no transport knowledge: everything that needs the application's bus name, its
//! object paths or its toolkit's quirks happens behind this trait, implemented by the daemon's
//! wiring over a connector. The cache-domain warm-up (Firefox serves AT-SPI from a lazily
//! populated parent-process cache) is one of those.

use std::time::Duration;

use gorca_model::{AppId, AppInfo, NodeKey};
use gorca_rt::BoxFuture;

use crate::ctx::DocSource;
use crate::request::SourceError;

/// What a warm-up did.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct WarmReport {
    /// The cache domains answer with content (tags present and stable).
    pub ready: bool,
    pub waited: Duration,
    pub probes: u32,
    /// Part of `waited` spent inside D-Bus calls (the application answering).
    pub blocked: Duration,
    /// Part of `waited` spent sleeping between sampling rounds (our own protocol).
    pub slept: Duration,
    pub rounds: u32,
    pub sampled_nodes: usize,
    /// Sampled nodes that carried a `tag` attribute (attribute completeness after the warm-up).
    pub tagged_nodes: usize,
}

/// Result of [`DocSourceFactory::warm`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WarmOutcome {
    /// The application has no document to probe yet; try again later.
    NoDocument,
    Done(WarmReport),
}

pub trait DocSourceFactory: Send + Sync + 'static {
    /// The application may host web documents (browser, Electron, a web view).
    fn is_web(&self, info: &AppInfo) -> bool;

    /// The application needs its cache domains activated before a document build.
    fn needs_warm(&self, info: &AppInfo) -> bool;

    /// Activate the cache domains of `app`. `hint` is any node the core already saw inside a
    /// document; without one the implementation finds a document itself (bounded walk).
    fn warm(
        &self,
        app: AppId,
        info: &AppInfo,
        hint: Option<NodeKey>,
    ) -> BoxFuture<'static, Result<WarmOutcome, SourceError>>;

    /// Walk from `focus` to the top-level web document that contains it. `Ok(None)` when the node
    /// is not inside a web document (browser chrome, a dialog).
    fn locate(
        &self,
        app: AppId,
        info: &AppInfo,
        focus: NodeKey,
    ) -> BoxFuture<'static, Result<Option<NodeKey>, SourceError>>;

    /// Create the source for the document rooted at `root`.
    fn create(
        &self,
        app: AppId,
        info: &AppInfo,
        root: NodeKey,
    ) -> BoxFuture<'static, Result<Box<dyn DocSource>, SourceError>>;
}
