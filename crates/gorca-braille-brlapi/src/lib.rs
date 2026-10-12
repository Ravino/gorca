//! BrlAPI braille backend. Stub until the braille ADR.
//!
//! Responsibility (ADR-0001 section 11): "`gorca-braille-brlapi/` stub until the braille ADR".
//! Braille is "unchanged: BrlAPI, same epoch, and its own ADR later" (section 8.3).
//!
//! Allowed dependencies: `gorca-output-api`. TBD: BrlAPI bindings (C interop) come with the ADR.

use gorca_model::Epoch;
use gorca_output_api::{BrailleBackend, BrailleCaps, BrailleError, BrailleRegion};

/// Reports "no display" for every call.
#[derive(Debug, Default)]
pub struct BrlapiBackend;

impl BrailleBackend for BrlapiBackend {
    fn caps(&self) -> BrailleCaps {
        BrailleCaps { cells: 0 }
    }

    fn show(&self, _regions: &[BrailleRegion], _epoch: Epoch) -> Result<(), BrailleError> {
        Err(BrailleError::NotAvailable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_no_display() {
        let b = BrlapiBackend;
        assert_eq!(b.caps().cells, 0);
        assert_eq!(b.show(&[], Epoch(1)), Err(BrailleError::NotAvailable));
    }
}
