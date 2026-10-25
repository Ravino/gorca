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

/// The role word of a node as review says it for a nameless object, "object" for a role without
/// a word.
pub fn role_word(snap: &NodeSnapshot, web: bool) -> &'static str {
    let entry = entry_of(snap);
    match gorca_present::role_word_of(&entry, web) {
        Some(w) => gorca_i18n::t(w),
        None => gorca_i18n::t(gorca_i18n::MsgId::WordObject),
    }
}
