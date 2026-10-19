// SPDX-License-Identifier: GPL-3.0-or-later
//! Legacy agreement (M22a spec section 5.5, dual-carry): while presentation still reads the
//! legacy node values, every node the translator builds is compared, per group, with the entry
//! the fusion stage produced for it. A disagreement is a bug in one of the two paths or a
//! documented deviation; the counters are the CI gate ("0 disagreements outside
//! `deviations`"), and a debug build asserts on a disagreement that is not a deviation.
//!
//! The comparison is deliberately one-directional where the new model knows more: a node whose
//! legacy name is empty may get a content name (S3), that is the feature, not a disagreement.

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use gorca_doc::web::NodeFlags;
use gorca_fuse::Rules;
use gorca_model::sem::{Check, Expand, Group, Select, SemEntry, SemRole};
use gorca_model::{Role, SrcNode, StateSet};

/// What is compared.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Aspect {
    Role,
    Name,
    Level,
    Live,
    Check,
    Expand,
    Select,
    /// The live-region properties of a region root (politeness, atomic, removals), the live
    /// tracker's own parse against the fused entry.
    LiveProps,
    /// The structural flags the translator derives from the DOM tag and the role tokens
    /// (article, blockquote, region, listbox, annotation) against the fused role.
    Structure,
}

impl Aspect {
    pub const fn label(self) -> &'static str {
        match self {
            Aspect::Role => "role",
            Aspect::Name => "name",
            Aspect::Level => "level",
            Aspect::Live => "live",
            Aspect::Check => "check",
            Aspect::Expand => "expand",
            Aspect::Select => "select",
            Aspect::LiveProps => "live-props",
            Aspect::Structure => "structure",
        }
    }

    const fn ix(self) -> usize {
        self as usize
    }
}

static CHECKED: AtomicU64 = AtomicU64::new(0);
static MISMATCH: [AtomicU64; ASPECTS] = [const { AtomicU64::new(0) }; ASPECTS];
const ASPECTS: usize = 9;
/// Mismatches logged so far (the log is capped: a systematic difference must not flood).
static LOGGED: AtomicU64 = AtomicU64::new(0);
const LOG_CAP: u64 = 256;

/// Process-wide counters, for tests, the debug probe and the CI gate.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct AgreementStats {
    /// Nodes compared.
    pub checked: u64,
    /// Disagreements by [`Aspect`] (index = `Aspect as usize`).
    pub mismatched: [u64; ASPECTS],
}

impl AgreementStats {
    pub fn total(&self) -> u64 {
        self.mismatched.iter().sum()
    }

    pub fn of(&self, a: Aspect) -> u64 {
        self.mismatched[a.ix()]
    }
}

pub fn stats() -> AgreementStats {
    let mut m = [0u64; ASPECTS];
    for (o, c) in m.iter_mut().zip(&MISMATCH) {
        *o = c.load(Ordering::Relaxed);
    }
    AgreementStats {
        checked: CHECKED.load(Ordering::Relaxed),
        mismatched: m,
    }
}

/// Known, accepted differences, each with its reason. They are built into [`compare`] (the
/// value is not compared in that case) and listed here for the record and for the CI report;
/// a difference that is not described here is a bug in one of the two paths.
///
/// * `avail`: not compared. The legacy web path has no unavailable state (`DISABLED` is never
///   set from AT-SPI for a web node); the model's `avail` is new information.
/// * `name`: a legacy name outside list and tree items keeps the CSS list marker the S-1 row
///   strips ("• New" on a `role=menuitem` list item); the row covers option and menu items
///   too, which is what Blink exports.
/// * `level`: the legacy level is clamped to 1..=6 (heading levels); `aria-level` is not.
/// * `live`: the legacy flag marks region roots and the status/log/marquee/timer tokens (also
///   `live=off`), and leaves `alert` to its role; the model also carries inherited container
///   values (`root` false), which are not region roots.
/// * `structure`: compared one way (a legacy article, blockquote, region, listbox or annotation
///   flag needs the matching fused role); the model also maps `role=blockquote` on any element
///   and the DPUB `doc-*` landmarks to `region`, which the legacy flags do not mark.
/// * `check`, `expand`, `select`: compared only where the fused role carries the group (repair
///   `R`, SM-5); the legacy flag is raw engine noise on other roles (an `option` exported
///   with `checked`, a select-element `menuitem` with `selected`).
pub const DEVIATIONS: &[(Aspect, &str)] = &[];

pub(super) fn deviation(a: Aspect) -> bool {
    DEVIATIONS.iter().any(|(d, _)| *d == a)
}

/// The legacy role against the fused role, for the roles whose mapping is one to one. `None`:
/// not compared (the legacy role is coarse or the model has no counterpart yet).
fn role_agrees(legacy: Role, sem: SemRole) -> Option<bool> {
    use SemRole as S;
    let ok: &[SemRole] = match legacy {
        Role::Heading => &[S::Heading],
        Role::Link => &[S::Link],
        Role::Button | Role::ToggleButton => &[S::Button, S::Switch],
        Role::CheckBox => &[S::Checkbox, S::Switch, S::MenuItemCheckbox],
        Role::RadioButton => &[S::Radio, S::MenuItemRadio],
        Role::ComboBox => &[S::Combobox],
        Role::TextEntry => &[S::TextBox, S::SearchBox],
        Role::PasswordEntry => &[S::PasswordBox],
        Role::SpinButton => &[S::SpinButton],
        Role::Slider => &[S::Slider],
        Role::ProgressBar => &[S::ProgressBar, S::Meter],
        Role::Tab => &[S::Tab],
        Role::TabList => &[S::TabList],
        Role::Tree => &[S::Tree],
        Role::TreeItem => &[S::TreeItem],
        Role::MenuBar => &[S::Menubar],
        Role::DocumentWeb => &[S::DocumentWeb],
        Role::Unknown
        | Role::Application
        | Role::Window
        | Role::Dialog
        | Role::Panel
        | Role::Document
        | Role::Paragraph
        | Role::Image
        | Role::List
        | Role::ListItem
        | Role::Table
        | Role::Row
        | Role::Cell
        | Role::ColumnHeader
        | Role::RowHeader
        | Role::Menu
        | Role::MenuItem
        | Role::Popover
        | Role::Toolbar
        | Role::StatusBar
        | Role::Separator
        | Role::Label
        | Role::StaticText
        | Role::Terminal
        | Role::Landmark
        | Role::Alert => return None,
    };
    Some(ok.contains(&sem))
}

/// Whether a disagreement log line may carry page text (names). Off unless the daemon's main
/// turns it on from an explicit environment opt-in (CI and the e2e harness); library code never
/// reads the environment.
static LOG_NAMES: AtomicBool = AtomicBool::new(false);

/// Opt in to page text in disagreement log lines (default off: a name can be a private message
/// or a form value, and a log is not the place for it).
pub fn set_log_names(on: bool) {
    LOG_NAMES.store(on, Ordering::Relaxed);
}

/// A short stable digest of a string, enough to tell two names apart in a log without saying
/// what they were.
fn short_hash(s: Option<&str>) -> u32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    let v = h.finish();
    u32::try_from(v & 0xFFFF_FFFF).unwrap_or(0)
}

/// What a disagreement log line says about the names: lengths and short hashes by default,
/// the (cut) text itself only with the opt-in.
fn describe(n: &SrcNode, e: &SemEntry, with_names: bool) -> String {
    use std::fmt::Write as _;
    let legacy = n.name.as_deref();
    let fused = e.name.as_deref();
    let mut out = format!(
        "legacy name {} bytes #{:08x}, fused name {} bytes #{:08x}",
        legacy.map_or(0, str::len),
        short_hash(legacy),
        fused.map_or(0, str::len),
        short_hash(fused)
    );
    if with_names {
        let cut = |s: Option<&str>| s.map(|s| s.chars().take(40).collect::<String>());
        let _ = write!(out, "; legacy {:?}, fused {:?}", cut(legacy), cut(fused));
    }
    out
}

fn note(n: &SrcNode, a: Aspect, e: &SemEntry) {
    MISMATCH[a.ix()].fetch_add(1, Ordering::Relaxed);
    if LOGGED.fetch_add(1, Ordering::Relaxed) < LOG_CAP {
        tracing::warn!(
            target: "gorca_agree",
            aspect = a.label(),
            legacy_role = ?n.role,
            fused_role = e.sem.role.name(),
            names = %describe(n, e, LOG_NAMES.load(Ordering::Relaxed)),
            "legacy and fused values disagree"
        );
    }
}

static RULES: LazyLock<Rules> = LazyLock::new(Rules::aria);

/// `legacy` is `fused` with a list marker in front ("• ", "1. ", "– "): the legacy path strips
/// markers only on list and tree items.
fn marker_only(legacy: &str, fused: &str) -> bool {
    legacy.strip_suffix(fused).is_some_and(|prefix| {
        !prefix.is_empty() && prefix.len() <= 8 && !prefix.chars().any(char::is_alphabetic)
    })
}

/// The aspects on which the legacy values of `n` and the fused entry `e` disagree. Pure.
pub fn compare(n: &SrcNode, flags: NodeFlags, level: u8, e: &SemEntry) -> Vec<Aspect> {
    let mut out = Vec::new();
    let k = |g: Group| e.known.has(g);
    let role = e.sem.role;
    if k(Group::Role) && role_agrees(n.role, role) == Some(false) {
        out.push(Aspect::Role);
    }
    if k(Group::Name)
        && let Some(legacy) = n.name.as_deref()
        && legacy.len() <= gorca_fuse::MAX_TEXT_BYTES
    {
        let fused = e.name.as_deref();
        let marker_kept = !matches!(n.role, Role::ListItem | Role::TreeItem)
            && fused.is_some_and(|f| marker_only(legacy, f));
        if fused != Some(legacy) && !marker_kept {
            out.push(Aspect::Name);
        }
    }
    if n.role == Role::Heading && k(Group::Level) {
        let want = if (1..=6).contains(&e.sem.level) {
            e.sem.level
        } else {
            0
        };
        if level != want {
            out.push(Aspect::Level);
        }
    }
    if k(Group::Role) {
        use SemRole as S;
        let structural: [(NodeFlags, &[SemRole]); 5] = [
            (NodeFlags::ARTICLE, &[S::Article]),
            (NodeFlags::BLOCKQUOTE, &[S::Blockquote]),
            (NodeFlags::REGION, &[S::Region]),
            (NodeFlags::LISTBOX, &[S::Listbox]),
            (
                NodeFlags::ANNOTATION,
                &[S::Insertion, S::Deletion, S::Comment, S::Suggestion],
            ),
        ];
        // One direction: a flag the translator derived must be backed by the fused role. The
        // model knows more roles than the flags do (`role=blockquote` on a `div`, the DPUB
        // landmarks that map to `region`), which the legacy flags never marked.
        if structural
            .iter()
            .any(|(f, roles)| flags.contains(*f) && !roles.contains(&role))
        {
            out.push(Aspect::Structure);
        }
    }
    if k(Group::Live) && !matches!(role, SemRole::Alert | SemRole::AlertDialog) {
        let region_root = e.sem.live.is_some_and(|l| l.root);
        let token_role = matches!(
            role,
            SemRole::Status | SemRole::Log | SemRole::Marquee | SemRole::Timer
        );
        if flags.contains(NodeFlags::LIVE) != (region_root || token_role) {
            out.push(Aspect::Live);
        }
    }
    let st = n.states;
    if k(Group::Check)
        && RULES.checkable.has(role)
        && st.contains(StateSet::CHECKED) != (e.sem.check == Check::On)
    {
        out.push(Aspect::Check);
    }
    if k(Group::Expand)
        && RULES.expandable.has(role)
        && st.contains(StateSet::EXPANDED) != (e.sem.expand == Expand::Expanded)
    {
        out.push(Aspect::Expand);
    }
    if k(Group::Select)
        && e.sem.select != Select::NotSelectable
        && st.contains(StateSet::SELECTED) != (e.sem.select == Select::On)
    {
        out.push(Aspect::Select);
    }
    out
}

/// A disagreement found outside [`compare`] (the live tracker's region parse): counted, logged
/// (capped) and asserted like the others.
pub(super) fn flag(a: Aspect, role: Role, detail: &str) {
    MISMATCH[a.ix()].fetch_add(1, Ordering::Relaxed);
    if LOGGED.fetch_add(1, Ordering::Relaxed) < LOG_CAP {
        tracing::warn!(
            target: "gorca_agree",
            aspect = a.label(),
            legacy_role = ?role,
            detail,
            "legacy and fused values disagree"
        );
    }
    debug_assert!(
        deviation(a),
        "legacy agreement: {} disagrees on a {role:?} node: {detail}",
        a.label()
    );
}

/// Compare one node's legacy values with its fused entry: count, log (capped) and, in a debug
/// build, assert on a disagreement that is not a listed deviation.
pub fn check(n: &SrcNode, flags: NodeFlags, level: u8, e: &SemEntry) {
    CHECKED.fetch_add(1, Ordering::Relaxed);
    for a in compare(n, flags, level, e) {
        note(n, a, e);
        debug_assert!(
            deviation(a),
            "legacy agreement: {} disagrees on a {:?} node (see the gorca_agree log)",
            a.label(),
            n.role
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::sem::{Check, Group, GroupMask, SemRole};

    use super::*;

    fn entry(role: SemRole, name: Option<&str>) -> SemEntry {
        let mut e = SemEntry::default();
        e.sem.role = role;
        e.name = name.map(Arc::from);
        e.known = GroupMask::EMPTY;
        e.known.set(Group::Role, true);
        e.known.set(Group::Name, true);
        e.known.set(Group::Check, true);
        e.known.set(Group::Level, true);
        e
    }

    fn node(role: Role, name: Option<&str>, states: StateSet) -> SrcNode {
        SrcNode {
            id: gorca_model::SrcNodeId(1),
            role,
            states,
            name: name.map(Arc::from),
            text: None,
            attrs: smallvec::SmallVec::default(),
            local: None,
            rec: None,
        }
    }

    #[test]
    fn matching_values_agree_and_the_new_model_may_know_more() {
        let n = node(Role::Link, Some("Home"), StateSet::default());
        assert!(compare(&n, NodeFlags::NONE, 0, &entry(SemRole::Link, Some("Home"))).is_empty());
        // No legacy name: a content name is the feature, not a disagreement.
        let n = node(Role::Link, None, StateSet::default());
        assert!(compare(&n, NodeFlags::NONE, 0, &entry(SemRole::Link, Some("Home"))).is_empty());
        // A legacy role the table does not cover is not compared.
        let n = node(Role::Panel, None, StateSet::default());
        assert!(compare(&n, NodeFlags::NONE, 0, &entry(SemRole::Heading, None)).is_empty());
    }

    #[test]
    fn each_disagreement_names_its_aspect() {
        let n = node(Role::Heading, Some("Title"), StateSet::CHECKED);
        let mut e = entry(SemRole::Link, Some("Other"));
        e.sem.level = 2;
        let got = compare(&n, NodeFlags::NONE, 3, &e);
        assert_eq!(got, vec![Aspect::Role, Aspect::Name, Aspect::Level]);
        // Check is compared only on roles that are checkable.
        let n = node(Role::CheckBox, Some("Opt"), StateSet::CHECKED);
        let mut e = entry(SemRole::Checkbox, Some("Opt"));
        assert_eq!(compare(&n, NodeFlags::NONE, 0, &e), vec![Aspect::Check]);
        e.sem.check = Check::On;
        assert!(compare(&n, NodeFlags::NONE, 0, &e).is_empty());
        let n = node(Role::ListItem, Some("x"), StateSet::CHECKED);
        let e = entry(SemRole::Option, Some("x"));
        assert!(
            compare(&n, NodeFlags::NONE, 0, &e).is_empty(),
            "not checkable"
        );
    }

    #[test]
    fn live_compares_region_roots_and_tokens_and_leaves_alert_to_its_role() {
        let n = node(Role::Panel, None, StateSet::default());
        let mut e = entry(SemRole::Generic, None);
        e.known.set(Group::Live, true);
        // A legacy live flag without a fused live region is a disagreement.
        assert_eq!(compare(&n, NodeFlags::LIVE, 0, &e), vec![Aspect::Live]);
        // A region root agrees with the flag; an inherited container value does not make one.
        let mut live = gorca_model::sem::Live {
            politeness: gorca_model::event::Politeness::Polite,
            atomic: false,
            relevant: gorca_model::sem::Relevant::default(),
            root: true,
        };
        e.sem.live = Some(live);
        assert!(compare(&n, NodeFlags::LIVE, 0, &e).is_empty());
        live.root = false;
        e.sem.live = Some(live);
        assert!(compare(&n, NodeFlags::NONE, 0, &e).is_empty());
        // The timer token is live for the legacy flag even when `live=off`; alert is not.
        let e = {
            let mut t = entry(SemRole::Timer, None);
            t.known.set(Group::Live, true);
            t
        };
        assert!(compare(&n, NodeFlags::LIVE, 0, &e).is_empty());
        let e = {
            let mut t = entry(SemRole::Alert, None);
            t.known.set(Group::Live, true);
            t.sem.live = Some(live);
            t
        };
        assert!(compare(&n, NodeFlags::NONE, 0, &e).is_empty());
    }

    #[test]
    fn marker_and_level_differences_are_the_documented_deviations() {
        // A list marker the legacy path kept outside list and tree items.
        let n = node(Role::MenuItem, Some("• New"), StateSet::default());
        assert!(
            compare(
                &n,
                NodeFlags::NONE,
                0,
                &entry(SemRole::MenuItem, Some("New"))
            )
            .is_empty()
        );
        let n = node(Role::ListItem, Some("• New"), StateSet::default());
        assert_eq!(
            compare(
                &n,
                NodeFlags::NONE,
                0,
                &entry(SemRole::ListItem, Some("New"))
            ),
            vec![Aspect::Name],
            "on a list item the legacy path strips markers too: a leftover is a bug"
        );
        // aria-level beyond 6 is not a heading level of the legacy path.
        let n = node(Role::Heading, Some("T"), StateSet::default());
        let mut e = entry(SemRole::Heading, Some("T"));
        e.sem.level = 9;
        assert!(compare(&n, NodeFlags::NONE, 0, &e).is_empty());
        assert_eq!(compare(&n, NodeFlags::NONE, 6, &e), vec![Aspect::Level]);
        assert!(marker_only("1. First", "First") && !marker_only("Say First", "First"));
    }

    #[test]
    fn the_default_log_line_carries_no_page_text() {
        let secret = "Dinner at 7 with Anna, door code 4821";
        let n = node(Role::Link, Some(secret), StateSet::default());
        let e = entry(SemRole::Link, Some("Another private message"));
        let line = describe(&n, &e, false);
        assert!(!line.contains("Dinner") && !line.contains("4821"), "{line}");
        assert!(
            !line.contains("Another") && !line.contains("private"),
            "{line}"
        );
        assert!(line.contains(&format!("{} bytes", secret.len())), "{line}");
        // Two different names stay distinguishable.
        assert_ne!(short_hash(Some("a")), short_hash(Some("b")));
        // The opt-in adds the text (cut), nothing else changes.
        let full = describe(&n, &e, true);
        assert!(
            full.starts_with(&line) && full.contains("Dinner at 7"),
            "{full}"
        );
        // The default is off.
        assert!(!LOG_NAMES.load(Ordering::Relaxed));
    }

    #[test]
    fn the_counters_add_up() {
        let s = stats();
        assert_eq!(s.total(), s.mismatched.iter().sum::<u64>());
        assert_eq!(s.of(Aspect::Role), s.mismatched[0]);
    }
}
