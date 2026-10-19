// SPDX-License-Identifier: GPL-3.0-or-later
//! Fact sources (M22a-C4, ADR-0024 section 2.2): the daemon registers every source the process
//! runs, so `debug.explain_last` can name what produced a fused value.
//!
//! Today that is one: the AT-SPI connector, a Primary source of trust tier T1 at `SourceId{0,0}`
//! (slot 0, generation 0: the registry's first slot is never recycled). The Newton connector and
//! the secondary sources of M23 register here when they exist; the core never imports a
//! connector's types, it only sees names and ids.

use gorca_core::{Core, SourceDirectory};

/// The sources of this process.
pub(crate) fn sources() -> SourceDirectory {
    SourceDirectory::with_atspi_primary()
}

/// Hand the registered sources to the core.
pub(crate) fn wire_sources(core: Core) -> Core {
    core.with_sources(sources())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_core::ATSPI_SOURCE;
    use gorca_model::sem::{SourceClass, TrustTier};

    #[test]
    fn atspi_is_the_primary_at_slot_zero() {
        let d = sources();
        let a = d.get(ATSPI_SOURCE);
        assert_eq!(a.map(|s| s.name), Some("atspi"));
        assert_eq!(
            a.map(|s| (s.class, s.tier)),
            Some((SourceClass::Primary, TrustTier::T1Platform))
        );
        assert_eq!((ATSPI_SOURCE.slot, ATSPI_SOURCE.generation), (0, 0));
        assert_eq!(d.len(), 1);
    }
}
