// SPDX-License-Identifier: GPL-3.0-or-later
//! `render`: `PresIr` to tokens. The only producer of text in the pipeline.

use std::sync::Arc;

use gorca_model::SegmentKind;
use smallvec::SmallVec;

use crate::ir::{Content, PresIr};

/// Token text: a catalog string or shared literal text. No allocation for catalog words.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenText {
    Static(&'static str),
    Shared(Arc<str>),
}

impl TokenText {
    pub fn as_str(&self) -> &str {
        match self {
            TokenText::Static(s) => s,
            TokenText::Shared(s) => s,
        }
    }
}

/// One rendered token: what the speech lane tags and speaks.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Token {
    pub kind: SegmentKind,
    pub text: TokenText,
}

/// Render settings.
#[derive(Copy, Clone, Debug, Default)]
pub struct RenderOpts {
    /// `browse.description_pause`: descriptions are a separate sentence.
    pub description_pause: bool,
}

/// Render `ir`; empty content renders nothing.
pub fn render(ir: &PresIr, opts: RenderOpts) -> SmallVec<[Token; 6]> {
    let mut out = SmallVec::new();
    for f in &ir.fields {
        let text = match &f.content {
            Content::Word(id) => TokenText::Static(gorca_i18n::t(*id)),
            Content::Text(s) => TokenText::Shared(Arc::clone(s)),
        };
        if text.as_str().is_empty() {
            continue;
        }
        out.push(Token {
            kind: f.kind.segment_kind(opts.description_pause),
            text,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use gorca_i18n::MsgId as M;
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};

    use super::*;
    use crate::ir::{Field, FieldKind, Origin, Reason};

    fn ir(fields: Vec<Field>) -> PresIr {
        PresIr {
            reason: Reason::Focus,
            node: NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                local: LocalId(1),
            },
            sem_rev: 0,
            may_refine: false,
            fields: fields.into(),
        }
    }

    #[test]
    fn words_and_text_render_in_field_order() {
        let r = render(
            &ir(vec![
                Field::new(
                    FieldKind::NAME,
                    Content::Text("OK".into()),
                    Origin::default(),
                ),
                Field::new(
                    FieldKind::STATE,
                    Content::Word(M::StateChecked),
                    Origin::default(),
                ),
                Field::new(
                    FieldKind::VALUE,
                    Content::Text("".into()),
                    Origin::default(),
                ),
            ]),
            RenderOpts::default(),
        );
        assert_eq!(r.len(), 2, "the empty value renders nothing");
        assert_eq!(r[0].text.as_str(), "OK");
        assert_eq!(r[0].kind, SegmentKind::Text);
        assert_eq!(r[1].text.as_str(), gorca_i18n::t(M::StateChecked));
        assert_eq!(r[1].kind, SegmentKind::State);
    }

    #[test]
    fn the_description_pause_option_changes_only_the_kind() {
        let i = ir(vec![Field::new(
            FieldKind::DESCRIPTION,
            Content::Text("help".into()),
            Origin::default(),
        )]);
        assert_eq!(render(&i, RenderOpts::default())[0].kind, SegmentKind::Text);
        assert_eq!(
            render(
                &i,
                RenderOpts {
                    description_pause: true
                }
            )[0]
            .kind,
            SegmentKind::Description
        );
    }
}
