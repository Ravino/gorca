// SPDX-License-Identifier: GPL-3.0-or-later
//! The desktop rows of `compose` (ADR-0022 section 8, M22b4): the title an activated window or
//! dialog leads with (`Reason::Window`) and the words of a notification or alert that appeared
//! (`Reason::Notice`). Both read the node's fused entry only; the static text inside a notice or
//! dialog is content of other nodes and arrives as lines.

use std::sync::Arc;

use gorca_model::NodeKey;
use gorca_model::sem::{Group, Press, SemEntry, SemRole};
use smallvec::SmallVec;

use crate::compose::{ComposeCtx, name_field, name_is_blank};
use crate::ir::{Content, Field, FieldKind, Origin, PresIr, Reason, RowId};
use crate::roles::native_role;

/// Rule row of the window lead-in.
pub const ROW_WINDOW: RowId = RowId(6);
/// Rule row of a notice.
pub const ROW_NOTICE: RowId = RowId(7);

fn origin(row: RowId, group: Group) -> Origin {
    Origin {
        row,
        group: group.slot(),
        via: 0,
    }
}

fn role_field(entry: &SemEntry, row: RowId) -> Option<Field> {
    let word = native_role(entry.sem.role, Press::NotToggle).word?;
    Some(Field::new(
        FieldKind::ROLE,
        Content::word(word),
        origin(row, Group::Role),
    ))
}

/// The title of a window, and the role word for a dialog or an alert (a plain window is its title:
/// "window" after every application name is noise).
pub fn compose_window(entry: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    if let Some(f) = name_field(entry, ctx, ROW_WINDOW).field {
        fields.push(f);
    }
    let says_role = matches!(
        entry.sem.role,
        SemRole::Dialog | SemRole::AlertDialog | SemRole::Alert
    );
    if says_role && let Some(f) = role_field(entry, ROW_WINDOW) {
        fields.push(f);
    }
    PresIr::new(Reason::Window, node, entry.sem_rev, fields)
}

/// What a notice says: the role word, the name, the description, then the static text inside
/// (`lines`). A piece equal to an earlier piece is dropped (a banner's name is often its title
/// label).
pub fn compose_notice(
    entry: &SemEntry,
    node: NodeKey,
    ctx: ComposeCtx,
    lines: &[Arc<str>],
) -> PresIr {
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    if let Some(f) = role_field(entry, ROW_NOTICE) {
        fields.push(f);
    }
    let mut said: SmallVec<[Arc<str>; 6]> = SmallVec::new();
    let mut add =
        |fields: &mut SmallVec<[Field; 6]>, kind: FieldKind, text: &Arc<str>, g: Group| {
            let t = text.trim();
            if name_is_blank(Some(t)) || said.iter().any(|s| &**s == t) {
                return;
            }
            let shared: Arc<str> = if t.len() == text.len() {
                Arc::clone(text)
            } else {
                Arc::from(t)
            };
            said.push(Arc::clone(&shared));
            fields.push(Field::new(
                kind,
                Content::Text(shared),
                origin(ROW_NOTICE, g),
            ));
        };
    if let Some(f) = name_field(entry, ctx, ROW_NOTICE).field
        && let Content::Text(t) = &f.content
    {
        add(&mut fields, f.kind, &Arc::clone(t), Group::Name);
    }
    if let Some(d) = entry
        .sem
        .text
        .as_deref()
        .and_then(|t| t.description.as_ref())
    {
        add(&mut fields, FieldKind::TEXT, d, Group::Description);
    }
    for l in lines {
        add(&mut fields, FieldKind::TEXT, l, Group::Name);
    }
    PresIr::new(Reason::Notice, node, entry.sem_rev, fields)
}

/// Whether a notice has anything to say beyond its role word.
pub fn notice_has_words(ir: &PresIr) -> bool {
    ir.fields.iter().any(|f| f.kind != FieldKind::ROLE)
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::SemText;
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;
    use crate::render::{RenderOpts, render};

    fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(3),
        }
    }

    const CTX: ComposeCtx = ComposeCtx {
        flags: crate::compose::ReportFlags(0),
        may_refine: false,
        web: false,
        skip_selected: false,
        hedge_inferred: false,
        lang: gorca_i18n::Lang::En,
    };

    fn entry(role: SemRole, name: Option<&str>, desc: Option<&str>) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.name = name.map(Arc::from);
        if let Some(d) = desc {
            e.sem.text = Some(Arc::new(SemText {
                description: Some(Arc::from(d)),
                ..SemText::default()
            }));
        }
        e
    }

    fn words(ir: &PresIr) -> Vec<String> {
        render(ir, RenderOpts::default())
            .iter()
            .map(|t| t.text.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_plain_window_is_its_title_and_a_dialog_adds_its_role() {
        let w = compose_window(&entry(SemRole::Window, Some("Files"), None), key(), CTX);
        assert_eq!(words(&w), ["Files"]);
        let d = compose_window(&entry(SemRole::Dialog, Some("Save"), None), key(), CTX);
        assert_eq!(words(&d), ["Save", "dialog"]);
        let a = compose_window(
            &entry(SemRole::AlertDialog, Some("Sure?"), None),
            key(),
            CTX,
        );
        assert_eq!(words(&a), ["Sure?", "dialog"]);
        let n = compose_window(&entry(SemRole::Alert, None, None), key(), CTX);
        assert_eq!(words(&n), ["alert"]);
        assert_eq!(w.reason, Reason::Window);
    }

    #[test]
    fn a_notice_is_role_name_description_and_text_without_repeats() {
        let e = entry(SemRole::Alert, Some("Mail"), Some("New message"));
        let lines: Vec<Arc<str>> =
            vec![Arc::from("Mail"), Arc::from(" Hello "), Arc::from("Hello")];
        let ir = compose_notice(&e, key(), CTX, &lines);
        assert_eq!(words(&ir), ["alert", "Mail", "New message", "Hello"]);
        assert!(notice_has_words(&ir));
        let bare = compose_notice(&entry(SemRole::Alert, None, None), key(), CTX, &[]);
        assert!(!notice_has_words(&bare));
        assert_eq!(bare.reason, Reason::Notice);
    }
}
