#![allow(clippy::unwrap_used, clippy::expect_used)]
// SPDX-License-Identifier: GPL-3.0-or-later
//! The replay half of the cut-over gate for "where am I" (`cargo xtask cutover whereami`,
//! ADR-0022 section 8.9, M22b4).
//!
//! `cutover_emit` replays every recorded corpus family: every node is the focus once, its
//! ancestry is the chain of parents the page holds (at most 24, leaf first), and the answer of the
//! legacy builder (`Role::spoken`, its own state table) and of the fused one (`Reason::WhereAmI`
//! over the entries, through `gorca-present`) are compared part by part (the parts are the answer
//! split at ", "). Four cases per node: the basic answer, the detailed one, the spelled name and
//! the brief one. One record per case goes to the file named by `GORCA_CUTOVER_OUT`. It does not
//! judge: `xtask` classifies the records. Without `GORCA_CUTOVER_OUT` the test is a no-op.
//!
//! Environment: `GORCA_CUTOVER_SURFACE` (`whereami`), `GORCA_CUTOVER_OUT`,
//! `GORCA_CUTOVER_LANG` (`en` or `ru`), `GORCA_CUTOVER_SHUFFLE` (non-zero seed: the arrival order
//! of the nodes at the fusion writer is shuffled; the records must not change).
//!
//! Record format: `corpus  node  case  surface  outcome  deviation  legacy  new  witness`, tab
//! separated and sorted, `\`, tab and newline escaped (the same as the focus and browse gates).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use gorca_model::{NodeSnapshot, Role, WebEngine};
use gorca_present::Deviations;

use crate::legacy_agreement_tests::{page, pages_of, root};

/// A corpus family: directory under `tests/fixtures/sem`, toolkit name for the profile tables,
/// and the web engine (`None` for native applications).
struct Family {
    dir: &'static str,
    toolkit: &'static str,
    web: WebEngine,
}

/// Keep in step with `FAMILIES` in `xtask/src/cutover.rs`.
const FAMILIES: &[Family] = &[
    Family {
        dir: "gtk3",
        toolkit: "GTK",
        web: WebEngine::None,
    },
    Family {
        dir: "gtk4",
        toolkit: "GTK",
        web: WebEngine::None,
    },
    Family {
        dir: "firefox",
        toolkit: "",
        web: WebEngine::Gecko,
    },
    Family {
        dir: "chromium",
        toolkit: "",
        web: WebEngine::Blink,
    },
    Family {
        dir: "electron",
        toolkit: "",
        web: WebEngine::Blink,
    },
    Family {
        dir: "telegram",
        toolkit: "",
        web: WebEngine::Blink,
    },
    // GNOME Shell: Clutter/St actors through cally; no toolkit attribute the profile table knows.
    Family {
        dir: "gnome-shell",
        toolkit: "",
        web: WebEngine::None,
    },
    // gnome-terminal: GTK 3 widgets around a VTE terminal.
    Family {
        dir: "vte",
        toolkit: "GTK",
        web: WebEngine::None,
    },
];

/// One output line.
struct Record {
    corpus: String,
    node: u64,
    case: String,
    surface: &'static str,
    outcome: &'static str,
    deviation: &'static str,
    legacy: String,
    new: String,
    witness: String,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

// ---------------------------------------------------------------------------------------------
// Drops: nothing the replay does not compare is skipped silently
// ---------------------------------------------------------------------------------------------

/// Drops of one (corpus, case): per reason a count and the first example.
#[derive(Default)]
struct Drops(BTreeMap<&'static str, (u64, String)>);

impl Drops {
    fn add_n(&mut self, reason: &'static str, n: u64, example: impl FnOnce() -> String) {
        if n == 0 {
            return;
        }
        let e = self.0.entry(reason).or_insert_with(|| (0, example()));
        e.0 += n;
    }

    fn add(&mut self, reason: &'static str, example: impl FnOnce() -> String) {
        self.add_n(reason, 1, example);
    }

    fn flush(self, corpus: &str, case: &str, surface: &'static str, out: &mut Vec<Record>) {
        for (reason, (n, example)) in self.0 {
            out.push(dropped(corpus, case, surface, reason, n, example));
        }
    }
}

fn dropped(
    corpus: &str,
    case: &str,
    surface: &'static str,
    reason: &'static str,
    n: u64,
    example: String,
) -> Record {
    Record {
        corpus: corpus.to_owned(),
        node: 0,
        case: case.to_owned(),
        surface,
        outcome: "dropped",
        deviation: reason,
        legacy: n.to_string(),
        new: example,
        witness: String::new(),
    }
}

fn describe(s: &NodeSnapshot) -> String {
    let name: String = s.name.as_deref().unwrap_or("").chars().take(30).collect();
    format!("{:?} {name:?}", s.role)
}

/// Why a node has no fused entry. A text leaf is merged into its parent's content name and is
/// never a focus target; any other role without an entry is a defect the gate must not hide.
fn no_entry_reason(s: &NodeSnapshot) -> &'static str {
    if matches!(s.role, Role::StaticText | Role::Label) {
        "static-leaf-no-entry"
    } else {
        "no-fused-entry"
    }
}

/// SplitMix64: a tiny deterministic generator (the shuffle must not depend on a crate version).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// A seeded arrival order that interleaves the nodes of different parents (and so moves them
/// between batches, children before parents included) but keeps the relative order of siblings:
/// that order is data (the child index), not arrival noise, so no contract may ignore it.
fn arrival_order(snaps: &[Arc<NodeSnapshot>], seed: u64) -> Vec<usize> {
    let mut queues: Vec<std::collections::VecDeque<usize>> = Vec::new();
    let mut slot: BTreeMap<Option<gorca_model::NodeKey>, usize> = BTreeMap::new();
    for (i, s) in snaps.iter().enumerate() {
        let q = *slot.entry(s.parent).or_insert_with(|| {
            queues.push(std::collections::VecDeque::new());
            queues.len() - 1
        });
        queues[q].push_back(i);
    }
    let mut rng = Rng(seed);
    let mut order = Vec::with_capacity(snaps.len());
    while !queues.is_empty() {
        let q = usize::try_from(rng.next() % queues.len() as u64).unwrap_or(0);
        if let Some(i) = queues[q].pop_front() {
            order.push(i);
        }
        if queues[q].is_empty() {
            queues.swap_remove(q);
        }
    }
    order
}

/// Fuse `snaps` the way the app actor does. With a non-zero `seed` the nodes arrive in a seeded
/// shuffled order (and so in other batches); the result is returned in the original order.
fn fuse(
    tables: Option<gorca_model::SemTables>,
    label: &str,
    snaps: &[Arc<NodeSnapshot>],
    seed: u64,
) -> Vec<Arc<NodeSnapshot>> {
    if seed == 0 {
        return gorca_core::fuse_snapshots(tables, snaps);
    }
    let order = arrival_order(snaps, seed ^ fnv(label));
    let shuffled: Vec<Arc<NodeSnapshot>> = order.iter().map(|&i| Arc::clone(&snaps[i])).collect();
    let fused = gorca_core::fuse_snapshots(tables, &shuffled);
    let mut back: Vec<Option<Arc<NodeSnapshot>>> = vec![None; snaps.len()];
    for (pos, &i) in order.iter().enumerate() {
        back[i] = Some(Arc::clone(&fused[pos]));
    }
    back.into_iter()
        .zip(snaps)
        .map(|(f, orig)| f.unwrap_or_else(|| Arc::clone(orig)))
        .collect()
}

fn is_web(s: &NodeSnapshot) -> bool {
    s.attrs
        .iter()
        .any(|(k, _)| *k == gorca_doc_source_api::web_attrs::TAG)
}

/// Whether each node is web content: it carries the engine's DOM tag, is a web document, or has a
/// web ancestor. Chromium exports no tag on its text leaves, so the tag alone would call them
/// native and drop them. The page is in document order (parents first).
fn web_flags(snaps: &[Arc<NodeSnapshot>]) -> Vec<bool> {
    let index: BTreeMap<gorca_model::NodeKey, usize> =
        snaps.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
    let mut out = vec![false; snaps.len()];
    for (i, s) in snaps.iter().enumerate() {
        let parent_web = s
            .parent
            .and_then(|p| index.get(&p))
            .is_some_and(|&j| j < i && out[j]);
        out[i] = is_web(s) || s.role == Role::DocumentWeb || parent_web;
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The replay
// ---------------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------------
// The intended differences of "where am I"
// ---------------------------------------------------------------------------------------------

/// Role and state words the one word table says differently from the legacy `Role::spoken` and
/// its state table (legacy suffix, fused suffix): the rest of the part is the same.
const WORD_PAIRS: &[(&str, &str)] = &[
    ("label", "text"),
    ("panel", "grouping"),
    ("tab list", "tab control"),
    ("tree", "tree view"),
    ("tree item", "tree view item"),
    ("toolbar", "tool bar"),
    ("toggle button", "button"),
    ("toggle button", "switch"),
    ("check box", "check menu item"),
    ("radio button", "radio menu item"),
    ("table", "grid"),
    ("panel", "list item"),
    ("panel", "list"),
    ("paragraph", "list item"),
    ("menu", "menu item"),
    ("table", "panel"),
    ("list", "list item"),
    ("alert", "dialog"),
    ("checked", "pressed"),
    ("checked", "on"),
    ("has popup", "submenu"),
    ("has popup", "collapsed"),
    ("has popup", "expanded"),
    ("collapsed has popup", "submenu"),
    ("expanded has popup", "submenu"),
];

/// The roles whose word the web table has none for where the legacy mapping spoke one.
const UNWORDED: &[&str] = &["panel", "text", "label"];

/// Legacy state words the model does not give the node (the row says why).
const DROPPED_STATES: &[&str] = &["has popup", "pressed", "popup", "invalid entry"];

fn is_marker(c: char) -> bool {
    "*-+>\u{2022}\u{25E6}\u{25AA}\u{25AB}\u{2023}\u{00B7}\u{2013}\u{2014}\u{25CF}\u{25CB}\u{25A0}\u{25A1}\u{2043}"
        .contains(c)
}

/// What the fused entry of the leaf says that decides which rows apply.
#[derive(Copy, Clone)]
struct Facts {
    /// Availability does not apply to the node (SM-5).
    avail_na: bool,
    /// The node's role carries a value the model knows from attributes or a text reading.
    value_role: bool,
}

/// The catalog's role, state and ARIA words in `lang` (lower case), whole phrases.
fn catalog_words(lang: gorca_present::Lang) -> BTreeSet<String> {
    // A message with placeholders panics in `t_in` (debug builds): it is no plain word, skip it.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let out = gorca_i18n::MsgId::ALL
        .iter()
        .filter(|id| {
            let k = id.key();
            k.starts_with("role-")
                || k.starts_with("state-")
                || k.starts_with("aria-")
                || k.starts_with("web-")
        })
        .filter_map(|id| {
            std::panic::catch_unwind(|| gorca_i18n::t_in(lang, *id).to_lowercase()).ok()
        })
        .filter(|w| !w.is_empty())
        .collect();
    std::panic::set_hook(hook);
    out
}

struct Rules<'a> {
    facts: Facts,
    spelled: bool,
    words: &'a BTreeSet<String>,
    /// The English word or phrase of the tables above to the catalog word of the run's language.
    tr: &'a BTreeMap<String, String>,
    /// The tail of the nameless answer (", no name") and the spelled space, in the run's language.
    no_name: &'a str,
    space: &'a str,
}

/// The English text of every plain role, state and ARIA message, to the same message in `lang`
/// (lower case): the tables of the rules are written in English.
fn translations(lang: gorca_present::Lang) -> BTreeMap<String, String> {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut out = BTreeMap::new();
    for id in gorca_i18n::MsgId::ALL {
        let k = id.key();
        if !(k.starts_with("role-")
            || k.starts_with("state-")
            || k.starts_with("aria-")
            || k.starts_with("web-"))
        {
            continue;
        }
        let both = std::panic::catch_unwind(|| {
            (
                gorca_i18n::t_in(gorca_present::Lang::En, *id).to_lowercase(),
                gorca_i18n::t_in(lang, *id).to_lowercase(),
            )
        });
        if let Ok((en, local)) = both
            && !en.is_empty()
        {
            out.entry(en).or_insert(local);
        }
    }
    std::panic::set_hook(hook);
    out
}

impl Rules<'_> {
    /// An English table entry in the run's language (unchanged when the catalog has no such word).
    fn t(&self, en: &str) -> String {
        self.lookup(en).unwrap_or_else(|| en.to_owned())
    }

    /// The phrase as one catalog word, or as two catalog phrases side by side ("collapsed has
    /// popup").
    fn lookup(&self, en: &str) -> Option<String> {
        if let Some(local) = self.tr.get(en) {
            return Some(local.clone());
        }
        en.match_indices(' ').find_map(|(at, _)| {
            let (a, b) = (&en[..at], &en[at + 1..]);
            Some(format!("{} {}", self.lookup(a)?, self.lookup(b)?))
        })
    }

    fn unworded(&self, w: &str) -> bool {
        UNWORDED.iter().any(|u| self.t(u) == w)
    }

    fn collapsed(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn squash(s: &str) -> String {
        s.split_whitespace().collect()
    }

    /// The spelled name without the separators the spelling adds.
    fn unspaced(&self, s: &str) -> String {
        s.split_whitespace()
            .filter(|w| *w != self.space && !w.chars().all(is_marker))
            .collect()
    }

    /// The row that explains why the legacy part `x` became the fused part `y`.
    fn pair(&self, x: &str, y: &str) -> Option<&'static str> {
        if self.spelled && x.ends_with(self.no_name) && !y.ends_with(self.no_name) {
            // A nameless node whose fused name is derived: the name is spelled.
            return Some("content-derived-name");
        }
        let bare = |s: &str| s.strip_suffix(self.no_name).unwrap_or(s).to_owned();
        let (x, y) = (bare(x), bare(y));
        if y.is_empty() && self.unworded(&x) {
            return Some("role-not-in-table");
        }
        // A blank name (white space only) is no name.
        if self.spelled && !x.is_empty() && self.unspaced(&x).is_empty() {
            return Some("name-whitespace");
        }
        if Self::collapsed(&x) == y || (self.spelled && self.unspaced(&x) == self.unspaced(&y)) {
            return Some("name-whitespace");
        }
        if x != y && x.chars().zip(y.chars()).take_while(|(a, b)| a == b).count() >= 200 {
            return Some("name-cap-differs");
        }
        // The same content joined with other separators (a derived name).
        if !x.is_empty() && Self::squash(&x) == Self::squash(&y) {
            return Some("content-derived-name");
        }
        // A marker the legacy name carried, and possibly another difference behind it.
        let unmarked = x.trim_start_matches(|c: char| is_marker(c) || c.is_whitespace());
        if unmarked.len() < x.len() && !unmarked.is_empty() {
            if unmarked == y {
                return Some("marker-stripped-name");
            }
            if let Some(id) = self.pair(unmarked, &y) {
                return Some(id);
            }
        }
        // The word the table has none for after a name ("Fig caption panel").
        if let Some((head, w)) = x.rsplit_once(' ')
            && head == y
            && self.unworded(w)
        {
            return Some("role-not-in-table");
        }
        // The legacy part is the bare word of a role (no name): the fused node has a name.
        if self.words.contains(&x.to_lowercase())
            && let Some(p) = y.strip_suffix(x.as_str())
            && p.ends_with(' ')
            && !p.trim().is_empty()
        {
            return Some("content-derived-name");
        }
        if self.words.contains(&x.to_lowercase())
            && let Some(head) = self.phrase_head(&y)
            && !head.is_empty()
        {
            // A bare legacy word and a fused name with a role word of its own.
            return Some("content-derived-name");
        }
        for (lw, nw) in WORD_PAIRS {
            let (lw, nw) = (self.t(lw), self.t(nw));
            for a in [x.clone(), Self::collapsed(&x)] {
                if let (Some(pa), Some(pb)) =
                    (a.strip_suffix(lw.as_str()), y.strip_suffix(nw.as_str()))
                    && pa == pb
                    && (pa.is_empty() || pa.ends_with(' '))
                {
                    return Some("role-word-improved");
                }
            }
        }
        // A number the model speaks as its text reading or formatted (2 and 02, 2 and Tuesday).
        if self.facts.value_role && x.parse::<f64>().is_ok() {
            return Some("value-text-spoken");
        }
        if let Some(base) = x.strip_suffix(format!(" {}", self.t("has popup")).as_str())
            && base == y
        {
            return Some("state-not-applicable");
        }
        // A state word the model does not give the node, inside a part of states.
        for st in DROPPED_STATES {
            let st = self.t(st);
            let ws: Vec<&str> = x.split(' ').collect();
            let sw: Vec<&str> = st.split(' ').collect();
            if let Some(at) = ws.windows(sw.len()).position(|w| w == sw.as_slice()) {
                let mut rest = ws.clone();
                rest.drain(at..at + sw.len());
                if rest.join(" ") == y {
                    return Some("state-not-applicable");
                }
            }
        }
        // A catalog word inserted in the middle of the legacy part (an off state).
        for w in self.words {
            let ys: Vec<&str> = y.split(' ').collect();
            let ww: Vec<&str> = w.split(' ').collect();
            if let Some(at) = ys.windows(ww.len()).position(|c| c == ww.as_slice()) {
                let mut rest = ys.clone();
                rest.drain(at..at + ww.len());
                if rest.join(" ") == x {
                    return Some("more-words");
                }
            }
        }
        // The legacy part with catalog words added to it (states, role words, refinements).
        if let Some(rest) = y
            .strip_prefix(x.as_str())
            .or_else(|| y.strip_suffix(x.as_str()))
            && self.words.contains(rest.trim().to_lowercase().as_str())
        {
            return Some("more-words");
        }
        None
    }

    /// What is in front of the longest catalog phrase `y` ends with.
    fn phrase_head(&self, y: &str) -> Option<String> {
        let lower = y.to_lowercase();
        self.words
            .iter()
            .filter(|w| lower.ends_with(w.as_str()))
            .find_map(|w| {
                let head = &lower[..lower.len() - w.len()];
                (head.ends_with(' ') && !head.trim().is_empty()).then(|| head.trim().to_owned())
            })
    }
}

/// Try to explain the difference of one comparison. The residue of the multiset difference of the
/// parts must be explained completely, otherwise the difference stays unexplained. Returns the row
/// ids used.
fn explain_parts(
    legacy: &[String],
    new: &[String],
    rules: &Rules<'_>,
    dev: &Deviations,
) -> Option<Vec<&'static str>> {
    let mut l: Vec<String> = legacy.to_vec();
    let mut n: Vec<String> = new.to_vec();
    // Common parts cancel (one for one).
    let mut i = 0;
    while i < l.len() {
        if let Some(j) = n.iter().position(|x| *x == l[i]) {
            l.remove(i);
            n.remove(j);
        } else {
            i += 1;
        }
    }
    let mut used: Vec<&'static str> = Vec::new();
    let allowed = |id: &'static str, used: &mut Vec<&'static str>| -> bool {
        if dev.allows(id, "whereami") {
            if !used.contains(&id) {
                used.push(id);
            }
            true
        } else {
            false
        }
    };
    let mut progress = true;
    while progress && !(l.is_empty() && n.is_empty()) {
        progress = false;
        // A legacy "unavailable" the model says does not apply (static structure).
        if rules.facts.avail_na {
            for k in 0..l.len() {
                let ws: Vec<&str> = l[k].split(' ').collect();
                let unavailable = rules.t("unavailable");
                if ws.contains(&unavailable.as_str()) {
                    let rest = ws
                        .iter()
                        .filter(|w| **w != unavailable)
                        .copied()
                        .collect::<Vec<_>>()
                        .join(" ");
                    if rest.is_empty() && allowed("unavailable-static", &mut used) {
                        l.remove(k);
                        progress = true;
                        break;
                    }
                    if let Some(j) = n.iter().position(|x| *x == rest)
                        && allowed("unavailable-static", &mut used)
                    {
                        l.remove(k);
                        n.remove(j);
                        progress = true;
                        break;
                    }
                }
            }
            if progress {
                continue;
            }
        }
        // Pairs of one legacy and one new part.
        'pairs: for a in 0..l.len() {
            for b in 0..n.len() {
                if let Some(id) = rules.pair(&l[a], &n[b])
                    && allowed(id, &mut used)
                {
                    l.remove(a);
                    n.remove(b);
                    progress = true;
                    break 'pairs;
                }
            }
        }
        if progress {
            continue;
        }
        // A new part that is only a catalog word (an off state, busy, current, a role word the
        // legacy table had none for).
        if let Some(j) = n
            .iter()
            .position(|y| rules.words.contains(&y.to_lowercase()))
            && allowed("more-words", &mut used)
        {
            n.remove(j);
            progress = true;
            continue;
        }
        // A value the model knows from attributes where the legacy snapshot had none.
        if rules.facts.value_role
            && let Some(j) = n.iter().position(|y| !y.is_empty())
            && allowed("value-from-attrs", &mut used)
        {
            n.remove(j);
            progress = true;
            continue;
        }
        // A new part "name role word" where the legacy answer listed the bare word only.
        if let Some(j) = n.iter().position(|y| {
            y.rsplit_once(' ').is_some_and(|(p, w)| {
                !p.trim().is_empty()
                    && rules.words.contains(&w.to_lowercase())
                    && legacy.iter().any(|x| x.eq_ignore_ascii_case(w))
            })
        }) && allowed("content-derived-name", &mut used)
        {
            n.remove(j);
            progress = true;
            continue;
        }
        // A bare legacy role word the web table says nothing for (a generic container, a text
        // run): the ancestry skips it.
        if let Some(a) = l.iter().position(|x| rules.unworded(x))
            && allowed("role-not-in-table", &mut used)
        {
            l.remove(a);
            progress = true;
            continue;
        }
        // The state words of a role the model does not give them.
        if let Some(a) = l
            .iter()
            .position(|x| DROPPED_STATES.iter().any(|d| rules.t(d) == *x))
            && allowed("state-not-applicable", &mut used)
        {
            l.remove(a);
            progress = true;
        }
    }
    (l.is_empty() && n.is_empty() && !used.is_empty()).then_some(used)
}

struct Run {
    lang: gorca_present::Lang,
    seed: u64,
}

/// The longest ancestry the answer walks (`commands::info::MAX_DEPTH`).
const MAX_DEPTH: usize = 24;

fn records(family: &Family, surface: &'static str, run: &Run, out: &mut Vec<Record>) {
    let dev = Deviations::builtin();
    let words = catalog_words(run.lang);
    let tr = translations(run.lang);
    let no_name = gorca_i18n::tf_in(
        run.lang,
        gorca_i18n::MsgId::InfoNoName,
        &[("role", gorca_i18n::Arg::Str(""))],
    );
    let space = gorca_i18n::t_in(run.lang, gorca_i18n::MsgId::WordSpace).to_owned();
    let tables = crate::sem_profile::tables_for(family.toolkit, "app", family.web);
    for name in pages_of(family.dir) {
        let label = format!("{}/{name}", family.dir);
        let Some(snaps) = page(family.dir, &name) else {
            out.push(dropped(
                &label,
                "all",
                surface,
                "page-unparsable",
                1,
                String::new(),
            ));
            continue;
        };
        let fused = fuse(tables, &label, &snaps, run.seed);
        let webs = web_flags(&fused);
        let index: BTreeMap<gorca_model::NodeKey, usize> =
            fused.iter().enumerate().map(|(i, s)| (s.key, i)).collect();
        let mut drops = Drops::default();
        let mut seen = 0u64;
        for (i, s) in fused.iter().enumerate() {
            let Some(entry) = s.sem.entry.as_ref() else {
                drops.add(no_entry_reason(s), || describe(s));
                continue;
            };
            seen += 1;
            // The ancestry as the mirror holds it: leaf first, up to the known parent.
            let mut chain: Vec<Arc<NodeSnapshot>> = vec![Arc::clone(s)];
            let mut chain_web = vec![webs[i]];
            let mut cur = s.parent;
            while let Some(p) = cur {
                if chain.len() >= MAX_DEPTH {
                    break;
                }
                let Some(&j) = index.get(&p) else { break };
                chain.push(Arc::clone(&fused[j]));
                chain_web.push(webs[j]);
                cur = fused[j].parent;
            }
            if chain.iter().any(|n| n.sem.entry.is_none()) {
                drops.add("ancestor-no-entry", || describe(s));
                continue;
            }
            for (case, repeat, brief) in [
                ("r1", 1u8, false),
                ("r2", 2, false),
                ("r3", 3, false),
                ("brief", 1, true),
            ] {
                let legacy_parts = gorca_core::info_gate::legacy_where_am_i(&chain, repeat, brief);
                let legacy_text = legacy_parts.join(", ");
                let new_parts = gorca_core::info_gate::fused_where_am_i(
                    &chain, repeat, brief, run.lang, &chain_web,
                );
                let new_text = new_parts.join(", ");
                let rules = Rules {
                    spelled: repeat == 3,
                    words: &words,
                    tr: &tr,
                    no_name: &no_name,
                    space: &space,
                    facts: Facts {
                        avail_na: entry.sem.avail == gorca_model::sem::Avail::NotApplicable,
                        value_role: gorca_present::diff::VALUE_ROLES.contains(&entry.sem.role),
                    },
                };
                let (outcome, deviation) = if legacy_parts == new_parts {
                    (
                        if new_parts.is_empty() {
                            "silent"
                        } else {
                            "equal"
                        },
                        "",
                    )
                } else {
                    match explain_parts(&legacy_parts, &new_parts, &rules, &dev) {
                        Some(ids) => ("allowed", ids[0]),
                        None => ("different", ""),
                    }
                };
                out.push(Record {
                    corpus: label.clone(),
                    node: s.key.local.0,
                    case: case.to_owned(),
                    surface,
                    outcome,
                    deviation,
                    legacy: legacy_text,
                    new: new_text,
                    witness: String::new(),
                });
            }
        }
        drops.flush(&label, "all", surface, out);
        if seen == 0 {
            out.push(dropped(
                &label,
                "all",
                surface,
                "empty-corpus",
                1,
                "no node of the page has a fused entry".to_owned(),
            ));
        }
    }
}

fn emit(surface: &str, seed: u64, lang: gorca_present::Lang) -> Option<String> {
    // Hidden inputs of `render` are pinned: the catalog language is `lang` on this thread whatever
    // the process language is, and no extension registry is passed.
    let _lang = gorca_i18n::scoped(lang);
    let surface: &'static str = match surface {
        "whereami" => "whereami",
        _ => return None,
    };
    let run = Run { lang, seed };
    let mut recs = Vec::new();
    for family in FAMILIES {
        if !root().join(family.dir).is_dir() {
            continue;
        }
        records(family, surface, &run, &mut recs);
    }
    recs.sort_by(|a, b| {
        (
            &a.corpus,
            a.node,
            &a.case,
            a.surface,
            a.outcome,
            a.deviation,
            &a.legacy,
            &a.new,
        )
            .cmp(&(
                &b.corpus,
                b.node,
                &b.case,
                b.surface,
                b.outcome,
                b.deviation,
                &b.legacy,
                &b.new,
            ))
    });
    let mut text = String::new();
    for r in &recs {
        let _ = writeln!(
            text,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            r.corpus,
            r.node,
            r.case,
            r.surface,
            r.outcome,
            r.deviation,
            esc(&r.legacy),
            esc(&r.new),
            esc(&r.witness)
        );
    }
    Some(text)
}

fn lang_of(s: &str) -> Option<gorca_present::Lang> {
    match s {
        "en" => Some(gorca_present::Lang::En),
        "ru" => Some(gorca_present::Lang::Ru),
        _ => None,
    }
}

/// The gate's replay entry (run by `cargo xtask cutover`; a no-op otherwise).
#[test]
fn cutover_emit() {
    let (Ok(surface), Ok(path)) = (
        std::env::var("GORCA_CUTOVER_SURFACE"),
        std::env::var("GORCA_CUTOVER_OUT"),
    ) else {
        return;
    };
    let seed: u64 = std::env::var("GORCA_CUTOVER_SHUFFLE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let lang_name = std::env::var("GORCA_CUTOVER_LANG").unwrap_or_else(|_| "en".to_owned());
    let lang = lang_of(&lang_name).expect("GORCA_CUTOVER_LANG is en or ru");
    let text = emit(&surface, seed, lang).expect("surface has a replay");
    std::fs::write(path, text).expect("write records");
}

#[test]
fn the_replay_is_repeatable_and_order_free_on_the_checked_in_corpus() {
    for lang in [gorca_present::Lang::En, gorca_present::Lang::Ru] {
        let a = emit("whereami", 0, lang).unwrap();
        assert!(!a.is_empty(), "empty replay");
        assert_eq!(a, emit("whereami", 0, lang).unwrap(), "two runs differ");
        assert_eq!(
            a,
            emit("whereami", 0x5eed, lang).unwrap(),
            "shuffled arrival changed the output"
        );
    }
}

#[test]
fn every_family_has_recorded_pages_and_every_page_compares_something() {
    let text = emit("whereami", 0, gorca_present::Lang::En).unwrap();
    for f in FAMILIES {
        assert!(
            text.lines().any(|l| l.starts_with(&format!("{}/", f.dir))
                && l.split('\t').nth(4).is_none_or(|o| o != "dropped")),
            "family {} compared no node",
            f.dir
        );
    }
    assert!(
        !text
            .lines()
            .any(|l| l.split('\t').nth(5) == Some("empty-corpus")
                || l.split('\t').nth(5) == Some("page-unparsable")),
        "a recorded page is empty or unparsable"
    );
    let _ = BTreeSet::<u8>::new();
}
