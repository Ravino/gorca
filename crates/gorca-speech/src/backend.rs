// SPDX-License-Identifier: GPL-3.0-or-later
//! Backend factory abstraction. `gorca-speech` knows no concrete backend (rule 3); the daemon
//! wraps `VoicegoClient::connect` and `SsipClient::connect` into [`SpeechBackend`]s.

use std::future::Future;
use std::sync::Arc;

use gorca_output_api::SpeechClient;
use gorca_rt::{BoxFuture, Deadline};

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
#[error("{backend}: {reason}")]
pub struct BackendConnectError {
    pub backend: &'static str,
    pub reason: Arc<str>,
}

impl BackendConnectError {
    pub fn new(backend: &'static str, reason: impl Into<Arc<str>>) -> Self {
        Self {
            backend,
            reason: reason.into(),
        }
    }
}

/// Opens one connection to a speech service. Cheap to call repeatedly: failover probes the
/// primary in the background.
pub trait SpeechBackend: Send + Sync + 'static {
    fn name(&self) -> &'static str;

    /// Must respect `deadline`; the caller also enforces it.
    fn connect(
        &self,
        deadline: Deadline,
    ) -> BoxFuture<'static, Result<Box<dyn SpeechClient>, BackendConnectError>>;
}

/// A [`SpeechBackend`] from a closure, for the daemon and for tests.
pub struct FnBackend<F> {
    name: &'static str,
    connect: F,
}

impl<F> std::fmt::Debug for FnBackend<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FnBackend")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

impl<F, Fut> FnBackend<F>
where
    F: Fn(Deadline) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Box<dyn SpeechClient>, BackendConnectError>> + Send + 'static,
{
    pub fn new(name: &'static str, connect: F) -> Self {
        Self { name, connect }
    }
}

impl<F, Fut> SpeechBackend for FnBackend<F>
where
    F: Fn(Deadline) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Box<dyn SpeechClient>, BackendConnectError>> + Send + 'static,
{
    fn name(&self) -> &'static str {
        self.name
    }

    fn connect(
        &self,
        deadline: Deadline,
    ) -> BoxFuture<'static, Result<Box<dyn SpeechClient>, BackendConnectError>> {
        Box::pin((self.connect)(deadline))
    }
}
