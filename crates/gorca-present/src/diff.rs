// SPDX-License-Identifier: GPL-3.0-or-later
//! The shadow diff (ADR-0022 section 2.11, M-13): token multisets of the legacy presenter and
//! of `gorca-present`, compared as (kind, literal).

use gorca_model::SegmentKind;
use gorca_model::sem::{Avail, Check, Group, Press, ProvCode, Select, SemEntry, SemRole};

use gorca_i18n::Lang;

use crate::deviations::Deviations;
use crate::render::Token;

/// A token the legacy presenter produced, reduced to what is compared.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LegacyToken {
    pub kind: SegmentKind,
    pub text: std::sync::Arc<str>,
}

/// A stable ordinal of a segment kind (the type has no `Ord`).
pub const fn kind_code(k: SegmentKind) -> u8 {
    match k {
        SegmentKind::Text => 0,
        SegmentKind::Role => 1,
        SegmentKind::State => 2,
        SegmentKind::Value => 3,
        SegmentKind::Description => 4,
        SegmentKind::Said => 5,
        SegmentKind::Capital => 6,
    }
}

/// The result of one comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiffOutcome {
    /// Same tokens in the same order.
    Equal,
    /// Same multiset, different order.
    OrderOnly,
    /// A difference listed in `deviations.toml`.
    Allowed(&'static str),
    /// A difference nobody allowed: `legacy_only` tokens the new presenter lacks, `new_only` the
    /// ones it added.
    Different { legacy_only: u32, new_only: u32 },
}

/// A classifier of a known, allowed difference. The id is a row of `deviations.toml`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Classifier {
    /// A field's contents are spoken by the legacy presenter as its value; they are text-layer
    /// content, not a fused group.
    FieldContentsNotFused,
    /// The fused name was derived from the node's content; the legacy snapshot had no name.
    ContentDerivedName,
    /// The legacy presenter says "pressed" for a node the model says is not a toggle.
    PressedWithoutToggle,
    /// The legacy presenter had no role word (its role mapping missed); the model has one.
    RoleFromModel,
    /// The fused name drops a list marker or bullet the legacy name carried (quirk row).
    MarkerStrippedName,
    /// The fused name is a one-line label: the legacy name carried runs of white space.
    NameWhitespace,
    /// Static text: the engine's `label` and `static` roles are one model role.
    StaticTextWord,
    /// A fieldset or group: the model says "grouping" where the legacy mapping said "panel".
    GroupingWord,
    /// A role the web table has no word for: the legacy mapping spoke one from the engine role.
    RoleNotInTable,
    /// The legacy presenter said "unavailable" for static structure the model never disables.
    UnavailableStatic,
    /// A role word the model improves where the legacy mapping used a coarser one (a fixed list
    /// of (role, legacy word, model word) pairs).
    RoleWordImproved,
    /// A very long name: the legacy and the model cut it at different lengths.
    NameCapDiffers,
    /// A flip of selection on a node the model says cannot be selected (a menu item, a button).
    SelectNotApplicable,
    /// A switch is never half-checked; the legacy presenter said "half pressed" for the mixed
    /// state the engine reported on it.
    MixedSwitchIgnored,
    /// A toggle button exports its on state as `checked` (GTK3); the legacy change presenter said
    /// checked, the model says pressed, like the focus row and Orca do.
    ToggleStateWord,
    /// The model says the field is read only; the legacy snapshot's state set did not.
    ReadOnlyFromModel,
    /// The fused value (`aria-valuetext`, `aria-valuenow`) is spoken where the legacy snapshot of
    /// the recorded fixture carries no `Value` interface.
    ValueFromAttrs,
}

/// Roles whose field contents the legacy presenter speaks as a value and the text layer owns:
/// text entries only (contents are fetched for them alone). Spin buttons and combo boxes carry a
/// numeric value that the fused entry holds, so a missing one is a real difference.
pub const CONTENTS_ROLES: &[SemRole] =
    &[SemRole::TextBox, SemRole::SearchBox, SemRole::PasswordBox];

/// Roles for which a role word the legacy mapping lacked is an accepted improvement: landmarks,
/// popovers, sliders, scroll bars, tooltips, generic panels. A scroll bar and a tooltip have their
/// own words (`scroll bar`, `tooltip`), not "slider" and "popover" (review F-5/F-7).
pub const ROLE_FROM_MODEL_ROLES: &[SemRole] = &[
    SemRole::Region,
    SemRole::Banner,
    SemRole::Complementary,
    SemRole::ContentInfo,
    SemRole::Main,
    SemRole::Navigation,
    SemRole::Search,
    SemRole::Article,
    SemRole::Popover,
    SemRole::Slider,
    SemRole::Scrollbar,
    SemRole::Tooltip,
    SemRole::Generic,
    SemRole::Group,
    SemRole::Presentation,
    SemRole::Application,
    SemRole::Term,
    SemRole::Definition,
    SemRole::Feed,
    SemRole::Img,
    SemRole::Form,
    SemRole::DescriptionList,
];

/// Roles the web table says nothing for where the legacy mapping spoke a word.
pub const WEB_UNWORDED_ROLES: &[SemRole] = &[
    SemRole::Log,
    SemRole::Figure,
    SemRole::Note,
    SemRole::Marquee,
    SemRole::Timer,
    SemRole::Math,
    SemRole::Strong,
    SemRole::Emphasis,
    SemRole::Mark,
    SemRole::Code,
    SemRole::Subscript,
    SemRole::Superscript,
    SemRole::Insertion,
    SemRole::Deletion,
    SemRole::Time,
    SemRole::Form,
    SemRole::RowGroup,
    SemRole::Comment,
    SemRole::Suggestion,
];

/// Roles whose value the fused entry knows from attributes.
pub const VALUE_ROLES: &[SemRole] = &[
    SemRole::Slider,
    SemRole::ProgressBar,
    SemRole::Meter,
    SemRole::SpinButton,
    SemRole::Separator,
    SemRole::Scrollbar,
    SemRole::Combobox,
];

/// (role, legacy word, model word) pairs accepted by `role-word-improved`.
pub fn role_word_pairs(lang: Lang) -> [(SemRole, &'static str, &'static str); 5] {
    use gorca_i18n::MsgId as M;
    let t = |id| gorca_i18n::t_in(lang, id);
    [
        (SemRole::Option, t(M::RolePanel), t(M::RoleListItem)),
        (SemRole::Feed, t(M::RolePanel), t(M::RoleList)),
        (SemRole::Definition, t(M::RoleParagraph), t(M::RoleListItem)),
        // A layout table (presentation role) is not a table for the user.
        (SemRole::Presentation, t(M::RoleTable), t(M::RolePanel)),
        (SemRole::AlertDialog, t(M::RoleAlert), t(M::RoleDialog)),
    ]
}

impl Classifier {
    pub const ALL: [Classifier; 17] = [
        Classifier::FieldContentsNotFused,
        Classifier::ContentDerivedName,
        Classifier::PressedWithoutToggle,
        Classifier::RoleFromModel,
        Classifier::MarkerStrippedName,
        Classifier::NameWhitespace,
        Classifier::StaticTextWord,
        Classifier::GroupingWord,
        Classifier::RoleNotInTable,
        Classifier::ValueFromAttrs,
        Classifier::ReadOnlyFromModel,
        Classifier::UnavailableStatic,
        Classifier::RoleWordImproved,
        Classifier::NameCapDiffers,
        Classifier::SelectNotApplicable,
        Classifier::MixedSwitchIgnored,
        Classifier::ToggleStateWord,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Classifier::FieldContentsNotFused => "field-contents-not-fused",
            Classifier::ContentDerivedName => "content-derived-name",
            Classifier::PressedWithoutToggle => "pressed-without-toggle",
            Classifier::RoleFromModel => "role-from-model",
            Classifier::MarkerStrippedName => "marker-stripped-name",
            Classifier::NameWhitespace => "name-whitespace",
            Classifier::StaticTextWord => "static-text-word",
            Classifier::GroupingWord => "grouping-word",
            Classifier::RoleNotInTable => "role-not-in-table",
            Classifier::ValueFromAttrs => "value-from-attrs",
            Classifier::ReadOnlyFromModel => "read-only-from-model",
            Classifier::UnavailableStatic => "unavailable-static",
            Classifier::RoleWordImproved => "role-word-improved",
            Classifier::NameCapDiffers => "name-cap-differs",
            Classifier::SelectNotApplicable => "select-not-applicable",
            Classifier::MixedSwitchIgnored => "mixed-switch-ignored",
            Classifier::ToggleStateWord => "toggle-state-word",
        }
    }
}

/// What the diff needs to know about the comparison besides the tokens.
#[derive(Copy, Clone, Debug)]
#[allow(clippy::struct_excessive_bools)] // one model fact per bit
pub struct DiffCtx<'a> {
    /// The surface being compared (`deviations.toml` `surface`).
    pub surface: &'a str,
    /// The fused role of the node: the per-role gates of the classifiers read it.
    pub role: SemRole,
    /// The fused name was derived from content (`ProvCode::Derived`).
    pub name_derived: bool,
    /// The model says the node is neither checkable nor a toggle.
    pub untoggled: bool,
    /// The model says the node has no pressed state (`Press::NotToggle`).
    pub not_toggle: bool,
    /// The model says the node cannot be selected (`Select::NotSelectable`).
    pub not_selectable: bool,
    /// The model says the node is a toggle button (a button with a pressed state).
    pub toggle_button: bool,
    /// The model says availability does not apply to the node (`Avail::NotApplicable`, SM-5).
    pub avail_na: bool,
    /// The UI language the classifiers' catalog words are taken in (the language `render` used).
    pub lang: Lang,
}

impl DiffCtx<'static> {
    /// A context with no special facts, for the native focus surface.
    pub const FOCUS: DiffCtx<'static> = DiffCtx {
        surface: "focus",
        role: SemRole::Unknown,
        name_derived: false,
        untoggled: false,
        not_toggle: false,
        not_selectable: false,
        toggle_button: false,
        avail_na: false,
        lang: Lang::En,
    };
}

impl<'a> DiffCtx<'a> {
    /// The facts of `entry` that decide which classifiers may apply.
    pub fn for_entry(surface: &'a str, entry: &SemEntry, lang: Lang) -> DiffCtx<'a> {
        DiffCtx {
            surface,
            role: entry.sem.role,
            name_derived: entry.prov.get(Group::Name) == Some(ProvCode::Derived),
            untoggled: entry.sem.press == Press::NotToggle
                && entry.sem.check == Check::NotCheckable,
            not_toggle: entry.sem.press == Press::NotToggle,
            not_selectable: entry.sem.select == Select::NotSelectable,
            toggle_button: entry.sem.role == SemRole::Button && entry.sem.press != Press::NotToggle,
            avail_na: entry.sem.avail == Avail::NotApplicable,
            lang,
        }
    }
}

type Key<'a> = (u8, &'a str);

fn keys_legacy(t: &[LegacyToken]) -> Vec<Key<'_>> {
    t.iter().map(|x| (kind_code(x.kind), &*x.text)).collect()
}

fn keys_new(t: &[Token]) -> Vec<Key<'_>> {
    t.iter()
        .map(|x| (kind_code(x.kind), x.text.as_str()))
        .collect()
}

/// Multiset difference of two sorted key lists: (only in `a`, only in `b`).
fn difference<'a>(a: &[Key<'a>], b: &[Key<'a>]) -> (Vec<Key<'a>>, Vec<Key<'a>>) {
    let (mut i, mut j) = (0, 0);
    let (mut only_a, mut only_b) = (Vec::new(), Vec::new());
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Equal => {
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => {
                only_a.push(a[i]);
                i += 1;
            }
            std::cmp::Ordering::Greater => {
                only_b.push(b[j]);
                j += 1;
            }
        }
    }
    only_a.extend_from_slice(&a[i..]);
    only_b.extend_from_slice(&b[j..]);
    (only_a, only_b)
}

/// Whether `surface` is one of the state-change surfaces.
fn is_change(surface: &str) -> bool {
    surface.starts_with("change")
}

fn count_kind(keys: &[Key<'_>], kind: SegmentKind) -> usize {
    keys.iter().filter(|(k, _)| *k == kind_code(kind)).count()
}

fn take(keys: &mut Vec<Key<'_>>, pred: impl Fn(&Key<'_>) -> bool) -> bool {
    match keys.iter().position(pred) {
        Some(i) => {
            keys.remove(i);
            true
        }
        None => false,
    }
}

/// True for the characters a bullet or dash marker is made of.
fn is_bullet_char(c: char) -> bool {
    matches!(
        c,
        '*' | '-'
            | '+'
            | '>'
            | '\u{2022}'
            | '\u{25E6}'
            | '\u{25AA}'
            | '\u{25AB}'
            | '\u{2023}'
            | '\u{00B7}'
            | '\u{2013}'
            | '\u{2014}'
            | '\u{25CF}'
            | '\u{25CB}'
            | '\u{25A0}'
            | '\u{25A1}'
            | '\u{2043}'
    )
}

/// `legacy` is `new` with a bullet in front: the removed prefix, trimmed, is bullet characters
/// only ("\u{2022} Alpha"). A number ("3. Beta") is information the name must keep, a dropped
/// word ("Delete all" to "all") is not a marker: both stay real differences.
fn is_marker_stripped(legacy: &str, new: &str) -> bool {
    if new.is_empty() {
        return false;
    }
    let Some(prefix) = legacy.strip_suffix(new) else {
        return false;
    };
    let marker = prefix.trim();
    !marker.is_empty() && marker.chars().all(is_bullet_char)
}

/// Try to explain part of the difference with `c`; true when it removed something.
fn explain<'a>(
    c: Classifier,
    ctx: &DiffCtx<'_>,
    only_legacy: &mut Vec<Key<'a>>,
    only_new: &mut Vec<Key<'a>>,
    legacy_all: &[Key<'a>],
    new_all: &[Key<'a>],
) -> bool {
    let kind = |k: SegmentKind| move |key: &Key<'_>| key.0 == kind_code(k);
    match c {
        Classifier::FieldContentsNotFused => {
            CONTENTS_ROLES.contains(&ctx.role)
                && count_kind(only_legacy, SegmentKind::Value) == 1
                && count_kind(new_all, SegmentKind::Value) == 0
                && take(only_legacy, kind(SegmentKind::Value))
        }
        Classifier::ContentDerivedName => {
            if !ctx.name_derived {
                return false;
            }
            let text = kind(SegmentKind::Text);
            if count_kind(legacy_all, SegmentKind::Text) == 0 {
                // The legacy snapshot had no name at all.
                count_kind(only_new, SegmentKind::Text) == 1 && take(only_new, text)
            } else {
                // The legacy name was a different join of the same content.
                let squash = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
                let text_code = kind_code(SegmentKind::Text);
                let pair = only_legacy.iter().position(|a| {
                    a.0 == text_code
                        && only_new
                            .iter()
                            .any(|b| b.0 == text_code && squash(a.1) == squash(b.1))
                });
                match pair {
                    Some(i) => {
                        let a = only_legacy.remove(i);
                        take(only_new, |b| b.0 == text_code && squash(a.1) == squash(b.1));
                        true
                    }
                    None => false,
                }
            }
        }
        Classifier::PressedWithoutToggle => {
            use gorca_i18n::MsgId as M;
            let (pressed, not_pressed) = (
                gorca_i18n::t_in(ctx.lang, M::StatePressed),
                gorca_i18n::t_in(ctx.lang, M::StateNotPressed),
            );
            let state = kind_code(SegmentKind::State);
            // Focus: a tab. A change: any node the model gives no pressed state (the engine
            // flipped PRESSED on a check box or a tab; the model keeps check and selection).
            let applies = if is_change(ctx.surface) {
                ctx.not_toggle
            } else {
                ctx.untoggled && ctx.role == SemRole::Tab
            };
            applies
                && take(only_legacy, |k| {
                    k.0 == state && (k.1 == pressed || k.1 == not_pressed)
                })
        }
        Classifier::MarkerStrippedName => {
            let text = kind_code(SegmentKind::Text);
            let pair = only_legacy.iter().position(|a| {
                a.0 == text
                    && only_new
                        .iter()
                        .any(|b| b.0 == text && is_marker_stripped(a.1, b.1))
            });
            match pair {
                Some(i) => {
                    let a = only_legacy.remove(i);
                    take(only_new, |b| b.0 == text && is_marker_stripped(a.1, b.1));
                    true
                }
                None => false,
            }
        }
        Classifier::NameWhitespace => {
            let text = kind_code(SegmentKind::Text);
            let collapsed = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
            let pair = only_legacy.iter().position(|a| {
                a.0 == text
                    && a.1 != collapsed(a.1)
                    && only_new
                        .iter()
                        .any(|b| b.0 == text && b.1 == collapsed(a.1))
            });
            match pair {
                Some(i) => {
                    let a = only_legacy.remove(i);
                    let want = collapsed(a.1);
                    take(only_new, |b| b.0 == text && b.1 == want)
                }
                None => false,
            }
        }
        Classifier::StaticTextWord => {
            let (label, text) = (
                gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::RoleLabel),
                gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::RoleStaticText),
            );
            let word =
                |k: &Key<'_>| k.0 == kind_code(SegmentKind::Role) && (k.1 == label || k.1 == text);
            matches!(ctx.role, SemRole::StaticText | SemRole::Caption)
                && only_legacy.iter().filter(|k| word(k)).count() == 1
                && only_new.iter().filter(|k| word(k)).count() == 1
                && take(only_legacy, word)
                && take(only_new, word)
        }
        Classifier::GroupingWord => {
            let panel = gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::RolePanel);
            let group = gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::AriaGroup);
            let role = kind_code(SegmentKind::Role);
            matches!(ctx.role, SemRole::Group | SemRole::RadioGroup)
                && only_legacy.iter().any(|k| k.0 == role && k.1 == panel)
                && only_new.iter().any(|k| k.0 == role && k.1 == group)
                && take(only_legacy, |k| k.0 == role && k.1 == panel)
                && take(only_new, |k| k.0 == role && k.1 == group)
        }
        Classifier::RoleNotInTable => {
            WEB_UNWORDED_ROLES.contains(&ctx.role)
                && count_kind(new_all, SegmentKind::Role) == 0
                && count_kind(only_legacy, SegmentKind::Role) == 1
                && take(only_legacy, kind(SegmentKind::Role))
        }
        Classifier::ValueFromAttrs => {
            VALUE_ROLES.contains(&ctx.role)
                && count_kind(legacy_all, SegmentKind::Value) == 0
                && count_kind(only_new, SegmentKind::Value) == 1
                && take(only_new, kind(SegmentKind::Value))
        }
        Classifier::UnavailableStatic => {
            use gorca_i18n::MsgId as M;
            let (un, av) = (
                gorca_i18n::t_in(ctx.lang, M::StateUnavailable),
                gorca_i18n::t_in(ctx.lang, M::StateAvailable),
            );
            let state = kind_code(SegmentKind::State);
            // Focus: a few static roles. A change: whatever the model says availability does not
            // apply to (SM-5: static structure is never "unavailable").
            let applies = if is_change(ctx.surface) {
                ctx.avail_na
            } else {
                matches!(
                    ctx.role,
                    SemRole::Generic | SemRole::Presentation | SemRole::Article
                )
            };
            applies && take(only_legacy, |k| k.0 == state && (k.1 == un || k.1 == av))
        }
        Classifier::ToggleStateWord => {
            use gorca_i18n::MsgId as M;
            let t = |id| gorca_i18n::t_in(ctx.lang, id);
            let state = kind_code(SegmentKind::State);
            if !(is_change(ctx.surface) && ctx.toggle_button) {
                return false;
            }
            // Same polarity: checked <-> pressed, not checked <-> not pressed.
            let pairs = [
                (t(M::StateChecked), t(M::StatePressed)),
                (t(M::StateNotChecked), t(M::StateNotPressed)),
            ];
            pairs.iter().any(|(l, n)| {
                only_legacy.iter().any(|k| k.0 == state && k.1 == *l)
                    && only_new.iter().any(|k| k.0 == state && k.1 == *n)
                    && take(only_legacy, |k| k.0 == state && k.1 == *l)
                    && take(only_new, |k| k.0 == state && k.1 == *n)
            })
        }
        Classifier::SelectNotApplicable => {
            use gorca_i18n::MsgId as M;
            let (sel, not_sel) = (
                gorca_i18n::t_in(ctx.lang, M::StateSelected),
                gorca_i18n::t_in(ctx.lang, M::StateNotSelected),
            );
            let state = kind_code(SegmentKind::State);
            ctx.not_selectable
                && take(only_legacy, |k| {
                    k.0 == state && (k.1 == sel || k.1 == not_sel)
                })
        }
        Classifier::MixedSwitchIgnored => {
            let half = gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::StateHalfPressed);
            let state = kind_code(SegmentKind::State);
            ctx.role == SemRole::Switch && take(only_legacy, |k| k.0 == state && k.1 == half)
        }
        Classifier::RoleWordImproved => {
            let role = kind_code(SegmentKind::Role);
            role_word_pairs(ctx.lang)
                .iter()
                .find(|(r, l, n)| {
                    *r == ctx.role
                        && only_legacy.iter().any(|k| k.0 == role && k.1 == *l)
                        && only_new.iter().any(|k| k.0 == role && k.1 == *n)
                })
                .is_some_and(|(_, l, n)| {
                    take(only_legacy, |k| k.0 == role && k.1 == *l)
                        && take(only_new, |k| k.0 == role && k.1 == *n)
                })
        }
        Classifier::NameCapDiffers => {
            let text = kind_code(SegmentKind::Text);
            let common =
                |a: &str, b: &str| a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count();
            let pair = only_legacy.iter().position(|a| {
                a.0 == text
                    && only_new
                        .iter()
                        .any(|b| b.0 == text && a.1 != b.1 && common(a.1, b.1) >= 200)
            });
            match pair {
                Some(i) => {
                    let a = only_legacy.remove(i);
                    take(only_new, |b| b.0 == text && common(a.1, b.1) >= 200);
                    true
                }
                None => false,
            }
        }
        Classifier::ReadOnlyFromModel => {
            let ro = gorca_i18n::t_in(ctx.lang, gorca_i18n::MsgId::StateReadOnly);
            matches!(
                ctx.role,
                SemRole::TextBox | SemRole::SearchBox | SemRole::PasswordBox | SemRole::Checkbox
            ) && take(only_new, |k| {
                k.0 == kind_code(SegmentKind::State) && k.1 == ro
            })
        }
        Classifier::RoleFromModel => {
            ROLE_FROM_MODEL_ROLES.contains(&ctx.role)
                && count_kind(legacy_all, SegmentKind::Role) == 0
                && count_kind(only_new, SegmentKind::Role) == 1
                && take(only_new, kind(SegmentKind::Role))
        }
    }
}

/// Compare the legacy tokens with the new ones. `allowed` decides which classifiers count on
/// `ctx.surface`; a difference is `Allowed` only when allowed classifiers explain all of it.
pub fn diff(
    legacy: &[LegacyToken],
    new: &[Token],
    allowed: &Deviations,
    ctx: DiffCtx<'_>,
) -> DiffOutcome {
    diff_report(legacy, new, allowed, ctx).0
}

/// Like [`diff`], and also every classifier that fired, in `Classifier::ALL` order (not only the
/// first). The `Allowed` id is the first of them. Several can fire on one comparison; the list is
/// what per-row hit counts must read.
pub fn diff_report(
    legacy: &[LegacyToken],
    new: &[Token],
    allowed: &Deviations,
    ctx: DiffCtx<'_>,
) -> (DiffOutcome, Vec<&'static str>) {
    let (la, lb) = (keys_legacy(legacy), keys_new(new));
    if la == lb {
        return (DiffOutcome::Equal, Vec::new());
    }
    let (mut sa, mut sb) = (la.clone(), lb.clone());
    sa.sort_unstable();
    sb.sort_unstable();
    if sa == sb {
        return (DiffOutcome::OrderOnly, Vec::new());
    }
    let (mut only_legacy, mut only_new) = difference(&sa, &sb);
    let mut used: Vec<&'static str> = Vec::new();
    for c in Classifier::ALL {
        if !allowed.allows(c.id(), ctx.surface) {
            continue;
        }
        if explain(c, &ctx, &mut only_legacy, &mut only_new, &sa, &sb) {
            used.push(c.id());
        }
    }
    let outcome = match used.first() {
        Some(id) if only_legacy.is_empty() && only_new.is_empty() => DiffOutcome::Allowed(id),
        Some(_) | None => DiffOutcome::Different {
            legacy_only: u32::try_from(only_legacy.len()).unwrap_or(u32::MAX),
            new_only: u32::try_from(only_new.len()).unwrap_or(u32::MAX),
        },
    };
    (outcome, used)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::render::TokenText;

    fn l(kind: SegmentKind, s: &str) -> LegacyToken {
        LegacyToken {
            kind,
            text: Arc::from(s),
        }
    }

    fn n(kind: SegmentKind, s: &str) -> Token {
        Token {
            kind,
            text: TokenText::Shared(Arc::from(s)),
        }
    }

    fn run(legacy: &[LegacyToken], new: &[Token]) -> DiffOutcome {
        diff(legacy, new, &Deviations::builtin(), DiffCtx::FOCUS)
    }

    #[test]
    fn equal_order_only_and_different() {
        let legacy = [l(SegmentKind::Text, "OK"), l(SegmentKind::Role, "button")];
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Role, "button")]
            ),
            DiffOutcome::Equal
        );
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Role, "button"), n(SegmentKind::Text, "OK")]
            ),
            DiffOutcome::OrderOnly
        );
        assert_eq!(
            run(&legacy, &[n(SegmentKind::Text, "OK")]),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
        // The kind is part of the token: the same words as another kind differ.
        assert_eq!(
            run(
                &legacy,
                &[n(SegmentKind::Text, "OK"), n(SegmentKind::Text, "button")]
            ),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 1
            }
        );
    }

    fn ctx(f: impl Fn(&mut DiffCtx<'static>)) -> DiffCtx<'static> {
        let mut c = DiffCtx::FOCUS;
        f(&mut c);
        c
    }

    fn d(legacy: &[LegacyToken], new: &[Token], c: DiffCtx<'_>) -> DiffOutcome {
        diff(legacy, new, &Deviations::builtin(), c)
    }

    #[test]
    fn a_field_contents_value_is_allowed_only_for_a_field_role() {
        let legacy = [l(SegmentKind::Text, "Name"), l(SegmentKind::Value, "Alice")];
        let new = [n(SegmentKind::Text, "Name")];
        let field = ctx(|c| c.role = SemRole::TextBox);
        assert_eq!(
            d(&legacy, &new, field),
            DiffOutcome::Allowed("field-contents-not-fused")
        );
        assert!(matches!(
            d(&legacy, &new, DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        // A list without the row, or another surface, allows nothing.
        assert!(matches!(
            diff(&legacy, &new, &Deviations::empty(), field),
            DiffOutcome::Different { .. }
        ));
        let browse = DiffCtx {
            surface: "browse",
            ..field
        };
        assert!(matches!(
            d(&legacy, &new, browse),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_range_value_the_fused_side_lacks_is_never_hidden() {
        // The connector puts a numeric range value into value_text: a missing one is a bug.
        let legacy = [l(SegmentKind::Text, "Vol"), l(SegmentKind::Value, "30")];
        assert!(matches!(
            d(&legacy, &[n(SegmentKind::Text, "Vol")], DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        assert!(matches!(
            d(
                &[l(SegmentKind::Value, "30")],
                &[n(SegmentKind::Value, "40")],
                DiffCtx::FOCUS
            ),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_content_derived_name_is_allowed_when_the_entry_says_derived() {
        let legacy = [l(SegmentKind::Role, "list item")];
        let new = [
            n(SegmentKind::Text, "Alpha"),
            n(SegmentKind::Role, "list item"),
        ];
        assert!(matches!(
            d(&legacy, &new, DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        let derived = ctx(|c| c.name_derived = true);
        assert_eq!(
            d(&legacy, &new, derived),
            DiffOutcome::Allowed("content-derived-name")
        );
        // A different join of the same content (other spacing) is explained too, but only when
        // the entry says the name was derived; a different name is not.
        let respaced = [
            l(SegmentKind::Text, "Al pha"),
            l(SegmentKind::Role, "list item"),
        ];
        assert_eq!(
            d(&respaced, &new, derived),
            DiffOutcome::Allowed("content-derived-name")
        );
        assert!(matches!(
            d(&respaced, &new, DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        let other = [
            l(SegmentKind::Text, "Beta"),
            l(SegmentKind::Role, "list item"),
        ];
        assert!(matches!(
            d(&other, &new, derived),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn pressed_without_toggle_and_role_from_model_combine() {
        let pressed = gorca_i18n::t(gorca_i18n::MsgId::StatePressed);
        let legacy = [
            l(SegmentKind::Text, "Tab 1"),
            l(SegmentKind::Role, "tab"),
            l(SegmentKind::State, pressed),
        ];
        let new = [n(SegmentKind::Text, "Tab 1"), n(SegmentKind::Role, "tab")];
        let c = ctx(|c| {
            c.untoggled = true;
            c.role = SemRole::Tab;
        });
        assert_eq!(
            d(&legacy, &new, c),
            DiffOutcome::Allowed("pressed-without-toggle")
        );
        // A role the legacy side did have but spells differently is a real difference.
        let legacy2 = [l(SegmentKind::Role, "label")];
        assert!(matches!(
            d(&legacy2, &[n(SegmentKind::Role, "text")], DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn the_role_and_contents_rows_are_gated_per_role() {
        // A role outside the list is never explained by the row, whatever word it gets.
        let new = [n(SegmentKind::Role, "slider")];
        let btn = ctx(|c| c.role = SemRole::Button);
        assert!(matches!(d(&[], &new, btn), DiffOutcome::Different { .. }));
        let sl = ctx(|c| c.role = SemRole::Slider);
        assert_eq!(d(&[], &new, sl), DiffOutcome::Allowed("role-from-model"));
        // A spin button's value is not field contents: a missing one is a difference.
        let legacy = [l(SegmentKind::Value, "5")];
        let spin = ctx(|c| c.role = SemRole::SpinButton);
        assert!(matches!(
            d(&legacy, &[], spin),
            DiffOutcome::Different { .. }
        ));
        // Pressed-without-toggle is for tabs only.
        let pressed = gorca_i18n::t(gorca_i18n::MsgId::StatePressed);
        let btn = ctx(|c| {
            c.untoggled = true;
            c.role = SemRole::Button;
        });
        assert!(matches!(
            d(&[l(SegmentKind::State, pressed)], &[], btn),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn the_narrow_web_rows_are_gated_per_cause() {
        let un = gorca_i18n::t(gorca_i18n::MsgId::StateUnavailable);
        let leg = [l(SegmentKind::Role, "panel"), l(SegmentKind::State, un)];
        let new = [n(SegmentKind::Role, "panel")];
        let web = |role| DiffCtx {
            surface: "focus-web",
            role,
            ..DiffCtx::FOCUS
        };
        let gen_ = web(SemRole::Generic);
        assert_eq!(
            d(&leg, &new, gen_),
            DiffOutcome::Allowed("unavailable-static")
        );
        let btn = web(SemRole::Button);
        assert!(matches!(d(&leg, &new, btn), DiffOutcome::Different { .. }));
        // Role word pairs only for their own role.
        let l2 = [l(SegmentKind::Role, "panel")];
        let n2 = [n(SegmentKind::Role, "list")];
        let feed = web(SemRole::Feed);
        assert_eq!(
            d(&l2, &n2, feed),
            DiffOutcome::Allowed("role-word-improved")
        );
        assert!(matches!(d(&l2, &n2, btn), DiffOutcome::Different { .. }));
        // Names that agree for 200 characters and then differ in length.
        let long_a = "x".repeat(260);
        let long_b = "x".repeat(300);
        assert_eq!(
            d(
                &[l(SegmentKind::Text, &long_a)],
                &[n(SegmentKind::Text, &long_b)],
                btn
            ),
            DiffOutcome::Allowed("name-cap-differs")
        );
        let short_a = "x".repeat(20);
        let short_b = "y".repeat(20);
        assert!(matches!(
            d(
                &[l(SegmentKind::Text, &short_a)],
                &[n(SegmentKind::Text, &short_b)],
                DiffCtx::FOCUS
            ),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn the_change_rows_are_gated_by_what_the_model_says() {
        use gorca_i18n::MsgId as M;
        let t = gorca_i18n::t;
        let ch = |f: &dyn Fn(&mut DiffCtx<'static>)| {
            let mut c = DiffCtx {
                surface: "change-web",
                ..DiffCtx::FOCUS
            };
            f(&mut c);
            c
        };
        let none: [Token; 0] = [];
        let un = [l(SegmentKind::State, t(M::StateUnavailable))];
        // Availability does not apply: allowed; applies: a real difference.
        assert_eq!(
            d(&un, &none, ch(&|c| c.avail_na = true)),
            DiffOutcome::Allowed("unavailable-static")
        );
        assert!(matches!(
            d(&un, &none, ch(&|_| ())),
            DiffOutcome::Different { .. }
        ));
        // Pressed on a node without a pressed state; not pressed too; a toggle is a difference.
        let pr = [l(SegmentKind::State, t(M::StatePressed))];
        assert_eq!(
            d(&pr, &none, ch(&|c| c.not_toggle = true)),
            DiffOutcome::Allowed("pressed-without-toggle")
        );
        let npr = [l(SegmentKind::State, t(M::StateNotPressed))];
        assert_eq!(
            d(&npr, &none, ch(&|c| c.not_toggle = true)),
            DiffOutcome::Allowed("pressed-without-toggle")
        );
        assert!(matches!(
            d(&pr, &none, ch(&|_| ())),
            DiffOutcome::Different { .. }
        ));
        // Selection on a node that cannot be selected.
        let sel = [l(SegmentKind::State, t(M::StateSelected))];
        assert_eq!(
            d(&sel, &none, ch(&|c| c.not_selectable = true)),
            DiffOutcome::Allowed("select-not-applicable")
        );
        assert!(matches!(
            d(&sel, &none, ch(&|_| ())),
            DiffOutcome::Different { .. }
        ));
        // A mixed switch.
        let half = [l(SegmentKind::State, t(M::StateHalfPressed))];
        assert_eq!(
            d(&half, &none, ch(&|c| c.role = SemRole::Switch)),
            DiffOutcome::Allowed("mixed-switch-ignored")
        );
        assert!(matches!(
            d(&half, &none, ch(&|c| c.role = SemRole::Button)),
            DiffOutcome::Different { .. }
        ));
        // The same facts on the focus surface do not widen the focus rows.
        let focus_web = DiffCtx {
            surface: "focus-web",
            avail_na: true,
            ..DiffCtx::FOCUS
        };
        assert!(matches!(
            d(&un, &none, focus_web),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn a_toggle_buttons_checked_is_the_models_pressed_on_the_change_surface_only() {
        use gorca_i18n::MsgId as M;
        let t = gorca_i18n::t;
        let toggle = |surface: &'static str| DiffCtx {
            surface,
            toggle_button: true,
            ..DiffCtx::FOCUS
        };
        let legacy = [l(SegmentKind::State, t(M::StateChecked))];
        let new = [n(SegmentKind::State, t(M::StatePressed))];
        assert_eq!(
            d(&legacy, &new, toggle("change")),
            DiffOutcome::Allowed("toggle-state-word")
        );
        let off = [l(SegmentKind::State, t(M::StateNotChecked))];
        let off_new = [n(SegmentKind::State, t(M::StateNotPressed))];
        assert_eq!(
            d(&off, &off_new, toggle("change")),
            DiffOutcome::Allowed("toggle-state-word")
        );
        // Opposite polarity, another surface, or a node that is no toggle: a real difference.
        assert!(matches!(
            d(&legacy, &off_new, toggle("change")),
            DiffOutcome::Different { .. }
        ));
        assert!(matches!(
            d(&legacy, &new, toggle("focus")),
            DiffOutcome::Different { .. }
        ));
        assert!(matches!(
            d(
                &legacy,
                &new,
                DiffCtx {
                    surface: "change",
                    ..DiffCtx::FOCUS
                }
            ),
            DiffOutcome::Different { .. }
        ));
    }

    #[test]
    fn collapsed_white_space_in_a_name_is_allowed_and_a_changed_word_is_not() {
        let legacy = [l(SegmentKind::Text, "\u{2022}  ")];
        let new = [n(SegmentKind::Text, "\u{2022}")];
        assert_eq!(
            d(&legacy, &new, DiffCtx::FOCUS),
            DiffOutcome::Allowed("name-whitespace")
        );
        let other = [n(SegmentKind::Text, "Alpha")];
        assert!(matches!(
            d(&legacy, &other, DiffCtx::FOCUS),
            DiffOutcome::Different { .. }
        ));
        // Equal text is not a deviation at all.
        let same = [l(SegmentKind::Text, "\u{2022}")];
        assert_eq!(d(&same, &new, DiffCtx::FOCUS), DiffOutcome::Equal);
    }

    #[test]
    fn duplicates_count_as_a_multiset() {
        let legacy = [l(SegmentKind::State, "x"), l(SegmentKind::State, "x")];
        assert_eq!(
            run(&legacy, &[n(SegmentKind::State, "x")]),
            DiffOutcome::Different {
                legacy_only: 1,
                new_only: 0
            }
        );
    }
}
