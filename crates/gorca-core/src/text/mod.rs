//! Text-caret and text-presentation handling (milestone M3). Empty seam: the Reader already
//! routes [`TextMsg`] here, so M3 adds variants and handlers without editing `reader.rs`.

use crate::reader::{Reader, ReaderEffects};

/// Messages for the text arm. No variants yet, so the arm cannot be produced.
#[derive(Debug)]
pub enum TextMsg {}

impl Reader {
    #[allow(clippy::unused_self, clippy::needless_pass_by_value)] // empty seam, see module docs
    pub(crate) fn on_text(&mut self, m: TextMsg, _fx: &mut ReaderEffects) {
        match m {}
    }
}
