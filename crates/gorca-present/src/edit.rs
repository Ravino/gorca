// SPDX-License-Identifier: GPL-3.0-or-later
//! IR edits (ADR-0022 section 2.9, ADR-0026 `present-edit`): the policy, lead-in and mode-word
//! changes of a focus utterance as operations on the `PresIr` fields, applied in one fixed order
//! before dedupe and render. Nothing here touches rendered text.
//!
//! Producers of text that are not part of the fused model yet (the window title and text a focus
//! leads with, the browse mode word, a line behind a web focus) hand over `(SegmentKind, text)`
//! tokens; [`fields_from_tokens`] turns them into fields (the "legacy adapter" of the `present`
//! hook, section 2.9) with the producer recorded in `Origin::via`. When such a producer is moved
//! to `compose`, its adapter call goes away and the edit stays.
//!
//! Order of application (fixed, so a result never depends on the order the producers ran in):
//! `Silence`, `ReplaceAll`, `SetValue`, `DropKind`, `Prepend`, `Append`.

use std::sync::Arc;

use gorca_model::SegmentKind;

use crate::ir::{Content, Field, FieldKind, Origin, PresIr};

/// One edit of a focus utterance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum IrEdit {
    /// Say nothing for this node (the application policy's verdict).
    Silence,
    /// Replace every field (the policy's own words).
    ReplaceAll(Vec<Field>),
    /// Replace the first `VALUE` field with `text`, or append one (the policy's value).
    SetValue(Arc<str>),
    /// Remove every field of the kind (a web "unavailable" on static content, a description).
    DropKind(FieldKind),
    /// Insert fields in front of the utterance (a window title, the mode word), in order.
    Prepend(Vec<Field>),
    /// Add fields behind the utterance (the browse line behind a web focus), in order.
    Append(Vec<Field>),
}

/// The field kind a legacy token becomes.
pub const fn kind_of_token(kind: SegmentKind) -> FieldKind {
    match kind {
        SegmentKind::Role => FieldKind::ROLE,
        SegmentKind::State => FieldKind::STATE,
        SegmentKind::Value => FieldKind::VALUE,
        SegmentKind::Description => FieldKind::DESCRIPTION,
        SegmentKind::Text | SegmentKind::Said | SegmentKind::Capital => FieldKind::TEXT,
    }
}

/// Fields for tokens produced outside `compose`; `via` names the producer (non-zero).
pub fn fields_from_tokens<'a>(
    tokens: impl IntoIterator<Item = (SegmentKind, &'a str)>,
    via: u32,
) -> Vec<Field> {
    tokens
        .into_iter()
        .map(|(kind, text)| {
            Field::new(
                kind_of_token(kind),
                Content::Text(Arc::from(text)),
                Origin {
                    via,
                    group: Origin::NO_GROUP,
                    ..Origin::default()
                },
            )
        })
        .collect()
}

/// Apply `edits` to `ir` in the fixed order. Returns `false` when the result is silence (no field
/// left to say because of a `Silence`). An IR of an unknown reason is left as it is: a newer host's
/// utterance is not edited by an older rule (ir.rs, "unknown kinds pass through").
pub fn apply(ir: &mut PresIr, edits: &[IrEdit]) -> bool {
    if !ir.reason.editable() {
        return true;
    }
    if edits.iter().any(|e| matches!(e, IrEdit::Silence)) {
        ir.fields.clear();
        return false;
    }
    for e in edits {
        if let IrEdit::ReplaceAll(fields) = e {
            ir.fields = fields.iter().cloned().collect();
        }
    }
    for e in edits {
        if let IrEdit::SetValue(text) = e {
            let f = Field::new(
                FieldKind::VALUE,
                Content::Text(Arc::clone(text)),
                Origin::default(),
            );
            match ir.fields.iter_mut().find(|x| x.kind == FieldKind::VALUE) {
                Some(slot) => *slot = f,
                None => ir.fields.push(f),
            }
        }
    }
    for e in edits {
        if let IrEdit::DropKind(k) = e {
            ir.fields.retain(|f| f.kind != *k);
        }
    }
    let mut at = 0;
    for e in edits {
        if let IrEdit::Prepend(fields) = e {
            for f in fields {
                ir.fields.insert(at, f.clone());
                at += 1;
            }
        }
    }
    for e in edits {
        if let IrEdit::Append(fields) = e {
            ir.fields.extend(fields.iter().cloned());
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};

    use super::*;
    use crate::ir::Reason;
    use crate::render::{RenderOpts, render};

    fn ir(parts: &[(FieldKind, &str)]) -> PresIr {
        let fields = parts
            .iter()
            .map(|(k, t)| Field::new(*k, Content::Text(Arc::from(*t)), Origin::default()))
            .collect();
        PresIr::new(
            Reason::Focus,
            NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 1,
                },
                local: LocalId(1),
            },
            1,
            fields,
        )
    }

    fn words(ir: &PresIr) -> Vec<String> {
        render(ir, RenderOpts::default())
            .iter()
            .map(|t| t.text.as_str().to_owned())
            .collect()
    }

    #[test]
    fn silence_wins_over_every_other_edit() {
        let mut i = ir(&[(FieldKind::NAME, "OK"), (FieldKind::ROLE, "button")]);
        let said = apply(
            &mut i,
            &[
                IrEdit::Prepend(fields_from_tokens([(SegmentKind::Text, "Browse")], 7)),
                IrEdit::Silence,
            ],
        );
        assert!(!said && i.fields.is_empty());
    }

    #[test]
    fn set_value_replaces_or_appends() {
        let mut a = ir(&[(FieldKind::NAME, "Vol"), (FieldKind::VALUE, "30")]);
        apply(&mut a, &[IrEdit::SetValue(Arc::from("30 percent"))]);
        assert_eq!(words(&a), ["Vol", "30 percent"]);
        let mut b = ir(&[(FieldKind::NAME, "Vol")]);
        apply(&mut b, &[IrEdit::SetValue(Arc::from("30 percent"))]);
        assert_eq!(words(&b), ["Vol", "30 percent"]);
    }

    #[test]
    fn replace_all_then_the_rest_in_the_fixed_order() {
        let mut i = ir(&[(FieldKind::NAME, "x")]);
        let own = fields_from_tokens([(SegmentKind::Text, "Custom")], 3);
        apply(
            &mut i,
            &[
                IrEdit::Append(fields_from_tokens([(SegmentKind::Text, "tail")], 4)),
                IrEdit::Prepend(fields_from_tokens([(SegmentKind::Text, "lead")], 5)),
                IrEdit::ReplaceAll(own),
            ],
        );
        assert_eq!(
            words(&i),
            ["lead", "Custom", "tail"],
            "order of the list does not matter"
        );
    }

    #[test]
    fn drop_kind_and_prepend_keep_the_producer_in_origin() {
        let mut i = ir(&[
            (FieldKind::NAME, "Row"),
            (FieldKind::STATE, "unavailable"),
            (FieldKind::ROLE, "row"),
        ]);
        apply(
            &mut i,
            &[
                IrEdit::DropKind(FieldKind::STATE),
                IrEdit::Prepend(fields_from_tokens(
                    [(SegmentKind::Text, "Files"), (SegmentKind::Role, "window")],
                    9,
                )),
            ],
        );
        assert_eq!(words(&i), ["Files", "window", "Row", "row"]);
        assert_eq!(i.fields[0].origin.via, 9);
        assert_eq!(i.fields[0].kind, FieldKind::TEXT);
        assert_eq!(i.fields[1].kind, FieldKind::ROLE);
    }

    #[test]
    fn an_unknown_reason_is_not_edited() {
        let mut i = ir(&[(FieldKind::NAME, "x")]);
        i.reason = Reason::Unknown(77);
        assert!(apply(&mut i, &[IrEdit::Silence]));
        assert_eq!(i.fields.len(), 1);
    }

    #[test]
    fn tokens_map_to_kinds() {
        assert_eq!(kind_of_token(SegmentKind::Said), FieldKind::TEXT);
        assert_eq!(
            kind_of_token(SegmentKind::Description),
            FieldKind::DESCRIPTION
        );
        assert_eq!(kind_of_token(SegmentKind::Value), FieldKind::VALUE);
    }
}
