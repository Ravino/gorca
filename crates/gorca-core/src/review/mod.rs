//! Review cursor navigation (milestone M4). Empty seam: the Reader already routes
//! [`ReviewMsg`] here, so M4 adds variants and handlers without editing `reader.rs`.

use crate::reader::{Reader, ReaderEffects};

/// Messages for the review arm. No variants yet, so the arm cannot be produced.
#[derive(Debug)]
pub enum ReviewMsg {}

impl Reader {
    #[allow(clippy::unused_self, clippy::needless_pass_by_value)] // empty seam, see module docs
    pub(crate) fn on_review(&mut self, m: ReviewMsg, _fx: &mut ReaderEffects) {
        match m {}
    }
}
