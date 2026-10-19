// SPDX-License-Identifier: GPL-3.0-or-later
//! Legacy agreement (ADR-0022 section 2.11, M22a-C1b): while the model is dual-carried the legacy
//! fields of a snapshot (`role`, `states`, `name`, `description`) and the fused entry made from
//! the same raw data must say the same thing, group by group. A difference is either a bug in
//! one of the two normalisers or an intended improvement of the new one; the latter are listed
//! in [`DEVIATIONS`] with the reason and stop counting. Everything else is a disagreement: counted
//! (`agreement_counters`), logged with the node and group (never the text), and a failure in the
//! tests (`debug_assert`).
//!
//! Compiled into debug builds and into builds with the `legacy-agree` feature (CI e2e); the
//! release reader never runs it.

use std::sync::atomic::{AtomicU64, Ordering};

use gorca_model::sem::{
    Avail, Check, Expand, Group, Invalid, Press, ProvCode, RoleSource, Select, SemEntry, SemRole,
    SrcRecord,
};
use gorca_model::{NodeSnapshot, PropMask, Role, StateSet};

/// Checks made since start, process wide.
static CHECKED: AtomicU64 = AtomicU64::new(0);
/// Disagreements outside [`DEVIATIONS`] since start, process wide.
static MISMATCH: AtomicU64 = AtomicU64::new(0);
/// Differences excused by a documented deviation since start, process wide.
static DEVIATED: AtomicU64 = AtomicU64::new(0);

pub(super) fn note_checked(n: u64) {
    CHECKED.fetch_add(n, Ordering::Relaxed);
}

pub(super) fn note_mismatch() {
    MISMATCH.fetch_add(1, Ordering::Relaxed);
}

/// `(checked, mismatched, deviated)` since process start: the CI counter of M22a-C1b. The e2e
/// harness reads the `legacy agreement` warnings of the reader log; unit tests read this.
pub fn agreement_counters() -> (u64, u64, u64) {
    (
        CHECKED.load(Ordering::Relaxed),
        MISMATCH.load(Ordering::Relaxed),
        DEVIATED.load(Ordering::Relaxed),
    )
}

/// One group where legacy and fused differ outside the deviations. Values are the short names of
/// the compared values (`"on"`, `"expanded"`, `"len=12"`): never page text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Mismatch {
    pub group: Group,
    pub legacy: String,
    pub fused: String,
}

/// The result of checking one snapshot.
#[derive(Debug, Default)]
pub struct Report {
    /// Groups compared.
    pub checked: u64,
    /// Differences excused by a deviation (its id).
    pub deviated: Vec<(Group, &'static str)>,
    pub mismatches: Vec<Mismatch>,
}

/// An intended difference between the legacy and the new normaliser, with the reason. A row is
/// matched by [`applies`]; adding one needs a reason a reviewer can check against `docs/`.
#[derive(Copy, Clone, Debug)]
pub struct Deviation {
    pub id: &'static str,
    pub group: Group,
    pub reason: &'static str,
}

/// What a deviation predicate sees of one difference.
struct Ctx<'a> {
    snap: &'a NodeSnapshot,
    rec: &'a SrcRecord,
    entry: &'a SemEntry,
}

/// The documented deviations. Each has a predicate in [`applies`] and is hit by the corpus test
/// (`gorca-connector-atspi`, `legacy_agreement_tests`), which prints the counts, so a row cannot
/// outlive its cause unnoticed.
pub const DEVIATIONS: &[Deviation] = &[
    Deviation {
        id: "gecko-list-marker",
        group: Group::Name,
        reason: "the engine's name of a list or menu item carries the CSS marker (\"• Alpha\") or the counter \
                 of an ordered list (\"1. First\"); the S-1 quirk rows strip them (the Reader's overlay takes the stripped name from the document entry; M22a-C3 removed the old role-gated branch)",
    },
    Deviation {
        id: "name-from-content",
        group: Group::Name,
        reason: "the legacy snapshot has no name; S3 derives one from the content (accname 1.2 \
                 step 2F), which replaced the Reader's `subtree_label` (M22a-C3)",
    },
    Deviation {
        id: "aria-role-over-engine",
        group: Group::Role,
        reason: "the author's ARIA role token wins over the engine role (ADR-0022 2.1 engine-vs-ARIA \
                 rule); the legacy mapping only knows the engine role",
    },
    Deviation {
        id: "weak-role-refined",
        group: Group::Role,
        reason: "the legacy role is a weak one (panel, label, text, unknown) and the model role is \
                 the more specific one the engine role, the DOM tag or the author's role gives",
    },
    Deviation {
        id: "layout-table-presentation",
        group: Group::Role,
        reason: "a table the engine guesses to be layout is `presentation` in the model",
    },
    Deviation {
        id: "select-not-applicable",
        group: Group::Select,
        reason: "the engine sets SELECTED on a role that has no selection state in ARIA (menu \
                 item focus); the model says `not selectable` and the legacy word never applied",
    },
    Deviation {
        id: "pressed-without-toggle",
        group: Group::Check,
        reason: "the engine sets PRESSED or CHECKED on a role that is no toggle (GTK page tabs); \
                 the model carries that as `selected`, the legacy flag was spoken as \"pressed\"",
    },
];

/// The CSS list markers the engine puts in front of a list item's name (the Gecko quirk row).
const MARKERS: [&str; 10] = ["• ", "◦ ", "▪ ", "■ ", "□ ", "– ", "● ", "○ ", "‣ ", "⁃ "];

/// The counter of an ordered list item (`1. `, `12) `) in front of a name.
fn strip_ordinal(s: &str) -> Option<&str> {
    let rest = s.trim_start_matches(|c: char| c.is_ascii_digit());
    if rest.len() == s.len() {
        return None;
    }
    rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") "))
}

/// Does deviation `id` explain this difference?
fn applies(id: &str, c: &Ctx<'_>) -> bool {
    match id {
        "gecko-list-marker" => {
            let (Some(l), Some(f)) = (c.snap.name.as_deref(), c.entry.name.as_deref()) else {
                return false;
            };
            MARKERS
                .iter()
                .any(|m| l.strip_prefix(m).is_some_and(|rest| clip1k(rest) == f))
                || strip_ordinal(l).is_some_and(|rest| clip1k(rest) == f)
        }
        "name-from-content" => {
            c.snap.name.as_deref().is_none_or(str::is_empty)
                && matches!(
                    c.entry.prov.get(Group::Name),
                    Some(ProvCode::Derived | ProvCode::DerivedTainted)
                )
        }
        "aria-role-over-engine" => c.rec.role_source == RoleSource::Token,
        "weak-role-refined" => weak(c.snap.role),
        "layout-table-presentation" => c.entry.sem.role == SemRole::Presentation,
        "select-not-applicable" => {
            c.snap.states.contains(StateSet::SELECTED)
                && c.entry.sem.select == Select::NotSelectable
        }
        "pressed-without-toggle" => {
            c.entry.sem.check == Check::NotCheckable && c.entry.sem.press == Press::NotToggle
        }
        _ => false,
    }
}

/// The legacy role a model role stands for (what the legacy mapping gives for the engine role the
/// model role comes from). `Unknown` for model roles the legacy set has no word for.
pub fn legacy_family(sem: SemRole) -> Role {
    use SemRole as S;
    match sem {
        S::Application => Role::Application,
        S::Window => Role::Window,
        S::Dialog | S::AlertDialog => Role::Dialog,
        S::Alert => Role::Alert,
        S::Status => Role::StatusBar,
        S::Generic | S::Group | S::Presentation => Role::Panel,
        S::Document => Role::Document,
        S::DocumentWeb => Role::DocumentWeb,
        S::Heading => Role::Heading,
        S::Paragraph => Role::Paragraph,
        S::Link => Role::Link,
        S::Img => Role::Image,
        S::List | S::Listbox | S::Feed | S::DescriptionList => Role::List,
        S::ListItem | S::Option | S::Term | S::Definition => Role::ListItem,
        S::Table | S::Grid | S::TreeGrid => Role::Table,
        S::Row => Role::Row,
        S::Cell | S::GridCell => Role::Cell,
        S::ColumnHeader => Role::ColumnHeader,
        S::RowHeader => Role::RowHeader,
        S::Button => Role::Button,
        S::Switch => Role::ToggleButton,
        S::Checkbox | S::MenuItemCheckbox => Role::CheckBox,
        S::Radio | S::MenuItemRadio => Role::RadioButton,
        S::Combobox => Role::ComboBox,
        S::TextBox | S::SearchBox => Role::TextEntry,
        S::PasswordBox => Role::PasswordEntry,
        S::SpinButton => Role::SpinButton,
        S::Slider | S::Scrollbar => Role::Slider,
        S::ProgressBar | S::Meter => Role::ProgressBar,
        S::Menu => Role::Menu,
        S::Menubar => Role::MenuBar,
        S::MenuItem => Role::MenuItem,
        S::Popover | S::Tooltip => Role::Popover,
        S::Tab => Role::Tab,
        S::TabList => Role::TabList,
        S::Tree => Role::Tree,
        S::TreeItem => Role::TreeItem,
        S::Toolbar => Role::Toolbar,
        S::Separator => Role::Separator,
        S::Caption => Role::Label,
        S::StaticText => Role::StaticText,
        S::Terminal => Role::Terminal,
        S::Region
        | S::Banner
        | S::Complementary
        | S::ContentInfo
        | S::Main
        | S::Navigation
        | S::Search
        | S::Article => Role::Landmark,
        // The legacy set has no word for these: an unlisted new role must be placed here.
        S::Unknown
        | S::Blockquote
        | S::Code
        | S::Deletion
        | S::Figure
        | S::Form
        | S::Insertion
        | S::Log
        | S::Mark
        | S::Marquee
        | S::Math
        | S::Note
        | S::RadioGroup
        | S::RowGroup
        | S::TabPanel
        | S::Timer
        | S::Comment
        | S::Suggestion
        | S::Details
        | S::Summary
        | S::Time
        | S::Iframe
        | S::Emphasis
        | S::Strong
        | S::Subscript
        | S::Superscript => Role::Unknown,
    }
}

/// Legacy roles that carry little meaning of their own: an ARIA role or a DOM tag may make the
/// model role more specific without the two disagreeing.
const fn weak(role: Role) -> bool {
    matches!(
        role,
        Role::Unknown | Role::Panel | Role::StaticText | Role::Label | Role::Paragraph
    )
}

/// Legacy roles that are one thing in the model (a toggle button is a button with a pressed
/// state; a label and a static text are both text).
const fn equivalent(a: Role, b: Role) -> bool {
    matches!(
        (a, b),
        (Role::Label, Role::StaticText)
            | (Role::StaticText, Role::Label)
            | (Role::Button, Role::ToggleButton)
            | (Role::ToggleButton, Role::Button)
    )
}

fn clip1k(s: &str) -> &str {
    let mut end = s.len().min(1024);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// The kind of a text value for a report: its length, never its content.
fn len_of(s: Option<&str>) -> String {
    s.map_or_else(|| "none".to_owned(), |s| format!("text len={}", s.len()))
}

/// Compare the legacy fields of `snap` with `entry` (fused from the record `rec` of that very
/// snapshot). Only groups the snapshot fetched *and* the entry knows are compared.
pub fn check(snap: &NodeSnapshot, rec: &SrcRecord, entry: &SemEntry) -> Report {
    let mut rep = Report::default();
    let diff = |rep: &mut Report, g: Group, legacy: String, fused: String| {
        rep.checked += 1;
        if legacy == fused {
            return;
        }
        if let Some(d) = excuse(snap, rec, entry, g) {
            rep.deviated.push((g, d.id));
            DEVIATED.fetch_add(1, Ordering::Relaxed);
        } else {
            rep.mismatches.push(Mismatch {
                group: g,
                legacy,
                fused,
            });
        }
    };
    let have = |m: PropMask| snap.have.contains(m);
    let st = snap.states;

    if have(PropMask::ROLE) && entry.known.has(Group::Role) {
        let fam = legacy_family(entry.sem.role);
        let ok = fam == snap.role || equivalent(fam, snap.role);
        let (l, f) = if ok {
            (String::new(), String::new())
        } else {
            (format!("{:?}", snap.role), entry.sem.role.name().to_owned())
        };
        diff(&mut rep, Group::Role, l, f);
    }
    if have(PropMask::NAME) && entry.known.has(Group::Name) {
        let legacy = snap.name.as_deref().map(clip1k).filter(|s| !s.is_empty());
        let fused = entry.name.as_deref().filter(|s| !s.is_empty());
        let (l, f) = if legacy == fused {
            (String::new(), String::new())
        } else {
            (len_of(legacy), len_of(fused))
        };
        diff(&mut rep, Group::Name, l, f);
    }
    if have(PropMask::DESCRIPTION) && entry.known.has(Group::Description) {
        let legacy = snap
            .description
            .as_deref()
            .map(clip1k)
            .filter(|s| !s.is_empty());
        let fused = entry.sem.description().filter(|s| !s.is_empty());
        let (l, f) = if legacy == fused {
            (String::new(), String::new())
        } else {
            (len_of(legacy), len_of(fused))
        };
        diff(&mut rep, Group::Description, l, f);
    }
    if have(PropMask::STATES) {
        // Checked-ness: the legacy set has one `on` (CHECKED or PRESSED) and one `mixed`.
        if entry.known.has(Group::Check) && entry.known.has(Group::Press) {
            let legacy = if st.contains(StateSet::MIXED) {
                "mixed"
            } else if st.intersects(StateSet::CHECKED | StateSet::PRESSED) {
                "on"
            } else {
                "off"
            };
            let fused = if entry.sem.check == Check::Mixed || entry.sem.press == Press::Mixed {
                "mixed"
            } else if entry.sem.check == Check::On || entry.sem.press == Press::On {
                "on"
            } else {
                "off"
            };
            diff(&mut rep, Group::Check, legacy.into(), fused.into());
        }
        if entry.known.has(Group::Expand) {
            let legacy = if st.contains(StateSet::EXPANDED) {
                "expanded"
            } else if st.contains(StateSet::COLLAPSED) {
                "collapsed"
            } else {
                "na"
            };
            let fused = match entry.sem.expand {
                Expand::Expanded => "expanded",
                Expand::Collapsed => "collapsed",
                Expand::NotExpandable => "na",
            };
            diff(&mut rep, Group::Expand, legacy.into(), fused.into());
        }
        if entry.known.has(Group::Select) {
            let legacy = st.contains(StateSet::SELECTED);
            let fused = entry.sem.select == Select::On;
            diff(
                &mut rep,
                Group::Select,
                legacy.to_string(),
                fused.to_string(),
            );
        }
        if entry.known.has(Group::Avail) && entry.sem.avail != Avail::NotApplicable {
            let legacy = st.contains(StateSet::DISABLED);
            let fused = entry.sem.avail == Avail::Disabled;
            diff(
                &mut rep,
                Group::Avail,
                legacy.to_string(),
                fused.to_string(),
            );
        }
        if entry.known.has(Group::Invalid) {
            let legacy = st.contains(StateSet::INVALID);
            let fused = entry.sem.invalid != Invalid::No;
            diff(
                &mut rep,
                Group::Invalid,
                legacy.to_string(),
                fused.to_string(),
            );
        }
    }
    rep
}

/// The deviation that explains a difference in `group`, if any (see [`DEVIATIONS`]).
fn excuse(
    snap: &NodeSnapshot,
    rec: &SrcRecord,
    entry: &SemEntry,
    group: Group,
) -> Option<&'static Deviation> {
    let c = Ctx { snap, rec, entry };
    DEVIATIONS
        .iter()
        .find(|d| d.group == group && applies(d.id, &c))
}
