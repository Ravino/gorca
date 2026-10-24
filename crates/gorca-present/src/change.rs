// SPDX-License-Identifier: GPL-3.0-or-later
//! `Reason::Change` (ADR-0022 section 2.7, ST-*): a short utterance that says only what changed on
//! the focused node. It needs two fused entries, the one that was spoken and the new one, so it is
//! a function of its own ([`compose_change`]); `compose` has no row for it.
//!
//! Wording follows the legacy change presenter: a tri-state says only the state it is in now
//! ("checked", "not checked", "half checked", "expanded", "collapsed"), never the flipped partner;
//! every other flag says its word when on and its negation when off.

use std::sync::Arc;

use gorca_i18n::MsgId as M;
use gorca_model::NodeKey;
use gorca_model::sem::{
    Avail, Check, Expand, Group, GroupMask, Invalid, Press, Select, SemEntry, SemFlags, SemRole,
};
use smallvec::SmallVec;

use crate::compose::ComposeCtx;
use crate::ir::{Arg, Content, Field, FieldKind, Origin, PresIr, Reason, RowId};
use crate::roles::{Class, native_role};

/// Rule row of the change utterance.
pub const ROW_CHANGE: RowId = RowId(3);

fn origin(group: Group) -> Origin {
    Origin {
        row: ROW_CHANGE,
        group: group.slot(),
        via: 0,
    }
}

fn word(id: M, group: Group) -> Field {
    Field::new(FieldKind::STATE, Content::word(id), origin(group))
}

fn not(id: M, group: Group, lang: gorca_i18n::Lang) -> Field {
    // "not <word>" through the catalog template, so every language words the negation itself.
    Field::new(
        FieldKind::STATE,
        Content::msg(
            M::StateNot,
            vec![Arg::text("word", gorca_i18n::t_in(lang, id))],
        ),
        origin(group),
    )
}

fn value_text(e: &SemEntry) -> Option<&Arc<str>> {
    e.sem
        .text
        .as_deref()
        .and_then(|t| t.value_text.as_ref())
        .filter(|s| !s.is_empty())
}

fn description(e: &SemEntry) -> Option<&Arc<str>> {
    e.sem
        .text
        .as_deref()
        .and_then(|t| t.description.as_ref())
        .filter(|s| !s.is_empty())
}

fn name_of(e: &SemEntry) -> Option<&Arc<str>> {
    e.name.as_ref().filter(|s| !s.is_empty())
}

/// The groups of the entry that differ in what a change utterance can say.
pub fn changed_groups(old: &SemEntry, new: &SemEntry, skip_selected: bool) -> GroupMask {
    let (o, n) = (&old.sem, &new.sem);
    let mut m = GroupMask::EMPTY;
    let mut set = |g: Group, differs: bool| {
        if differs {
            m = m.with(g);
        }
    };
    set(Group::Name, name_of(old) != name_of(new));
    set(Group::Role, o.role != n.role);
    set(Group::Check, o.check != n.check);
    set(Group::Press, o.press != n.press);
    set(Group::Expand, o.expand != n.expand);
    set(
        Group::Select,
        !skip_selected && (o.select == Select::On) != (n.select == Select::On),
    );
    set(
        Group::Avail,
        (o.avail == Avail::Disabled) != (n.avail == Avail::Disabled),
    );
    set(
        Group::Invalid,
        (o.invalid != Invalid::No) != (n.invalid != Invalid::No),
    );
    set(
        Group::Flags,
        o.flags.contains(SemFlags::REQUIRED) != n.flags.contains(SemFlags::REQUIRED),
    );
    set(Group::ValueText, value_text(old) != value_text(new));
    set(Group::Description, description(old) != description(new));
    m
}

/// Compose what changed between `old` (spoken) and `new`. An empty IR means nothing to say.
pub fn compose_change(old: &SemEntry, new: &SemEntry, node: NodeKey, ctx: ComposeCtx) -> PresIr {
    let changed = changed_groups(old, new, ctx.skip_selected);
    let sem = &new.sem;
    let info = native_role(sem.role, sem.press);
    let class = info.class;
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();

    if changed.has(Group::Name)
        && let Some(name) = name_of(new)
    {
        fields.push(Field::new(
            FieldKind::NAME,
            Content::Text(Arc::clone(name)),
            origin(Group::Name),
        ));
    }
    if changed.has(Group::Role)
        && let Some(w) = info.word
    {
        fields.push(Field::new(
            FieldKind::ROLE,
            Content::word(w),
            origin(Group::Role),
        ));
    }

    let mixed = sem.check == Check::Mixed || sem.press == Press::Mixed;
    let tri = match class {
        Class::Checkable => Some(M::StateHalfChecked),
        Class::Toggle => Some(M::StateHalfPressed),
        Class::Progress => Some(M::StateIndeterminate),
        Class::Switch | Class::Field | Class::Other => None,
    };
    // Checked / mixed: say the state it is in now.
    let check_changed =
        changed.has(Group::Check) || (class != Class::Toggle && changed.has(Group::Press));
    if check_changed
        && (mixed.then_some(tri).flatten().is_some() || !mixed)
        && class != Class::Toggle
    {
        let on = sem.check == Check::On || sem.press == Press::On;
        let w = if mixed {
            tri.unwrap_or(M::StateChecked)
        } else if class == Class::Switch || sem.role == SemRole::Switch {
            if on { M::StateOn } else { M::StateOff }
        } else if on {
            M::StateChecked
        } else {
            M::StateNotChecked
        };
        fields.push(word(w, Group::Check));
    }
    if changed.has(Group::Expand) {
        let w = match sem.expand {
            Expand::Expanded => M::StateExpanded,
            Expand::Collapsed => M::StateCollapsed,
            Expand::NotExpandable => M::StateNotExpanded,
        };
        fields.push(word(w, Group::Expand));
    }
    // The remaining flags in the legacy order: unavailable, pressed, selected, required, invalid.
    if changed.has(Group::Avail) {
        fields.push(word(
            if sem.avail == Avail::Disabled {
                M::StateUnavailable
            } else {
                M::StateAvailable
            },
            Group::Avail,
        ));
    }
    if class == Class::Toggle && !mixed && (changed.has(Group::Press) || changed.has(Group::Check))
    {
        let on = sem.press == Press::On || sem.check == Check::On;
        fields.push(word(
            if on {
                M::StatePressed
            } else {
                M::StateNotPressed
            },
            Group::Press,
        ));
    }
    if class == Class::Toggle && mixed && changed.has(Group::Press) {
        fields.push(word(M::StateHalfPressed, Group::Press));
    }
    if changed.has(Group::Select) {
        fields.push(if sem.select == Select::On {
            word(M::StateSelected, Group::Select)
        } else {
            word(M::StateNotSelected, Group::Select)
        });
    }
    if changed.has(Group::Flags) {
        fields.push(if sem.flags.contains(SemFlags::REQUIRED) {
            word(M::StateRequired, Group::Flags)
        } else {
            not(M::StateRequired, Group::Flags, ctx.lang)
        });
    }
    if changed.has(Group::Invalid) {
        fields.push(word(
            if sem.invalid == Invalid::No {
                M::StateValid
            } else {
                M::StateInvalidEntry
            },
            Group::Invalid,
        ));
    }
    if changed.has(Group::ValueText)
        && let Some(v) = value_text(new)
    {
        fields.push(Field::new(
            FieldKind::VALUE,
            Content::Text(Arc::clone(v)),
            origin(Group::ValueText),
        ));
    }
    if changed.has(Group::Description)
        && class == Class::Field
        && let Some(d) = description(new)
    {
        fields.push(Field::new(
            FieldKind::DESCRIPTION,
            Content::Text(Arc::clone(d)),
            origin(Group::Description),
        ));
    }

    let mut ir = PresIr::new(Reason::Change { changed }, node, new.sem_rev, fields);
    ir.may_refine = ctx.may_refine;
    ir
}

#[cfg(test)]
mod tests {
    use gorca_model::{AppId, ConnectorId, LocalId};

    use super::*;
    use crate::render::{RenderOpts, render};

    fn key() -> NodeKey {
        NodeKey {
            app: AppId {
                connector: ConnectorId(0),
                serial: 1,
            },
            local: LocalId(1),
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

    fn entry(role: SemRole) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.name = Some(Arc::from("Wi-Fi"));
        e
    }

    fn words(old: &SemEntry, new: &SemEntry) -> Vec<String> {
        render(&compose_change(old, new, key(), CTX), RenderOpts::default())
            .iter()
            .map(|t| t.text.as_str().to_owned())
            .collect()
    }

    #[test]
    fn a_check_box_flip_says_the_state_it_is_in_now() {
        let off = entry(SemRole::Checkbox);
        let mut on = off.clone();
        on.sem.check = Check::On;
        assert_eq!(words(&off, &on), ["checked"]);
        assert_eq!(words(&on, &off), ["not checked"]);
        let mut mixed = off.clone();
        mixed.sem.check = Check::Mixed;
        assert_eq!(words(&on, &mixed), ["half checked"]);
    }

    #[test]
    fn a_toggle_button_says_pressed_or_not_pressed_and_a_switch_on_or_off() {
        let off = entry(SemRole::Button);
        let mut off = off;
        off.sem.press = Press::Off;
        let mut on = off.clone();
        on.sem.press = Press::On;
        assert_eq!(words(&off, &on), ["pressed"]);
        assert_eq!(words(&on, &off), ["not pressed"]);
        let mut sw_off = entry(SemRole::Switch);
        sw_off.sem.check = Check::Off;
        let mut sw_on = sw_off.clone();
        sw_on.sem.check = Check::On;
        assert_eq!(words(&sw_off, &sw_on), ["on"]);
        assert_eq!(words(&sw_on, &sw_off), ["off"]);
    }

    #[test]
    fn expand_collapse_and_other_flags() {
        let mut a = entry(SemRole::TreeItem);
        a.sem.expand = Expand::Collapsed;
        let mut b = a.clone();
        b.sem.expand = Expand::Expanded;
        assert_eq!(words(&a, &b), ["expanded"]);
        assert_eq!(words(&b, &a), ["collapsed"]);
        let mut dis = a.clone();
        dis.sem.avail = Avail::Disabled;
        assert_eq!(words(&a, &dis), ["unavailable"]);
        assert_eq!(words(&dis, &a), [gorca_i18n::t(M::StateAvailable)]);
        let mut inv = a.clone();
        inv.sem.invalid = Invalid::True;
        assert_eq!(words(&a, &inv), ["invalid entry"]);
        assert_eq!(words(&inv, &a), [gorca_i18n::t(M::StateValid)]);
    }

    #[test]
    fn a_new_name_and_a_new_value_are_said_and_equal_entries_say_nothing() {
        let a = entry(SemRole::Slider);
        let mut b = a.clone();
        b.name = Some(Arc::from("Volume"));
        assert_eq!(words(&a, &b), ["Volume"]);
        assert!(words(&a, &a.clone()).is_empty());
        let mut c = a.clone();
        c.sem.text = Some(Arc::new(gorca_model::sem::SemText {
            value_text: Some(Arc::from("30")),
            ..gorca_model::sem::SemText::default()
        }));
        assert_eq!(words(&a, &c), ["30"]);
    }

    #[test]
    fn the_reason_carries_the_changed_groups() {
        let a = entry(SemRole::Checkbox);
        let mut b = a.clone();
        b.sem.check = Check::On;
        let ir = compose_change(&a, &b, key(), CTX);
        let Reason::Change { changed } = ir.reason else {
            unreachable!("a change reason");
        };
        assert!(changed.has(Group::Check) && !changed.has(Group::Name));
    }

    #[test]
    fn selection_is_not_news_in_a_switcher() {
        let a = entry(SemRole::ListItem);
        let mut b = a.clone();
        b.sem.select = Select::On;
        let skip = ComposeCtx {
            skip_selected: true,
            lang: gorca_i18n::Lang::En,
            ..CTX
        };
        assert!(compose_change(&a, &b, key(), skip).fields.is_empty());
        assert_eq!(words(&a, &b), ["selected"]);
    }
}
