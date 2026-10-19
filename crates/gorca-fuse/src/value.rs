// SPDX-License-Identifier: GPL-3.0-or-later
//! Group values as the fusion sees them: one typed [`Val`] per registered group, the three-state
//! [`Slot`] (`Unknown`, `Absent`, a value) and the provenance triple [`Info`].
//!
//! `Unknown` (never observed) is **not** a value and lets a lower layer fill in; `Absent`
//! (observed, property not present) **is** a value and wins against lower layers (math section
//! 1.1). Both finalise to the group's default when the entry is written.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::Arc;

use gorca_model::event::Politeness;
use gorca_model::sem::{
    Avail, Check, ConfClass, Current, Expand, GridPos, Group, Invalid, Live, NameFrom, Orientation,
    Popup, Press, ProvCode, Select, SemEntry, SemFlags, SemRole, SemText, SetPos, Sort, TrustTier,
};

/// Product limit for a name or description string (ADR string limit, 1 KiB).
pub const MAX_TEXT_BYTES: usize = 1024;

/// `s` cut to at most `max` bytes at a char boundary.
pub fn truncate_str(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or("")
}

/// True when `fused` is a name from content that contains everything of the engine's `own` name
/// and is not the same text: the same words (the engine joins a row's cells without a separator,
/// "SeatLeftRight", where S3 puts a space between them) plus whatever the engine did not see.
/// Spaces are not compared.
pub fn name_extends(own: &str, fused: &str) -> bool {
    let squeeze = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let (o, f) = (squeeze(own), squeeze(fused));
    !o.is_empty() && own != fused && fused.len() >= own.len() && f.contains(&o)
}

/// A per-source monotone sequence number (orders writes within one source only, Lemma 1).
pub type Seq = u64;

/// One typed group value. The group is implied by the variant (`name` and `description` are two
/// variants because they share the string payload).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Val {
    Role(SemRole),
    Name(Arc<str>),
    Description(Arc<str>),
    Check(Check),
    Press(Press),
    Expand(Expand),
    Select(Select),
    Current(Current),
    Invalid(Invalid),
    Popup(Popup),
    Avail(Avail),
    Flags(SemFlags),
    Level(u8),
    SetPos(SetPos),
    Grid(GridPos),
    /// `None` is an explicit "off" (a value), unlike `Absent`.
    Live(Option<Live>),
    Orientation(Orientation),
    Sort(Sort),
    /// `aria-roledescription` (rare text, one register each; primary sources only).
    RoleDescription(Arc<str>),
    Placeholder(Arc<str>),
    KeyShortcuts(Arc<str>),
    ValueText(Arc<str>),
    NameFrom(NameFrom),
}

impl Val {
    /// The group this value belongs to.
    pub const fn group(&self) -> Group {
        match self {
            Val::Role(_) => Group::Role,
            Val::Name(_) => Group::Name,
            Val::Description(_) => Group::Description,
            Val::Check(_) => Group::Check,
            Val::Press(_) => Group::Press,
            Val::Expand(_) => Group::Expand,
            Val::Select(_) => Group::Select,
            Val::Current(_) => Group::Current,
            Val::Invalid(_) => Group::Invalid,
            Val::Popup(_) => Group::Popup,
            Val::Avail(_) => Group::Avail,
            Val::Flags(_) => Group::Flags,
            Val::Level(_) => Group::Level,
            Val::SetPos(_) => Group::SetPos,
            Val::Grid(_) => Group::Grid,
            Val::Live(_) => Group::Live,
            Val::Orientation(_) => Group::Orientation,
            Val::Sort(_) => Group::Sort,
            Val::RoleDescription(_) => Group::RoleDescription,
            Val::Placeholder(_) => Group::Placeholder,
            Val::KeyShortcuts(_) => Group::KeyShortcuts,
            Val::ValueText(_) => Group::ValueText,
            Val::NameFrom(_) => Group::NameFrom,
        }
    }

    /// True for an empty string value or `Level(0)`: the "nothing here" values that the fill rule
    /// (A6) treats like `Absent`.
    pub fn is_empty_like(&self) -> bool {
        match self {
            Val::Name(s)
            | Val::Description(s)
            | Val::RoleDescription(s)
            | Val::Placeholder(s)
            | Val::KeyShortcuts(s)
            | Val::ValueText(s) => s.is_empty(),
            Val::Level(l) => *l == 0,
            _ => false,
        }
    }

    /// The value text used by the deterministic tie-break (and diagnostics).
    fn tie_text(&self) -> Cow<'_, str> {
        match self {
            Val::Role(v) => Cow::Borrowed(v.name()),
            Val::Name(s)
            | Val::Description(s)
            | Val::RoleDescription(s)
            | Val::Placeholder(s)
            | Val::KeyShortcuts(s)
            | Val::ValueText(s) => Cow::Borrowed(s),
            Val::NameFrom(v) => Cow::Borrowed(v.name()),
            Val::Check(v) => Cow::Borrowed(v.name()),
            Val::Press(v) => Cow::Borrowed(v.name()),
            Val::Expand(v) => Cow::Borrowed(v.name()),
            Val::Select(v) => Cow::Borrowed(v.name()),
            Val::Current(v) => Cow::Borrowed(v.name()),
            Val::Invalid(v) => Cow::Borrowed(v.name()),
            Val::Popup(v) => Cow::Borrowed(v.name()),
            Val::Avail(v) => Cow::Borrowed(v.name()),
            Val::Orientation(v) => Cow::Borrowed(v.name()),
            Val::Sort(v) => Cow::Borrowed(v.name()),
            Val::Level(l) => Cow::Owned(l.to_string()),
            Val::Flags(f) => Cow::Owned(f.bits().to_string()),
            Val::SetPos(p) => Cow::Owned(format!("{p:?}")),
            Val::Grid(g) => Cow::Owned(format!("{g:?}")),
            Val::Live(None) => Cow::Borrowed("off"),
            Val::Live(Some(l)) => Cow::Owned(format!(
                "{}.{}.{}.{}",
                match l.politeness {
                    Politeness::Polite => "polite",
                    Politeness::Assertive => "assertive",
                },
                l.atomic,
                l.relevant.bits(),
                l.root
            )),
        }
    }

    /// Write this value into `entry` (the group's field). Empty names and descriptions store
    /// `None`.
    pub fn write_to(&self, entry: &mut SemEntry) {
        match self {
            Val::Role(v) => entry.sem.role = *v,
            Val::Name(s) => entry.name = non_empty(s),
            Val::Description(s) => set_description(entry, non_empty(s)),
            Val::Check(v) => entry.sem.check = *v,
            Val::Press(v) => entry.sem.press = *v,
            Val::Expand(v) => entry.sem.expand = *v,
            Val::Select(v) => entry.sem.select = *v,
            Val::Current(v) => entry.sem.current = *v,
            Val::Invalid(v) => entry.sem.invalid = *v,
            Val::Popup(v) => entry.sem.popup = *v,
            Val::Avail(v) => entry.sem.avail = *v,
            Val::Flags(v) => entry.sem.flags = *v,
            Val::Level(v) => entry.sem.level = *v,
            Val::SetPos(v) => entry.sem.pos = Some(*v),
            Val::Grid(v) => entry.sem.grid = Some(*v),
            Val::Live(v) => entry.sem.live = *v,
            Val::Orientation(v) => entry.sem.orientation = *v,
            Val::Sort(v) => entry.sem.sort = *v,
            Val::RoleDescription(s) => set_text(entry, TextField::RoleDescription, non_empty(s)),
            Val::Placeholder(s) => set_text(entry, TextField::Placeholder, non_empty(s)),
            Val::KeyShortcuts(s) => set_text(entry, TextField::KeyShortcuts, non_empty(s)),
            Val::ValueText(s) => set_text(entry, TextField::ValueText, non_empty(s)),
            Val::NameFrom(v) => entry.sem.name_from = *v,
        }
    }
}

/// The register write of one of the groups that read a single field of `sem` and nothing else
/// (the rare text and the name source): an empty or missing string is `Absent`. `None` for any
/// other group (the writers own those). One definition for every writer, so a new rare group is
/// added here and nowhere else.
pub fn rare_write(sem: &gorca_model::sem::Semantics, g: Group) -> Option<Write> {
    fn text(g: Group, s: Option<&str>, f: impl FnOnce(Arc<str>) -> Val) -> Write {
        match s {
            Some(t) if !t.is_empty() => Write::Val(f(Arc::from(t))),
            Some(_) | None => Write::Absent(g),
        }
    }
    let t = sem.text.as_deref();
    Some(match g {
        Group::RoleDescription => text(g, sem.role_description(), Val::RoleDescription),
        Group::Placeholder => text(
            g,
            t.and_then(|t| t.placeholder.as_deref()),
            Val::Placeholder,
        ),
        Group::KeyShortcuts => text(
            g,
            t.and_then(|t| t.key_shortcuts.as_deref()),
            Val::KeyShortcuts,
        ),
        Group::ValueText => text(g, t.and_then(|t| t.value_text.as_deref()), Val::ValueText),
        Group::NameFrom => Write::Val(Val::NameFrom(sem.name_from)),
        Group::Role
        | Group::Name
        | Group::Description
        | Group::Check
        | Group::Press
        | Group::Expand
        | Group::Select
        | Group::Current
        | Group::Invalid
        | Group::Popup
        | Group::Avail
        | Group::Flags
        | Group::Level
        | Group::SetPos
        | Group::Grid
        | Group::Live
        | Group::Orientation
        | Group::Sort => return None,
    })
}

fn non_empty(s: &Arc<str>) -> Option<Arc<str>> {
    if s.is_empty() {
        None
    } else {
        Some(Arc::clone(s))
    }
}

/// The `SemText` fields that have a register of their own.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum TextField {
    Description,
    RoleDescription,
    Placeholder,
    KeyShortcuts,
    ValueText,
}

fn set_description(entry: &mut SemEntry, d: Option<Arc<str>>) {
    set_text(entry, TextField::Description, d);
}

fn set_text(entry: &mut SemEntry, f: TextField, v: Option<Arc<str>>) {
    let mut t = entry
        .sem
        .text
        .as_deref()
        .cloned()
        .unwrap_or_else(SemText::default);
    match f {
        TextField::Description => t.description = v,
        TextField::RoleDescription => t.role_description = v,
        TextField::Placeholder => t.placeholder = v,
        TextField::KeyShortcuts => t.key_shortcuts = v,
        TextField::ValueText => t.value_text = v,
    }
    entry.sem.text = if t.is_empty() {
        None
    } else {
        Some(Arc::new(t))
    };
}

/// Reset one group of `entry` to its default (what `Absent` and "nothing known" finalise to).
/// Only the role default is configurable (the model default is `Unknown`).
pub fn write_default(entry: &mut SemEntry, g: Group, default_role: SemRole) {
    match g {
        Group::Role => entry.sem.role = default_role,
        Group::Name => entry.name = None,
        Group::Description => set_description(entry, None),
        Group::Check => entry.sem.check = Check::default(),
        Group::Press => entry.sem.press = Press::default(),
        Group::Expand => entry.sem.expand = Expand::default(),
        Group::Select => entry.sem.select = Select::default(),
        Group::Current => entry.sem.current = Current::default(),
        Group::Invalid => entry.sem.invalid = Invalid::default(),
        Group::Popup => entry.sem.popup = Popup::default(),
        Group::Avail => entry.sem.avail = Avail::default(),
        Group::Flags => entry.sem.flags = SemFlags::default(),
        Group::Level => entry.sem.level = 0,
        Group::SetPos => entry.sem.pos = None,
        Group::Grid => entry.sem.grid = None,
        Group::Live => entry.sem.live = None,
        Group::Orientation => entry.sem.orientation = Orientation::default(),
        Group::Sort => entry.sem.sort = Sort::default(),
        Group::RoleDescription => set_text(entry, TextField::RoleDescription, None),
        Group::Placeholder => set_text(entry, TextField::Placeholder, None),
        Group::KeyShortcuts => set_text(entry, TextField::KeyShortcuts, None),
        Group::ValueText => set_text(entry, TextField::ValueText, None),
        Group::NameFrom => entry.sem.name_from = NameFrom::default(),
    }
}

/// The state of one group in a record or in the effective view.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum Slot {
    /// Never observed: not a value, a lower layer may fill in.
    #[default]
    Unknown,
    /// Observed, property not present: a value.
    Absent,
    Val(Val),
}

impl Slot {
    pub const fn is_unknown(&self) -> bool {
        matches!(self, Slot::Unknown)
    }

    /// The fill rule (A6): `Unknown`, `Absent` and the empty-like values are fillable.
    pub fn is_empty_like(&self) -> bool {
        match self {
            Slot::Unknown | Slot::Absent => true,
            Slot::Val(v) => v.is_empty_like(),
        }
    }

    /// The name or description text, `None` for any other state.
    pub fn text(&self) -> Option<&Arc<str>> {
        match self {
            Slot::Val(Val::Name(s) | Val::Description(s)) => Some(s),
            _ => None,
        }
    }

    /// The role, if this slot holds one.
    pub const fn role(&self) -> Option<SemRole> {
        match self {
            Slot::Val(Val::Role(r)) => Some(*r),
            _ => None,
        }
    }
}

/// A register write: a value or an explicit `Absent`. The group is carried, so a value can never
/// be written into another group's register.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Write {
    Absent(Group),
    Val(Val),
}

impl Write {
    pub const fn group(&self) -> Group {
        match self {
            Write::Absent(g) => *g,
            Write::Val(v) => v.group(),
        }
    }

    /// True for `Absent` and the empty-like values (the fill rule's "nothing here").
    pub fn is_empty_like(&self) -> bool {
        match self {
            Write::Absent(_) => true,
            Write::Val(v) => v.is_empty_like(),
        }
    }

    /// This write with a `Name`/`Description` string cut to `max` bytes; the flag tells whether
    /// it was cut.
    #[must_use]
    pub fn capped(self, max: usize) -> (Write, bool) {
        match self {
            Write::Val(Val::Name(s)) if s.len() > max => (
                Write::Val(Val::Name(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            Write::Val(Val::Description(s)) if s.len() > max => (
                Write::Val(Val::Description(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            Write::Val(Val::RoleDescription(s)) if s.len() > max => (
                Write::Val(Val::RoleDescription(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            Write::Val(Val::Placeholder(s)) if s.len() > max => (
                Write::Val(Val::Placeholder(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            Write::Val(Val::KeyShortcuts(s)) if s.len() > max => (
                Write::Val(Val::KeyShortcuts(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            Write::Val(Val::ValueText(s)) if s.len() > max => (
                Write::Val(Val::ValueText(Arc::from(truncate_str(&s, max)))),
                true,
            ),
            w => (w, false),
        }
    }

    pub fn slot(&self) -> Slot {
        match self {
            Write::Absent(_) => Slot::Absent,
            Write::Val(v) => Slot::Val(v.clone()),
        }
    }

    /// Deterministic total order used only to break an impossible sequence collision
    /// (`vkey` of the oracle: numbers before strings, then the text; `Absent` is "-").
    pub fn tie_cmp(&self, other: &Write) -> Ordering {
        self.tie_key().cmp(&other.tie_key())
    }

    /// The `(type tag, text)` key behind [`Write::tie_cmp`]. Allocates; only used on the
    /// (practically never taken) collision path.
    pub fn tie_key(&self) -> (u8, String) {
        match self {
            Write::Absent(_) => (1, "-".to_owned()),
            Write::Val(v) => (
                u8::from(!matches!(v, Val::Level(_))),
                v.tie_text().into_owned(),
            ),
        }
    }
}

/// Provenance of one group's winner: the `ProvCode`, the confidence class after the T3 cap and the
/// highest trust tier among the inputs (taint).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Info {
    pub prov: ProvCode,
    pub conf: ConfClass,
    pub tier: TrustTier,
}

impl Info {
    /// Default, plain, untainted.
    pub const DEFAULT: Info = Info::plain(ProvCode::Default);

    /// Primary, patch, user and untainted derived winners: plain and untainted.
    pub const fn plain(prov: ProvCode) -> Info {
        Info {
            prov,
            conf: ConfClass::Asserted,
            tier: TrustTier::T0Core,
        }
    }

    /// True for the winners that are tainted (secondary, inferred, derived from tainted inputs).
    pub const fn is_tainted_prov(prov: ProvCode) -> bool {
        match prov {
            ProvCode::DerivedTainted | ProvCode::Inferred | ProvCode::ObservedSecondary => true,
            ProvCode::Default
            | ProvCode::Derived
            | ProvCode::ObservedPrimary
            | ProvCode::Patch
            | ProvCode::User => false,
        }
    }
}

/// Any trust tier T3 input caps the class at `Probable` (H-5): "probably Delete", never a plain
/// T3-derived name.
pub const fn hedge_cap(conf: ConfClass, tier: TrustTier) -> ConfClass {
    match tier {
        TrustTier::T3Untrusted => conf.weaker(ConfClass::Probable),
        TrustTier::T0Core | TrustTier::T1Platform | TrustTier::T2FirstParty => conf,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn val_group_matches_write_default_round_trip() {
        let mut e = SemEntry::default();
        Val::Name(Arc::from("Close")).write_to(&mut e);
        Val::Description(Arc::from("Opens")).write_to(&mut e);
        assert_eq!(e.name.as_deref(), Some("Close"));
        assert_eq!(e.sem.description(), Some("Opens"));
        write_default(&mut e, Group::Name, SemRole::Generic);
        write_default(&mut e, Group::Description, SemRole::Generic);
        assert_eq!(e.name, None);
        assert_eq!(e.sem.text, None, "an empty SemText is dropped");
    }

    #[test]
    fn empty_like_values() {
        assert!(Slot::Unknown.is_empty_like());
        assert!(Slot::Absent.is_empty_like());
        assert!(Slot::Val(Val::Name(Arc::from(""))).is_empty_like());
        assert!(Slot::Val(Val::Level(0)).is_empty_like());
        assert!(!Slot::Val(Val::Check(Check::NotCheckable)).is_empty_like());
    }

    #[test]
    fn t3_cap_is_probable_or_weaker() {
        assert_eq!(
            hedge_cap(ConfClass::High, TrustTier::T3Untrusted),
            ConfClass::Probable
        );
        assert_eq!(
            hedge_cap(ConfClass::Possible, TrustTier::T3Untrusted),
            ConfClass::Possible
        );
        assert_eq!(
            hedge_cap(ConfClass::High, TrustTier::T2FirstParty),
            ConfClass::High
        );
    }

    #[test]
    fn tie_order_matches_the_oracle_vkey() {
        let a = Write::Val(Val::Description(Arc::from("Opens a dialog")));
        let b = Write::Val(Val::Description(Arc::from("Required")));
        assert_eq!(a.tie_cmp(&b), Ordering::Less);
        let lvl = Write::Val(Val::Level(2));
        assert_eq!(lvl.tie_cmp(&Write::Absent(Group::Level)), Ordering::Less);
    }
}
