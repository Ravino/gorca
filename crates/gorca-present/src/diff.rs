// SPDX-License-Identifier: GPL-3.0-or-later
//! The shadow diff (ADR-0022 section 2.11, M-13): token multisets of the legacy presenter and
//! of `gorca-present`, compared as (kind, literal).

use gorca_model::SegmentKind;
use gorca_model::sem::{Check, Group, Press, ProvCode, SemEntry, SemRole};

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
    /// Static text: the engine's `label` and `static` roles are one model role.
    StaticTextWord,
    /// A fieldset or group: the model says "grouping" where the legacy mapping said "panel".
    GroupingWord,
    /// A role the web table has no word for: the legacy mapping spoke one from the engine role.
    RoleNotInTable,
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

impl Classifier {
    pub const ALL: [Classifier; 10] = [
        Classifier::FieldContentsNotFused,
        Classifier::ContentDerivedName,
        Classifier::PressedWithoutToggle,
        Classifier::RoleFromModel,
        Classifier::MarkerStrippedName,
        Classifier::StaticTextWord,
        Classifier::GroupingWord,
        Classifier::RoleNotInTable,
        Classifier::ValueFromAttrs,
        Classifier::ReadOnlyFromModel,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Classifier::FieldContentsNotFused => "field-contents-not-fused",
            Classifier::ContentDerivedName => "content-derived-name",
            Classifier::PressedWithoutToggle => "pressed-without-toggle",
            Classifier::RoleFromModel => "role-from-model",
            Classifier::MarkerStrippedName => "marker-stripped-name",
            Classifier::StaticTextWord => "static-text-word",
            Classifier::GroupingWord => "grouping-word",
            Classifier::RoleNotInTable => "role-not-in-table",
            Classifier::ValueFromAttrs => "value-from-attrs",
            Classifier::ReadOnlyFromModel => "read-only-from-model",
        }
    }
}

/// What the diff needs to know about the comparison besides the tokens.
#[derive(Copy, Clone, Debug)]
pub struct DiffCtx<'a> {
    /// The surface being compared (`deviations.toml` `surface`).
    pub surface: &'a str,
    /// The fused role of the node: the per-role gates of the classifiers read it.
    pub role: SemRole,
    /// The fused name was derived from content (`ProvCode::Derived`).
    pub name_derived: bool,
    /// The model says the node is neither checkable nor a toggle.
    pub untoggled: bool,
}

impl DiffCtx<'static> {
    /// A context with no special facts, for the native focus surface.
    pub const FOCUS: DiffCtx<'static> = DiffCtx {
        surface: "focus",
        role: SemRole::Unknown,
        name_derived: false,
        untoggled: false,
    };
}

impl<'a> DiffCtx<'a> {
    /// The facts of `entry` that decide which classifiers may apply.
    pub fn for_entry(surface: &'a str, entry: &SemEntry) -> DiffCtx<'a> {
        DiffCtx {
            surface,
            role: entry.sem.role,
            name_derived: entry.prov.get(Group::Name) == Some(ProvCode::Derived),
            untoggled: entry.sem.press == Press::NotToggle
                && entry.sem.check == Check::NotCheckable,
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
                count_kind(only_legacy, SegmentKind::Text) == 1
                    && count_kind(only_new, SegmentKind::Text) == 1
                    && take(only_legacy, text)
                    && take(only_new, text)
            }
        }
        Classifier::PressedWithoutToggle => {
            let pressed = gorca_i18n::t(gorca_i18n::MsgId::StatePressed);
            ctx.untoggled
                && ctx.role == SemRole::Tab
                && take(only_legacy, |k| {
                    k.0 == kind_code(SegmentKind::State) && k.1 == pressed
                })
        }
        Classifier::MarkerStrippedName => {
            let text = kind_code(SegmentKind::Text);
            let pair = only_legacy.iter().position(|a| {
                a.0 == text
                    && only_new
                        .iter()
                        .any(|b| b.0 == text && a.1.len() > b.1.len() && a.1.ends_with(b.1))
            });
            match pair {
                Some(i) => {
                    let a = only_legacy.remove(i);
                    take(only_new, |b| b.0 == text && a.1.ends_with(b.1));
                    true
                }
                None => false,
            }
        }
        Classifier::StaticTextWord => {
            let (label, text) = (
                gorca_i18n::t(gorca_i18n::MsgId::RoleLabel),
                gorca_i18n::t(gorca_i18n::MsgId::RoleStaticText),
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
            let panel = gorca_i18n::t(gorca_i18n::MsgId::RolePanel);
            let group = gorca_i18n::t(gorca_i18n::MsgId::AriaGroup);
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
        Classifier::ReadOnlyFromModel => {
            let ro = gorca_i18n::t(gorca_i18n::MsgId::StateReadOnly);
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
    let (la, lb) = (keys_legacy(legacy), keys_new(new));
    if la == lb {
        return DiffOutcome::Equal;
    }
    let (mut sa, mut sb) = (la.clone(), lb.clone());
    sa.sort_unstable();
    sb.sort_unstable();
    if sa == sb {
        return DiffOutcome::OrderOnly;
    }
    let (mut only_legacy, mut only_new) = difference(&sa, &sb);
    let mut used: Option<&'static str> = None;
    for c in Classifier::ALL {
        if !allowed.allows(c.id(), ctx.surface) {
            continue;
        }
        if explain(c, &ctx, &mut only_legacy, &mut only_new, &sa, &sb) {
            used.get_or_insert(c.id());
        }
    }
    match used {
        Some(id) if only_legacy.is_empty() && only_new.is_empty() => DiffOutcome::Allowed(id),
        Some(_) | None => DiffOutcome::Different {
            legacy_only: u32::try_from(only_legacy.len()).unwrap_or(u32::MAX),
            new_only: u32::try_from(only_new.len()).unwrap_or(u32::MAX),
        },
    }
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
        // A different join of the same content (the legacy name had other spacing) is explained
        // by derivation too, but only when the entry says the name was derived.
        let legacy2 = [
            l(SegmentKind::Text, "Beta"),
            l(SegmentKind::Role, "list item"),
        ];
        assert_eq!(
            d(&legacy2, &new, derived),
            DiffOutcome::Allowed("content-derived-name")
        );
        assert!(matches!(
            d(&legacy2, &new, DiffCtx::FOCUS),
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
