// SPDX-License-Identifier: GPL-3.0-or-later
//! What flat review and object review say for a whole object, made by `gorca-present` from the
//! node's fused entry (ADR-0022 section 2.5, M22b3): `compose(Reason::Review)` -> `render`. The
//! entry is the one the application mirror carries (`NodeSnapshot::sem`), so the words are the
//! ones every other surface speaks from; review has no table of its own.

use std::sync::Arc;

use gorca_model::sem::{SemEntry, SemRole};
use gorca_model::{NodeSnapshot, SegmentKind};
use gorca_present::{
    ComposeCtx, IrEdit, PresIr, Reason, RenderOpts, ReportFlags, Token, TokenText, apply_edits,
    compose, render,
};
use smallvec::SmallVec;

use crate::present::entry_of;

fn ctx() -> ComposeCtx {
    ComposeCtx {
        flags: ReportFlags::default(),
        may_refine: false,
        web: false,
        skip_selected: false,
        hedge_inferred: false,
        lang: gorca_i18n::language(),
    }
}

fn opts() -> RenderOpts<'static> {
    static EMPTY: std::sync::OnceLock<gorca_i18n::ext::ExtCatalogs> = std::sync::OnceLock::new();
    RenderOpts {
        description_pause: false,
        lang: gorca_i18n::language(),
        ext: EMPTY.get_or_init(gorca_i18n::ext::ExtCatalogs::empty),
    }
}

/// The review IR of `entry`. `contents` is what a text field holds (text-layer data: the entry
/// has no value for it); a password's contents are never spoken.
fn review_ir(entry: &SemEntry, snap: &NodeSnapshot, contents: Option<&str>) -> PresIr {
    let mut ir = compose(entry, snap.key, Reason::Review, ctx()).unwrap_or_else(|_| {
        PresIr::new(
            Reason::Review,
            snap.key,
            entry.sem_rev,
            smallvec::SmallVec::new(),
        )
    });
    if let Some(t) = contents.filter(|t| !t.is_empty())
        && entry.sem.role != SemRole::PasswordBox
    {
        apply_edits(&mut ir, &[IrEdit::SetValue(Arc::from(t))]);
    }
    ir
}

/// The pieces the zone of a whole object says: the name, a role word for controls, the state
/// words and the value. Empty when the object has nothing to say.
pub fn object_tokens(snap: &NodeSnapshot, contents: Option<&str>) -> SmallVec<[Token; 6]> {
    let entry = entry_of(snap);
    render(&review_ir(&entry, snap, contents), opts())
}

/// The pieces the zone of an object says, and for an object that can be acted on or focused but
/// says nothing, its role word (or "object"). Empty for a silent object nobody can act on.
pub fn zone_tokens(
    snap: &NodeSnapshot,
    web: bool,
    contents: Option<&str>,
    actionable: bool,
) -> SmallVec<[Token; 6]> {
    let mut tokens = object_tokens(snap, contents);
    if tokens.is_empty() && actionable {
        tokens.push(Token {
            kind: SegmentKind::Role,
            text: TokenText::Static(role_word(snap, web)),
        });
    }
    tokens
}

/// The tokens as one line.
pub fn join_tokens(tokens: &[Token]) -> String {
    let mut out = String::new();
    for t in tokens {
        let s = t.text.as_str();
        if s.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(s);
    }
    out
}

/// The text of the zone of a whole object: [`object_tokens`] as one line.
pub fn object_text(snap: &NodeSnapshot, contents: Option<&str>) -> String {
    join_tokens(&object_tokens(snap, contents))
}

/// The zone text of a node as flat review reads it where no text-layer reading is at hand: the
/// contents of a field are the snapshot's value.
pub fn flat_text(snap: &NodeSnapshot) -> String {
    let contents = match &snap.value {
        Some(gorca_model::Value::Text(t)) => Some(&**t),
        Some(gorca_model::Value::Range { .. }) | None => None,
    };
    object_text(snap, contents)
}

/// The role word of a node as review says it for a nameless object, "object" for a role without
/// a word.
pub fn role_word(snap: &NodeSnapshot, web: bool) -> &'static str {
    let entry = entry_of(snap);
    match gorca_present::role_word_of(&entry, web) {
        Some(w) => gorca_i18n::t(w),
        None => gorca_i18n::t(gorca_i18n::MsgId::WordObject),
    }
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::Semantics;
    use gorca_model::{
        AppId, ChildList, ConnectorId, Freshness, Ifaces, LocalId, NodeKey, NodeSem, PropMask,
        Role, StateSet,
    };

    use super::*;

    /// A snapshot that carries the fused entry of a node with `role` and `name`.
    fn snap(role: SemRole, name: &str) -> NodeSnapshot {
        let entry = SemEntry {
            sem: Semantics {
                role,
                ..Semantics::default()
            },
            name: (!name.is_empty()).then(|| Arc::from(name)),
            ..SemEntry::default()
        };
        NodeSnapshot {
            key: NodeKey {
                app: AppId {
                    connector: ConnectorId(1),
                    serial: 1,
                },
                local: LocalId(1),
            },
            role: Role::Unknown,
            states: StateSet::empty(),
            name: None,
            description: None,
            value: None,
            parent: None,
            children: ChildList::Unknown,
            interfaces: Ifaces::empty(),
            attrs: gorca_model::AttrMap::default(),
            relations: gorca_model::Relations::default(),
            bounds: None,
            have: PropMask::PRESENT,
            confirmed_seq: 0,
            fresh: Freshness::Defunct,
            sem: NodeSem {
                rec: None,
                entry: Some(Arc::new(entry)),
            },
        }
    }

    #[test]
    fn the_words_come_from_the_entry_not_from_the_snapshot_fields() {
        // The legacy fields say nothing (Role::Unknown, no name); the entry is a named button.
        let s = snap(SemRole::Button, "Apply");
        assert_eq!(object_text(&s, None), "Apply button");
    }

    #[test]
    fn the_contents_of_a_field_are_its_value_and_a_password_is_never_read() {
        let field = snap(SemRole::TextBox, "Name");
        assert_eq!(object_text(&field, Some("Ada")), "Name edit Ada");
        assert_eq!(object_text(&field, Some("")), "Name edit");
        let pw = snap(SemRole::PasswordBox, "Password");
        assert_eq!(object_text(&pw, Some("hunter2")), "Password password edit");
    }

    #[test]
    fn a_silent_object_that_can_be_acted_on_says_its_role_or_object() {
        // A nameless button still says what it is.
        let b = snap(SemRole::Button, "");
        assert_eq!(join_tokens(&zone_tokens(&b, false, None, false)), "button");
        // A row has no word in a line of text: silent, unless it can be acted on.
        let row = snap(SemRole::Row, "");
        assert!(zone_tokens(&row, false, None, false).is_empty());
        let t = zone_tokens(&row, false, None, true);
        assert_eq!(join_tokens(&t), "row");
        assert_eq!(t[0].kind, SegmentKind::Role);
        // A role with no word at all is called object.
        let log = snap(SemRole::Log, "");
        assert!(zone_tokens(&log, false, None, false).is_empty());
        assert_eq!(join_tokens(&zone_tokens(&log, false, None, true)), "object");
    }

    #[test]
    fn a_nameless_text_run_is_called_text_on_the_web_and_label_in_an_application() {
        let s = snap(SemRole::StaticText, "");
        assert_eq!(role_word(&s, true), "text");
        assert_eq!(role_word(&s, false), "label");
        let em = snap(SemRole::Emphasis, "");
        assert_eq!(role_word(&em, true), "text");
        assert_eq!(role_word(&em, false), "object");
    }

    #[test]
    fn the_language_is_the_catalog_language_of_the_thread() {
        let s = snap(SemRole::Button, "OK");
        let _ru = gorca_i18n::scoped(gorca_i18n::Lang::Ru);
        assert_eq!(
            object_text(&s, None),
            format!(
                "OK {}",
                gorca_i18n::t_in(gorca_i18n::Lang::Ru, gorca_i18n::MsgId::RoleButton)
            )
        );
    }
}
