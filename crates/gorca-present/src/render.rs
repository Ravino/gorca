// SPDX-License-Identifier: GPL-3.0-or-later
//! `render`: `PresIr` to tokens. The only producer of text in the pipeline.
//!
//! * `Content::Text` is literal text; message-reference marker characters are removed so a
//!   reference can never be spoken raw (P-2).
//! * `Content::Msg` goes through the core catalog (with arguments), `Content::Ext` through the
//!   extension catalog; a missing extension message renders nothing.
//! * `Content::Opaque` (a newer host's kind) renders nothing and is otherwise left alone.

use std::sync::Arc;

use gorca_i18n::{Arg as I18nArg, ext};
use gorca_model::SegmentKind;
use smallvec::SmallVec;

use crate::ir::{Arg, ArgVal, Content, PresIr};

/// Token text: a catalog string, shared literal text or a formatted message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TokenText {
    Static(&'static str),
    Shared(Arc<str>),
    Owned(Box<str>),
}

impl TokenText {
    pub fn as_str(&self) -> &str {
        match self {
            TokenText::Static(s) => s,
            TokenText::Shared(s) => s,
            TokenText::Owned(s) => s,
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

fn pairs(args: &[Arg]) -> SmallVec<[(&str, I18nArg<'_>); 4]> {
    args.iter()
        .take(ext::MAX_REF_ARGS)
        .map(|a| {
            let v = match &a.val {
                ArgVal::Num(n) => I18nArg::Num(*n),
                ArgVal::Text(t) => I18nArg::Str(t),
            };
            (&*a.name, v)
        })
        .collect()
}

fn text_of(c: &Content) -> Option<TokenText> {
    match c {
        Content::Text(s) => Some(match ext::sanitize(s) {
            std::borrow::Cow::Borrowed(_) => TokenText::Shared(Arc::clone(s)),
            std::borrow::Cow::Owned(o) => TokenText::Owned(o.into()),
        }),
        Content::Msg { id, args } if args.is_empty() => Some(TokenText::Static(gorca_i18n::t(*id))),
        Content::Msg { id, args } => {
            Some(TokenText::Owned(gorca_i18n::tf(*id, &pairs(args)).into()))
        }
        Content::Ext(r) => ext::format(&r.ns, &r.key, gorca_i18n::language(), &pairs(&r.args))
            .map(|s| TokenText::Owned(s.into())),
        Content::Opaque { .. } => None,
    }
}

/// Render `ir`; empty content renders nothing.
pub fn render(ir: &PresIr, opts: RenderOpts) -> SmallVec<[Token; 6]> {
    let mut out = SmallVec::new();
    for f in &ir.fields {
        let Some(text) = text_of(&f.content) else {
            continue;
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
        PresIr::new(
            Reason::Focus,
            NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                local: LocalId(1),
            },
            0,
            fields.into(),
        )
    }

    fn f(kind: FieldKind, c: Content) -> Field {
        Field::new(kind, c, Origin::default())
    }

    #[test]
    fn words_and_text_render_in_field_order() {
        let r = render(
            &ir(vec![
                f(FieldKind::NAME, Content::Text("OK".into())),
                f(FieldKind::STATE, Content::word(M::StateChecked)),
                f(FieldKind::VALUE, Content::Text("".into())),
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
    fn a_message_with_numeric_arguments_is_formatted_by_the_catalog() {
        let r = render(
            &ir(vec![f(
                FieldKind::LEVEL,
                Content::msg(M::StateLevel, vec![Arg::num("l", 3)]),
            )]),
            RenderOpts::default(),
        );
        assert_eq!(
            r[0].text.as_str(),
            gorca_i18n::tf(M::StateLevel, &[("l", I18nArg::Num(3))])
        );
        assert!(r[0].text.as_str().contains('3'));
    }

    #[test]
    fn a_reference_in_literal_text_is_never_spoken_raw() {
        let raw = format!("{}ns:key{}a=b", '\u{1e}', '\u{1f}');
        let r = render(
            &ir(vec![f(FieldKind::NAME, Content::Text(raw.as_str().into()))]),
            RenderOpts::default(),
        );
        assert!(!r[0].text.as_str().contains(['\u{1e}', '\u{1f}']));
    }

    #[test]
    fn an_add_on_message_resolves_through_its_catalog_and_a_missing_one_renders_nothing() {
        let errors = ext::register("m22b1-test", gorca_i18n::Lang::En, "unread = {n} unread\n");
        assert!(errors.is_empty(), "{errors:?}");
        let r = render(
            &ir(vec![
                f(
                    FieldKind(0x8000),
                    Content::ext("m22b1-test", "unread", vec![Arg::num("n", 4)]),
                ),
                f(
                    FieldKind(0x8001),
                    Content::ext("m22b1-test", "missing", vec![]),
                ),
                f(
                    FieldKind(0x8002),
                    Content::ext("no-such-ns", "unread", vec![]),
                ),
            ]),
            RenderOpts::default(),
        );
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text.as_str(), "4 unread");
        assert_eq!(r[0].kind, SegmentKind::Text, "an add-on kind is plain text");
    }

    #[test]
    fn opaque_content_renders_nothing() {
        let r = render(
            &ir(vec![f(FieldKind::NAME, Content::Opaque { code: 9 })]),
            RenderOpts::default(),
        );
        assert!(r.is_empty());
    }

    #[test]
    fn the_description_pause_option_changes_only_the_kind() {
        let i = ir(vec![f(
            FieldKind::DESCRIPTION,
            Content::Text("help".into()),
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
