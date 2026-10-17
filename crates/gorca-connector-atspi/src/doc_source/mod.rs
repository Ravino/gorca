// SPDX-License-Identifier: GPL-3.0-or-later
use gorca_doc_source_api::{
    ActionCaps, DocSource, DocSourceCtx, JoinKeyCaps, SourceCaps, SourceError, SourceKind,
};
use gorca_model::TextUnit;
use gorca_rt::BoxFuture;

/// The mandatory AT-SPI fallback source under every web document mirror (section 9.1).
/// Phase 0: attaches and idles until shutdown.
#[derive(Debug)]
pub struct AtspiDocSource {
    text_unit: TextUnit,
}

impl AtspiDocSource {
    /// `text_unit` comes from the toolkit table ([`crate::Toolkit::text_unit`]).
    pub fn new(text_unit: TextUnit) -> Self {
        Self { text_unit }
    }
}

impl DocSource for AtspiDocSource {
    fn kind(&self) -> SourceKind {
        SourceKind::Atspi
    }

    fn caps(&self) -> SourceCaps {
        SourceCaps {
            bulk_snapshot: false,
            push_deltas: true,
            settled_markers: false,
            text_unit: self.text_unit,
            actions: ActionCaps::FOCUS
                | ActionCaps::CLICK
                | ActionCaps::SCROLL_INTO_VIEW
                | ActionCaps::SET_CARET
                | ActionCaps::SET_SELECTION,
            // The AT-SPI object reference is the join key itself.
            join_keys: JoinKeyCaps::EXACT,
            silent_reorders: false,
        }
    }

    fn attach(self: Box<Self>, ctx: DocSourceCtx) -> BoxFuture<'static, Result<(), SourceError>> {
        Box::pin(async move {
            ctx.shutdown.cancelled().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_advertise_exact_joins_and_all_actions() {
        let s = AtspiDocSource::new(TextUnit::Utf16Bom);
        assert_eq!(s.kind(), SourceKind::Atspi);
        let caps = s.caps();
        assert_eq!(caps.text_unit, TextUnit::Utf16Bom);
        assert!(caps.join_keys.contains(JoinKeyCaps::EXACT));
        assert!(caps.actions.contains(ActionCaps::SET_CARET));
    }
}
