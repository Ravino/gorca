// SPDX-License-Identifier: GPL-3.0-or-later
use std::sync::Arc;

use gorca_model::Epoch;

/// Braille is unchanged by ADR-0001: BrlAPI, same epoch, its own ADR later.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BrailleCaps {
    pub cells: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BrailleRegion {
    pub text: Arc<str>,
    pub cursor: Option<u32>,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BrailleError {
    #[error("no braille display available")]
    NotAvailable,
    #[error("braille display write failed")]
    Io,
}

pub trait BrailleBackend: Send + 'static {
    fn caps(&self) -> BrailleCaps;
    /// Same segments and epoch as speech. Stale epochs may be ignored by the backend.
    fn show(&self, regions: &[BrailleRegion], epoch: Epoch) -> Result<(), BrailleError>;
}
