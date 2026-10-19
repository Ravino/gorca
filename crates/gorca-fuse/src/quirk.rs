// SPDX-License-Identifier: GPL-3.0-or-later
//! The S-1 quirk engine and the loaders of the per-source-profile data tables (M22a-B5,
//! ADR-0024 sections 2.2 and 2.13).
//!
//! Everything an engine-specific table says is **data** (TOML), parsed and validated here:
//!
//! * [`QuirkSet`]: rewrite rules of stratum S-1. The action set is **closed** ([`QuirkAction`]);
//!   an unknown action, key, group, role or source is a load error, never ignored. Rule ids are
//!   non-zero and unique; they are the `hit` ids that `explain` reports.
//! * [`parse_pi`]: the trust table `PI` (per-group primary priority).
//! * [`parse_inexpressible`]: the static `inexpressible` row (each row names its decision, D-4).
//! * [`ProfileSelector`]: source-profile selection from app facts.
//!
//! Size caps apply to every document and every list; no input can make a table grow without bound.

use std::sync::Arc;

use gorca_model::sem::{GROUP_COUNT, Group, GroupMask, SemRole};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};

use crate::profile::{Profile, RoleSet};
use crate::register::{MAX_PRIMARIES, PrimarySlot};
use crate::repair::ix;
use crate::strata::{Quirks, Record};
use crate::value::{Slot, Val};

/// Largest accepted table document.
pub const MAX_DOC_BYTES: usize = 64 * 1024;
/// Most rules in one [`QuirkSet`].
pub const MAX_RULES: usize = 256;
/// Most prefixes in one `strip-name-prefix` rule.
pub const MAX_PREFIXES: usize = 32;
/// Longest prefix, in bytes.
pub const MAX_PREFIX_BYTES: usize = 16;
/// Longest rule name, pattern or reason, in bytes.
pub const MAX_LABEL_BYTES: usize = 128;
/// Most rows in a selector or `inexpressible` document, and most patterns in one selector row.
pub const MAX_ROWS: usize = 64;

/// A load error. Messages name the 1-based row so a table author can find it.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum QuirkError {
    #[error("table is {bytes} bytes, the limit is {max}")]
    TooLarge { bytes: usize, max: usize },
    #[error("table does not parse: {0}")]
    Syntax(String),
    #[error("more than {max} {what}")]
    TooMany { what: &'static str, max: usize },
    #[error("row {row}: missing or mistyped `{field}`")]
    Field { row: usize, field: &'static str },
    #[error("row {row}: unknown key `{key}`")]
    UnknownKey { row: usize, key: String },
    #[error("row {row}: unknown action `{action}`")]
    UnknownAction { row: usize, action: String },
    #[error("row {row}: unknown group `{name}`")]
    UnknownGroup { row: usize, name: String },
    #[error("row {row}: unknown role `{name}`")]
    UnknownRole { row: usize, name: String },
    #[error("row {row}: unknown primary source `{name}` (expected p0 or p1)")]
    UnknownSource { row: usize, name: String },
    #[error("row {row}: {why}")]
    Invalid { row: usize, why: &'static str },
    #[error("rule id {0} is used twice")]
    DuplicateId(u16),
}

type Result<T> = std::result::Result<T, QuirkError>;

// ------------------------------------------------------------------------------------------------
// TOML helpers
// ------------------------------------------------------------------------------------------------

fn parse_doc(src: &str) -> Result<DocumentMut> {
    if src.len() > MAX_DOC_BYTES {
        return Err(QuirkError::TooLarge {
            bytes: src.len(),
            max: MAX_DOC_BYTES,
        });
    }
    src.parse::<DocumentMut>()
        .map_err(|e| QuirkError::Syntax(e.to_string()))
}

/// The `[[name]]` tables of a document; absent is empty. Any other top-level key is rejected.
fn rows<'a>(doc: &'a DocumentMut, name: &'static str) -> Result<Vec<&'a Table>> {
    for (k, _) in doc.iter() {
        if k != name {
            return Err(QuirkError::UnknownKey {
                row: 0,
                key: k.to_owned(),
            });
        }
    }
    let Some(item) = doc.get(name) else {
        return Ok(Vec::new());
    };
    let arr = item.as_array_of_tables().ok_or(QuirkError::Field {
        row: 0,
        field: name,
    })?;
    if arr.len() > MAX_ROWS.max(MAX_RULES) {
        return Err(QuirkError::TooMany {
            what: "rows",
            max: MAX_ROWS.max(MAX_RULES),
        });
    }
    Ok(arr.iter().collect())
}

fn check_keys(t: &Table, row: usize, allowed: &[&str]) -> Result<()> {
    for (k, _) in t {
        if !allowed.contains(&k) {
            return Err(QuirkError::UnknownKey {
                row,
                key: k.to_owned(),
            });
        }
    }
    Ok(())
}

fn string<'a>(t: &'a Table, row: usize, field: &'static str) -> Result<&'a str> {
    t.get(field)
        .and_then(Item::as_str)
        .filter(|s| !s.is_empty() && s.len() <= MAX_LABEL_BYTES)
        .ok_or(QuirkError::Field { row, field })
}

fn opt_string<'a>(t: &'a Table, row: usize, field: &'static str) -> Result<Option<&'a str>> {
    match t.get(field) {
        None => Ok(None),
        Some(_) => string(t, row, field).map(Some),
    }
}

fn string_list<'a>(
    t: &'a Table,
    row: usize,
    field: &'static str,
    max_items: usize,
    max_bytes: usize,
) -> Result<Vec<&'a str>> {
    match t.get(field) {
        None => Ok(Vec::new()),
        Some(item) => list_items(item, row, field, max_items, max_bytes),
    }
}

fn list_items<'a>(
    item: &'a Item,
    row: usize,
    field: &'static str,
    max_items: usize,
    max_bytes: usize,
) -> Result<Vec<&'a str>> {
    let arr = item.as_array().ok_or(QuirkError::Field { row, field })?;
    if arr.len() > max_items {
        return Err(QuirkError::TooMany {
            what: "list items",
            max: max_items,
        });
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= max_bytes)
                .ok_or(QuirkError::Field { row, field })
        })
        .collect()
}

fn source(name: &str, row: usize) -> Result<PrimarySlot> {
    let unknown = || QuirkError::UnknownSource {
        row,
        name: name.to_owned(),
    };
    let i = match name {
        "p0" => 0,
        "p1" => 1,
        _ => return Err(unknown()),
    };
    PrimarySlot::new(i).ok_or_else(unknown)
}

fn group(name: &str, row: usize) -> Result<Group> {
    Group::from_name(name).ok_or_else(|| QuirkError::UnknownGroup {
        row,
        name: name.to_owned(),
    })
}

fn role(name: &str, row: usize) -> Result<SemRole> {
    SemRole::from_name(name).ok_or_else(|| QuirkError::UnknownRole {
        row,
        name: name.to_owned(),
    })
}

// ------------------------------------------------------------------------------------------------
// Quirk rules
// ------------------------------------------------------------------------------------------------

/// What a rule does. Closed: a new action is a code change with a review, never a table edit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum QuirkAction {
    /// Remove the first matching prefix from the `name` value, unless nothing would remain.
    /// (Gecko list markers.)
    StripNamePrefix(Box<[Box<str>]>),
    /// Make a group `Unknown` when the source reported a value: the value is known to be wrong
    /// for the engine, so a lower layer (S3) may fill in. (Gecko per-role menu positions.)
    UnknownGroup(Group),
    /// Rewrite one role to another.
    RoleRemap { from: SemRole, to: SemRole },
}

/// One validated rule.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuirkRule {
    pub id: u16,
    pub name: Arc<str>,
    pub src: PrimarySlot,
    /// Empty set = any role (a record without a role matches only then).
    pub roles: RoleSet,
    pub has_roles: bool,
    pub action: QuirkAction,
}

/// An ordered rule list for one source profile. Rules apply in file order and each sees the
/// previous rule's output, so the result is a pure function of the record.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct QuirkSet {
    rules: Vec<QuirkRule>,
}

impl QuirkSet {
    /// Parse and validate one `[[rule]]` document.
    pub fn parse(src: &str) -> Result<QuirkSet> {
        let doc = parse_doc(src)?;
        let mut set = QuirkSet::default();
        for (i, t) in rows(&doc, "rule")?.into_iter().enumerate() {
            set.push(parse_rule(t, i + 1)?)?;
        }
        Ok(set)
    }

    /// Append the rules of `other` (a second document of the same profile); ids must stay unique.
    pub fn extend(&mut self, other: QuirkSet) -> Result<()> {
        for r in other.rules {
            self.push(r)?;
        }
        Ok(())
    }

    fn push(&mut self, r: QuirkRule) -> Result<()> {
        if self.rules.len() >= MAX_RULES {
            return Err(QuirkError::TooMany {
                what: "rules",
                max: MAX_RULES,
            });
        }
        if self.rules.iter().any(|x| x.id == r.id) {
            return Err(QuirkError::DuplicateId(r.id));
        }
        self.rules.push(r);
        Ok(())
    }

    /// Reject a rule that makes a group `Unknown` when the profile lists that group as
    /// statically inexpressible: Unknown would let a Secondary in through the back door (H-3).
    pub fn validate_against(&self, inexpressible: GroupMask) -> Result<()> {
        for (i, r) in self.rules.iter().enumerate() {
            if let QuirkAction::UnknownGroup(g) = &r.action
                && inexpressible.has(*g)
            {
                return Err(QuirkError::Invalid {
                    row: i + 1,
                    why: "unknown-group on a group the profile lists as inexpressible",
                });
            }
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.rules.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    pub fn rules(&self) -> &[QuirkRule] {
        &self.rules
    }

    /// The name of the rule behind a `hit` id of `explain` (None for 0 or an unknown id).
    pub fn rule_name(&self, id: u16) -> Option<&str> {
        self.rules
            .iter()
            .find(|r| r.id == id && id != 0)
            .map(|r| &*r.name)
    }
}

fn parse_rule(t: &Table, row: usize) -> Result<QuirkRule> {
    let action_name = string(t, row, "action")?;
    let allowed: &[&str] = match action_name {
        "strip-name-prefix" => &["id", "name", "source", "roles", "action", "prefixes"],
        "unknown-group" => &["id", "name", "source", "roles", "action", "group"],
        "role-remap" => &["id", "name", "source", "roles", "action", "from", "to"],
        other => {
            return Err(QuirkError::UnknownAction {
                row,
                action: other.to_owned(),
            });
        }
    };
    check_keys(t, row, allowed)?;

    let id = t
        .get("id")
        .and_then(Item::as_integer)
        .and_then(|v| u16::try_from(v).ok())
        .filter(|v| *v != 0)
        .ok_or(QuirkError::Field { row, field: "id" })?;
    let name: Arc<str> = Arc::from(string(t, row, "name")?);
    let src = match opt_string(t, row, "source")? {
        Some(s) => source(s, row)?,
        None => PrimarySlot::P0,
    };
    let role_names = string_list(t, row, "roles", MAX_ROWS, MAX_LABEL_BYTES)?;
    let mut roles = RoleSet::EMPTY;
    for n in &role_names {
        roles = roles.with(role(n, row)?);
    }

    let action = match action_name {
        "strip-name-prefix" => {
            let prefixes = string_list(t, row, "prefixes", MAX_PREFIXES, MAX_PREFIX_BYTES)?;
            if prefixes.is_empty() {
                return Err(QuirkError::Field {
                    row,
                    field: "prefixes",
                });
            }
            QuirkAction::StripNamePrefix(prefixes.into_iter().map(Box::from).collect())
        }
        "unknown-group" => {
            let g = group(string(t, row, "group")?, row)?;
            if g == Group::Role {
                return Err(QuirkError::Invalid {
                    row,
                    why: "the role group is never made unknown by a quirk",
                });
            }
            QuirkAction::UnknownGroup(g)
        }
        _ => QuirkAction::RoleRemap {
            from: role(string(t, row, "from")?, row)?,
            to: role(string(t, row, "to")?, row)?,
        },
    };
    Ok(QuirkRule {
        id,
        name,
        src,
        roles,
        has_roles: !role_names.is_empty(),
        action,
    })
}

impl QuirkRule {
    fn role_matches(&self, rec: &Record) -> bool {
        if !self.has_roles {
            return true;
        }
        matches!(&rec.slots[ix(Group::Role)], Slot::Val(Val::Role(r)) if self.roles.has(*r))
    }

    fn apply(&self, rec: &mut Record) {
        if !self.role_matches(rec) {
            return;
        }
        match &self.action {
            QuirkAction::StripNamePrefix(prefixes) => {
                let i = ix(Group::Name);
                let Slot::Val(Val::Name(name)) = &rec.slots[i] else {
                    return;
                };
                let stripped = prefixes
                    .iter()
                    .find_map(|p| name.strip_prefix(&**p))
                    .filter(|rest| !rest.is_empty());
                if let Some(rest) = stripped {
                    rec.slots[i] = Slot::Val(Val::Name(Arc::from(rest)));
                    rec.hit[i] = self.id;
                }
            }
            QuirkAction::UnknownGroup(g) => {
                let i = ix(*g);
                if !rec.slots[i].is_unknown() {
                    rec.slots[i] = Slot::Unknown;
                    rec.hit[i] = self.id;
                }
            }
            QuirkAction::RoleRemap { from, to } => {
                let i = ix(Group::Role);
                if matches!(&rec.slots[i], Slot::Val(Val::Role(r)) if r == from) {
                    rec.slots[i] = Slot::Val(Val::Role(*to));
                    rec.hit[i] = self.id;
                }
            }
        }
    }
}

impl Quirks for QuirkSet {
    fn apply(&self, src: PrimarySlot, rec: &mut Record) {
        for r in self.rules.iter().filter(|r| r.src == src) {
            r.apply(rec);
        }
    }
}

// ------------------------------------------------------------------------------------------------
// PI and inexpressible
// ------------------------------------------------------------------------------------------------

/// The parsed trust table: a default order and per-group overrides.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PiData {
    pub default: Vec<PrimarySlot>,
    pub groups: Vec<(Group, Vec<PrimarySlot>)>,
}

impl PiData {
    /// Write the table into `profile`.
    pub fn apply(&self, profile: &mut Profile) {
        for g in Group::ALL {
            profile.set_pi(*g, &self.default);
        }
        for (g, order) in &self.groups {
            profile.set_pi(*g, order);
        }
    }
}

fn order(names: &[&str], row: usize) -> Result<Vec<PrimarySlot>> {
    if names.is_empty() || names.len() > MAX_PRIMARIES {
        return Err(QuirkError::Invalid {
            row,
            why: "a PI order lists 1..=MAX_PRIMARIES sources",
        });
    }
    let mut out: Vec<PrimarySlot> = Vec::with_capacity(names.len());
    for n in names {
        let s = source(n, row)?;
        if out.contains(&s) {
            return Err(QuirkError::Invalid {
                row,
                why: "a source appears twice in one PI order",
            });
        }
        out.push(s);
    }
    Ok(out)
}

/// Parse a `[pi]` document: `default = ["p0", "p1"]` and an optional `[pi.groups]` table of
/// per-group orders.
pub fn parse_pi(src: &str) -> Result<PiData> {
    let doc = parse_doc(src)?;
    for (k, _) in doc.iter() {
        if k != "pi" {
            return Err(QuirkError::UnknownKey {
                row: 0,
                key: k.to_owned(),
            });
        }
    }
    let pi = doc
        .get("pi")
        .and_then(Item::as_table)
        .ok_or(QuirkError::Field {
            row: 0,
            field: "pi",
        })?;
    check_keys(pi, 0, &["default", "groups"])?;
    let default = order(&string_list(pi, 0, "default", MAX_PRIMARIES, 8)?, 0)?;
    let mut groups = Vec::new();
    if let Some(g) = pi.get("groups") {
        let g = g.as_table().ok_or(QuirkError::Field {
            row: 0,
            field: "groups",
        })?;
        if g.len() > GROUP_COUNT {
            return Err(QuirkError::TooMany {
                what: "PI group rows",
                max: GROUP_COUNT,
            });
        }
        for (i, (k, item)) in g.iter().enumerate() {
            let row = i + 1;
            let names = list_items(item, row, "groups", MAX_PRIMARIES, 8)?;
            groups.push((group(k, row)?, order(&names, row)?));
        }
    }
    Ok(PiData { default, groups })
}

/// Parse an `inexpressible` document. Each `[[row]]` names a group, a reason and the decision row
/// that approved it (D-4). The `role` group is never inexpressible: a Secondary must not be able
/// to replace a primary role.
pub fn parse_inexpressible(src: &str) -> Result<GroupMask> {
    let doc = parse_doc(src)?;
    let mut mask = GroupMask::EMPTY;
    for (i, t) in rows(&doc, "row")?.into_iter().enumerate() {
        let row = i + 1;
        check_keys(t, row, &["group", "reason", "decision"])?;
        let g = group(string(t, row, "group")?, row)?;
        string(t, row, "reason")?;
        string(t, row, "decision")?;
        if g == Group::Role {
            return Err(QuirkError::Invalid {
                row,
                why: "the role group cannot be inexpressible",
            });
        }
        mask = mask.with(g);
    }
    Ok(mask)
}

// ------------------------------------------------------------------------------------------------
// Source-profile selection
// ------------------------------------------------------------------------------------------------

/// What the connector knows about an app when it picks a source profile.
#[derive(Copy, Clone, Debug, Default)]
pub struct AppFacts<'a> {
    pub app_name: &'a str,
    pub toolkit: Option<&'a str>,
    pub web_engine: Option<&'a str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SelectorRow {
    profile: Arc<str>,
    app_names: Vec<Box<str>>,
    toolkit: Option<Box<str>>,
    web_engine: Option<Box<str>>,
}

/// An ordered list of `[[profile]]` rows; the first row whose every present condition matches wins.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProfileSelector {
    rows: Vec<SelectorRow>,
}

impl ProfileSelector {
    /// Parse and validate a selector document. A row without any condition is rejected (it
    /// would match every app and hide the later rows).
    pub fn parse(src: &str) -> Result<ProfileSelector> {
        let doc = parse_doc(src)?;
        let mut out = Vec::new();
        for (i, t) in rows(&doc, "profile")?.into_iter().enumerate() {
            let row = i + 1;
            check_keys(t, row, &["name", "app_name", "toolkit", "web_engine"])?;
            let app_names: Vec<Box<str>> =
                string_list(t, row, "app_name", MAX_ROWS, MAX_LABEL_BYTES)?
                    .into_iter()
                    .map(Box::from)
                    .collect();
            let toolkit = opt_string(t, row, "toolkit")?.map(Box::from);
            let web_engine = opt_string(t, row, "web_engine")?.map(Box::from);
            if app_names.is_empty() && toolkit.is_none() && web_engine.is_none() {
                return Err(QuirkError::Invalid {
                    row,
                    why: "a profile row needs at least one condition",
                });
            }
            out.push(SelectorRow {
                profile: Arc::from(string(t, row, "name")?),
                app_names,
                toolkit,
                web_engine,
            });
        }
        if out.len() > MAX_ROWS {
            return Err(QuirkError::TooMany {
                what: "profile rows",
                max: MAX_ROWS,
            });
        }
        Ok(ProfileSelector { rows: out })
    }

    /// The profile name for `app`, or `None` (the caller uses its default profile).
    pub fn select(&self, app: &AppFacts<'_>) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| {
                (r.app_names.is_empty() || r.app_names.iter().any(|p| glob(p, app.app_name)))
                    && cond(r.toolkit.as_deref(), app.toolkit)
                    && cond(r.web_engine.as_deref(), app.web_engine)
            })
            .map(|r| &*r.profile)
    }
}

fn cond(want: Option<&str>, have: Option<&str>) -> bool {
    match want {
        None => true,
        Some(w) => have.is_some_and(|h| h.eq_ignore_ascii_case(w)),
    }
}

/// ASCII case-insensitive match with `*` as "any run of characters". Iterative, no recursion,
/// no allocation: a hostile app name cannot blow the stack, and the cost is O(text x pattern).
fn glob(pat: &str, text: &str) -> bool {
    let (p, t) = (pat.as_bytes(), text.as_bytes());
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some(b'*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(c) if c.eq_ignore_ascii_case(&t[ti]) => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == b'*')
}

#[cfg(test)]
mod tests {
    use gorca_model::sem::SemRole;

    use super::*;
    use crate::repair::unknown_eff;

    fn rec_with(role: SemRole, name: &str) -> Record {
        let mut slots = unknown_eff();
        slots[ix(Group::Role)] = Slot::Val(Val::Role(role));
        slots[ix(Group::Name)] = Slot::Val(Val::Name(Arc::from(name)));
        Record {
            slots,
            hit: [0; gorca_model::sem::GROUP_COUNT],
        }
    }

    const MARKER: &str = r#"
[[rule]]
id = 7
name = "t.marker"
roles = ["listitem", "option"]
action = "strip-name-prefix"
prefixes = ["• ", "◦ "]
"#;

    fn name_of(rec: &Record) -> Option<String> {
        match &rec.slots[ix(Group::Name)] {
            Slot::Val(Val::Name(n)) => Some(n.to_string()),
            _ => None,
        }
    }

    #[test]
    fn strip_prefix_applies_by_role_source_and_never_empties_a_name() {
        let q = QuirkSet::parse(MARKER).expect("parses");
        let mut r = rec_with(SemRole::ListItem, "• Alpha");
        q.apply(PrimarySlot::P0, &mut r);
        assert_eq!(name_of(&r).as_deref(), Some("Alpha"));
        assert_eq!(r.hit[ix(Group::Name)], 7);
        assert_eq!(q.rule_name(7), Some("t.marker"));
        assert_eq!(q.rule_name(0), None);
        // Other role, other source, no prefix, bare marker: untouched, no hit.
        for (role, name, src) in [
            (SemRole::Button, "• Alpha", PrimarySlot::P0),
            (SemRole::ListItem, "• Alpha", PrimarySlot::P1),
            (SemRole::ListItem, "Alpha", PrimarySlot::P0),
            (SemRole::ListItem, "• ", PrimarySlot::P0),
        ] {
            let mut r = rec_with(role, name);
            q.apply(src, &mut r);
            assert_eq!(name_of(&r).as_deref(), Some(name));
            assert_eq!(r.hit[ix(Group::Name)], 0);
        }
        // Only the first matching prefix goes ("• ◦ x" keeps "◦ x").
        let mut r = rec_with(SemRole::Option, "• ◦ x");
        q.apply(PrimarySlot::P0, &mut r);
        assert_eq!(name_of(&r).as_deref(), Some("◦ x"));
        // A record without a role matches no role-restricted rule.
        let mut r = Record {
            slots: unknown_eff(),
            hit: [0; gorca_model::sem::GROUP_COUNT],
        };
        r.slots[ix(Group::Name)] = Slot::Val(Val::Name(Arc::from("• A")));
        q.apply(PrimarySlot::P0, &mut r);
        assert_eq!(name_of(&r).as_deref(), Some("• A"));
    }

    #[test]
    fn unknown_group_and_role_remap() {
        let q = QuirkSet::parse(
            r#"
[[rule]]
id = 1
name = "pos"
roles = ["menuitem"]
action = "unknown-group"
group = "setpos"
[[rule]]
id = 2
name = "remap"
action = "role-remap"
from = "list"
to = "descriptionlist"
"#,
        )
        .expect("parses");
        let mut r = rec_with(SemRole::MenuItem, "x");
        r.slots[ix(Group::SetPos)] = Slot::Absent;
        q.apply(PrimarySlot::P0, &mut r);
        assert!(r.slots[ix(Group::SetPos)].is_unknown());
        assert_eq!(r.hit[ix(Group::SetPos)], 1);
        let mut r = rec_with(SemRole::List, "x");
        q.apply(PrimarySlot::P0, &mut r);
        assert_eq!(
            r.slots[ix(Group::Role)],
            Slot::Val(Val::Role(SemRole::DescriptionList))
        );
        assert_eq!(r.hit[ix(Group::Role)], 2);
        // Rules chain in file order: a later rule sees an earlier rewrite.
        let q = QuirkSet::parse(
            r#"
[[rule]]
id = 1
name = "a"
action = "role-remap"
from = "list"
to = "menu"
[[rule]]
id = 2
name = "b"
roles = ["menu"]
action = "unknown-group"
group = "name"
"#,
        )
        .expect("parses");
        let mut r = rec_with(SemRole::List, "x");
        q.apply(PrimarySlot::P0, &mut r);
        assert!(r.slots[ix(Group::Name)].is_unknown());
    }

    #[test]
    fn the_loader_rejects_everything_outside_the_closed_schema() {
        let bad = |s: &str| QuirkSet::parse(s).expect_err("must be rejected");
        let rule = |extra: &str| format!("[[rule]]\nid = 1\nname = \"n\"\n{extra}\n");
        assert!(matches!(
            bad(&rule("action = \"run-code\"")),
            QuirkError::UnknownAction { .. }
        ));
        assert!(matches!(
            bad(&rule("action = \"unknown-group\"\ngroup = \"colour\"")),
            QuirkError::UnknownGroup { .. }
        ));
        assert!(matches!(
            bad(&rule("action = \"unknown-group\"\ngroup = \"role\"")),
            QuirkError::Invalid { .. }
        ));
        assert!(matches!(
            bad(&rule(
                "action = \"role-remap\"\nfrom = \"nope\"\nto = \"list\""
            )),
            QuirkError::UnknownRole { .. }
        ));
        assert!(matches!(
            bad(&rule(
                "action = \"strip-name-prefix\"\nprefixes = [\"x\"]\nextra = 1"
            )),
            QuirkError::UnknownKey { .. }
        ));
        assert!(matches!(
            bad(&rule("action = \"strip-name-prefix\"\nprefixes = []")),
            QuirkError::Field { .. }
        ));
        assert!(matches!(
            bad(&rule(
                "action = \"strip-name-prefix\"\nprefixes = [\"x\"]\nsource = \"p9\""
            )),
            QuirkError::UnknownSource { .. }
        ));
        assert!(matches!(bad("rule = 3"), QuirkError::Field { .. }));
        assert!(matches!(bad("surprise = 1"), QuirkError::UnknownKey { .. }));
        assert!(matches!(bad("[[rule"), QuirkError::Syntax(_)));
        // id 0 and ids that do not fit u16.
        for id in ["0", "70000", "-1"] {
            let s = format!(
                "[[rule]]\nid = {id}\nname = \"n\"\naction = \"unknown-group\"\ngroup = \"name\""
            );
            assert!(
                matches!(bad(&s), QuirkError::Field { field: "id", .. }),
                "id {id}"
            );
        }
        // Duplicate ids, within a document and across two.
        let two = format!("{MARKER}{MARKER}");
        assert_eq!(bad(&two), QuirkError::DuplicateId(7));
        let mut a = QuirkSet::parse(MARKER).expect("a");
        assert_eq!(
            a.extend(QuirkSet::parse(MARKER).expect("b")),
            Err(QuirkError::DuplicateId(7))
        );
    }

    #[test]
    fn unknown_group_on_an_inexpressible_group_is_rejected() {
        let q = QuirkSet::parse(
            "[[rule]]\nid = 1\nname = \"n\"\naction = \"unknown-group\"\ngroup = \"invalid\"",
        )
        .expect("parses");
        assert!(q.validate_against(GroupMask::EMPTY).is_ok());
        assert!(
            q.validate_against(GroupMask::EMPTY.with(Group::Name))
                .is_ok()
        );
        assert!(matches!(
            q.validate_against(GroupMask::EMPTY.with(Group::Invalid)),
            Err(QuirkError::Invalid { row: 1, .. })
        ));
    }

    #[test]
    fn size_caps_hold() {
        let big = format!("# {}\n", "x".repeat(MAX_DOC_BYTES));
        assert!(matches!(
            QuirkSet::parse(&big),
            Err(QuirkError::TooLarge { .. })
        ));
        let prefixes = (0..=MAX_PREFIXES)
            .map(|i| format!("\"p{i}\""))
            .collect::<Vec<_>>()
            .join(",");
        let s = format!(
            "[[rule]]\nid = 1\nname = \"n\"\naction = \"strip-name-prefix\"\nprefixes = [{prefixes}]"
        );
        assert!(matches!(
            QuirkSet::parse(&s),
            Err(QuirkError::TooMany { .. })
        ));
        let long = "y".repeat(MAX_PREFIX_BYTES + 1);
        let s = format!(
            "[[rule]]\nid = 1\nname = \"n\"\naction = \"strip-name-prefix\"\nprefixes = [\"{long}\"]"
        );
        assert!(matches!(QuirkSet::parse(&s), Err(QuirkError::Field { .. })));
        let mut many = String::new();
        for i in 1..=MAX_RULES + 1 {
            let _ = std::fmt::Write::write_fmt(
                &mut many,
                format_args!(
                    "[[rule]]\nid = {i}\nname = \"n\"\naction = \"unknown-group\"\ngroup = \"name\"\n"
                ),
            );
        }
        assert!(matches!(
            QuirkSet::parse(&many),
            Err(QuirkError::TooMany { .. })
        ));
    }

    #[test]
    fn pi_tables_load_and_apply() {
        let pi =
            parse_pi("[pi]\ndefault = [\"p0\", \"p1\"]\n[pi.groups]\nlevel = [\"p1\", \"p0\"]\n")
                .expect("pi");
        let mut p = Profile::new(&[PrimarySlot::P1], crate::profile::Rules::aria());
        pi.apply(&mut p);
        assert_eq!(p.pi(Group::Name), &[PrimarySlot::P0, PrimarySlot::P1]);
        assert_eq!(p.pi(Group::Level), &[PrimarySlot::P1, PrimarySlot::P0]);
        for s in [
            "[pi]\ndefault = []",
            "[pi]\ndefault = [\"p0\", \"p0\"]",
            "[pi]\ndefault = [\"p0\", \"p1\", \"p0\"]",
            "[pi]\ndefault = [\"p2\"]",
            "[pi]\ndefault = [\"p0\"]\nmore = 1",
            "[pi]\ndefault = [\"p0\"]\n[pi.groups]\nbogus = [\"p0\"]",
            "other = 1",
        ] {
            assert!(parse_pi(s).is_err(), "{s}");
        }
    }

    #[test]
    fn inexpressible_rows_need_a_decision_and_never_cover_role() {
        let ok = "[[row]]\ngroup = \"invalid\"\nreason = \"r\"\ndecision = \"d\"\n";
        assert_eq!(
            parse_inexpressible(ok).expect("ok"),
            GroupMask::EMPTY.with(Group::Invalid)
        );
        assert_eq!(
            parse_inexpressible("# none").expect("empty"),
            GroupMask::EMPTY
        );
        for s in [
            "[[row]]\ngroup = \"invalid\"\nreason = \"r\"",
            "[[row]]\ngroup = \"invalid\"\ndecision = \"d\"",
            "[[row]]\ngroup = \"role\"\nreason = \"r\"\ndecision = \"d\"",
            "[[row]]\ngroup = \"nope\"\nreason = \"r\"\ndecision = \"d\"",
            "[[row]]\ngroup = \"invalid\"\nreason = \"r\"\ndecision = \"d\"\nx = 1",
        ] {
            assert!(parse_inexpressible(s).is_err(), "{s}");
        }
    }

    #[test]
    fn selector_first_match_wins_and_conditions_are_anded() {
        let s = ProfileSelector::parse(
            r#"
[[profile]]
name = "gecko"
web_engine = "Gecko"
[[profile]]
name = "blink"
app_name = ["chrom*", "*-browser"]
[[profile]]
name = "gtk4"
toolkit = "gtk"
app_name = ["gnome-*"]
"#,
        )
        .expect("parses");
        let f = |n, t, w| {
            s.select(&AppFacts {
                app_name: n,
                toolkit: t,
                web_engine: w,
            })
        };
        assert_eq!(f("anything", None, Some("gecko")), Some("gecko"));
        assert_eq!(f("Chromium", None, None), Some("blink"));
        assert_eq!(f("my-BROWSER", None, None), Some("blink"));
        assert_eq!(f("gnome-calculator", Some("GTK"), None), Some("gtk4"));
        assert_eq!(
            f("gnome-calculator", None, None),
            None,
            "both conditions must hold"
        );
        assert_eq!(f("kate", Some("Qt"), None), None);
        assert!(
            ProfileSelector::parse("[[profile]]\nname = \"x\"").is_err(),
            "no condition"
        );
        assert!(ProfileSelector::parse("[[profile]]\nname = \"x\"\nweb = \"a\"").is_err());
    }

    #[test]
    fn glob_is_linear_and_exact() {
        assert!(glob("firefox*", "Firefox-esr"));
        assert!(glob("*fox", "FIREFOX"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        assert!(glob("*", ""));
        assert!(!glob("a", "ab"));
        // Pathological pattern against a long non-matching text finishes at once.
        let text = "a".repeat(100_000);
        assert!(!glob("a*a*a*a*a*a*b", &text));
    }
}
