// SPDX-License-Identifier: GPL-3.0-or-later
//! The cut-over gate (ADR-0022 section 8.9): `cargo xtask cutover <surface>`.
//!
//! Owner rule: before a surface moves from the legacy presenter to `gorca-present` (PresIr v1),
//! the difference between old and new output on every recorded corpus is zero, and whatever
//! remains is a dated, tagged, reasoned row of `crates/gorca-present/deviations.toml`. The gate
//!
//! 1. builds the replay test binary of `gorca-connector-atspi` (`cutover_gate_tests`), which feeds
//!    every corpus family of `tests/fixtures/sem` through both presenters and writes one canonical
//!    record per node;
//! 2. runs it three times per UI language (English, then Russian, the owner's) inside a private
//!    session (`tests/harness/run-isolated.sh`): twice plainly and once with a seeded shuffle of
//!    the arrival order and batching, and requires the three record files to be byte-identical
//!    (nondeterminism fails the gate);
//! 3. classifies each record: EQUAL, INTENDED (a deviation row for exactly this surface, not
//!    expired, with an `epoch` tag and a reason, whose predicate does not rest on a fused model
//!    fact that the source record contradicts) or UNCLASSIFIED; every node or case the replay did
//!    not compare is a DROP counted per reason, and a drop with no explanation fails the gate. It
//!    passes, in each language, with 0 UNCLASSIFIED, 0 unexplained drops and every required corpus
//!    family covered by compared nodes (or waived with a dated reason);
//! 4. prints the table and writes `bench/results/cutover/<surface>-<date>.md`, never overwriting
//!    an older report (a failing report is evidence and stays).
//!
//! `cargo xtask cutover --check` (also run by `cargo xtask legacy`) is the CI hook: every surface
//! is cut over (the legacy presentation APIs are banned outright, rule 12), so a passing, tracked
//! report made on an ancestor of `HEAD` must exist for each of them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use toml_edit::DocumentMut;

use crate::legacyrule::Date;

/// One surface of the cut-over plan (docs/reports/m22b1-plan.md).
pub struct Surface {
    pub name: &'static str,
    /// The `gorca-present` path and the replay exist.
    pub ready: bool,
    /// The package whose test binary holds the replay (empty: the replay was deleted with the
    /// legacy path it compared against).
    pub package: &'static str,
    /// The replay test, `--exact` path inside that binary.
    pub test: &'static str,
    /// The corpus families the surface applies to (empty: every family of the manifest).
    pub families: &'static [&'static str],
}

/// The families with a browse cursor: browse and quick navigation do not exist in the native
/// toolkits.
const WEB_FAMILIES: &[&str] = &["firefox", "chromium", "electron", "telegram"];

/// The families whose recordings hold a window, a dialog or an application top level. The Telegram
/// fixtures are page-only recordings of a web app (no window node above the document), so there
/// is no window lead-in to replay for them.
const WINDOW_FAMILIES: &[&str] = &[
    "gtk4",
    "firefox",
    "chromium",
    "gtk3",
    "electron",
    "gnome-shell",
    "vte",
];

/// The families whose recordings hold an alert or a dialog (the roles the Reader reads as a
/// notice). GTK 3, Electron, Telegram and the terminal recordings hold none.
const NOTICE_FAMILIES: &[&str] = &["gtk4", "firefox", "chromium", "gnome-shell"];

pub const SURFACES: &[Surface] = &[
    Surface {
        name: "focus",
        ready: true,
        package: "",
        test: "",
        families: &[],
    },
    Surface {
        name: "change",
        ready: true,
        package: "",
        test: "",
        families: &[],
    },
    Surface {
        name: "browse",
        ready: true,
        package: "",
        test: "",
        families: WEB_FAMILIES,
    },
    Surface {
        name: "quicknav",
        ready: true,
        package: "",
        test: "",
        families: WEB_FAMILIES,
    },
    Surface {
        name: "review",
        ready: true,
        package: "",
        test: "",
        families: &[],
    },
    Surface {
        name: "window",
        ready: true,
        package: "",
        test: "",
        families: WINDOW_FAMILIES,
    },
    Surface {
        name: "notification",
        ready: true,
        package: "",
        test: "",
        families: NOTICE_FAMILIES,
    },
    Surface {
        name: "whereami",
        ready: true,
        package: "",
        test: "",
        families: &[],
    },
    Surface {
        name: "docflags",
        ready: true,
        package: "",
        test: "",
        families: WEB_FAMILIES,
    },
    Surface {
        name: "explain",
        ready: true,
        package: "gorca-connector-atspi",
        test: "explain_gate_tests::explain_emit",
        families: &[],
    },
];

const REPORT_DIR: &str = "bench/results/cutover";
const DEVIATIONS: &str = "crates/gorca-present/deviations.toml";
const CORPORA: &str = "tests/fixtures/sem/cutover-corpora.toml";
const FIXTURES: &str = "tests/fixtures/sem";
/// The recorded state changes (`<family>/<case>.before|after.jsonl`) are inputs of the gate too.
const CHANGE_FIXTURES: &str = "tests/fixtures/sem-change";
/// The seed of the shuffled-arrival run (override with `--seed`).
const DEFAULT_SEED: u64 = 0x5eed;

fn repo_root() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    p.canonicalize().unwrap_or(p)
}

// ---------------------------------------------------------------------------------------------
// Records (the replay's output)
// ---------------------------------------------------------------------------------------------

/// One replayed node.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Rec {
    pub corpus: String,
    pub node: u64,
    pub case: String,
    pub surface: String,
    pub outcome: String,
    pub deviation: String,
    pub legacy: String,
    pub new: String,
    /// Set on an `allowed` record whose row rests on a fused model fact that the source record
    /// contradicts (empty otherwise).
    pub witness: String,
}

impl Rec {
    /// The corpus family: the directory under `tests/fixtures/sem`.
    pub fn family(&self) -> &str {
        self.corpus.split('/').next().unwrap_or("")
    }
}

/// Parse the replay's tab-separated records.
pub fn parse_records(text: &str) -> Result<Vec<Rec>> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 9 {
            bail!("record {}: want 9 fields, got {}", n + 1, f.len());
        }
        out.push(Rec {
            corpus: f[0].to_owned(),
            node: f[1]
                .parse()
                .with_context(|| format!("record {}: bad node id", n + 1))?,
            case: f[2].to_owned(),
            surface: f[3].to_owned(),
            outcome: f[4].to_owned(),
            deviation: f[5].to_owned(),
            legacy: f[6].to_owned(),
            new: f[7].to_owned(),
            witness: f[8].to_owned(),
        });
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Deviation rows
// ---------------------------------------------------------------------------------------------

/// A row of `deviations.toml` as the gate needs it.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: String,
    pub surface: String,
    pub until: Date,
    pub epoch: String,
    pub reason: String,
}

/// Parse every `[[deviation]]` row. A row missing a field is kept (with an empty value) so that
/// the gate can name it as unusable instead of silently ignoring it.
pub fn parse_rows(src: &str) -> Result<Vec<Row>> {
    let doc: DocumentMut = src.parse().context("deviations.toml is not valid TOML")?;
    let mut rows = Vec::new();
    if let Some(tables) = doc.get("deviation").and_then(|i| i.as_array_of_tables()) {
        for t in tables {
            let s = |k: &str| {
                t.get(k)
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            rows.push(Row {
                id: s("id"),
                surface: s("surface"),
                until: Date::parse(&s("until")).unwrap_or(Date(0, 0, 0)),
                epoch: s("epoch"),
                reason: s("reason"),
            });
        }
    }
    Ok(rows)
}

/// Why a row cannot make a difference INTENDED, or `None` when it can.
fn row_defect(r: &Row, today: Date) -> Option<&'static str> {
    if r.until < today {
        Some("row expired")
    } else if r.epoch.trim().is_empty() {
        Some("row has no `epoch` tag")
    } else if r.reason.trim().is_empty() {
        Some("row has no reason")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------------------------
// Classification
// ---------------------------------------------------------------------------------------------

/// The gate's verdict on one record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Class {
    Equal,
    /// Both presenters say nothing (counted as equal, shown separately).
    Silent,
    Intended(String),
    Unclassified(String),
    /// The replay did not compare this (reason code). Counted apart from the nodes.
    Dropped(String),
}

pub fn classify(rec: &Rec, rows: &[Row], today: Date) -> Class {
    match rec.outcome.as_str() {
        "equal" => Class::Equal,
        "silent" => Class::Silent,
        "allowed" => {
            let matching: Vec<&Row> = rows
                .iter()
                .filter(|r| r.id == rec.deviation && r.surface == rec.surface)
                .collect();
            let Some(first) = matching.first() else {
                return Class::Unclassified(format!(
                    "no row `{}` for surface `{}`",
                    rec.deviation, rec.surface
                ));
            };
            if matching.iter().any(|r| row_defect(r, today).is_none()) {
                // A row must not justify a regression with the model fact that causes it: when
                // the source record contradicts the fused fact the row's predicate read, the
                // difference is a model defect, not an intended improvement.
                if !rec.witness.is_empty() {
                    return Class::Unclassified(format!(
                        "row `{}` rests on a fused model fact the source record contradicts: {}",
                        rec.deviation, rec.witness
                    ));
                }
                return Class::Intended(format!(
                    "{}@{} [{}]",
                    first.id, first.surface, first.epoch
                ));
            }
            let defect = row_defect(first, today).unwrap_or("row unusable");
            Class::Unclassified(format!(
                "{defect}: `{}` on `{}`",
                rec.deviation, rec.surface
            ))
        }
        "dropped" => Class::Dropped(rec.deviation.clone()),
        "order-only" => Class::Unclassified("same tokens, different order".to_owned()),
        "different" => Class::Unclassified("tokens differ".to_owned()),
        other => Class::Unclassified(format!("unknown outcome `{other}`")),
    }
}

/// Counts per family and in total.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Tally {
    pub nodes: u64,
    pub equal: u64,
    pub silent: u64,
    pub intended: u64,
    pub unclassified: u64,
}

impl Tally {
    fn add(&mut self, c: &Class) {
        // Drops are counted by reason (`Verdict::drops`), never as compared nodes.
        if matches!(c, Class::Dropped(_)) {
            return;
        }
        self.nodes += 1;
        match c {
            Class::Equal => self.equal += 1,
            Class::Silent => self.silent += 1,
            Class::Intended(_) => self.intended += 1,
            Class::Unclassified(_) => self.unclassified += 1,
            Class::Dropped(_) => {}
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Corpus manifest
// ---------------------------------------------------------------------------------------------

/// One corpus family of `cutover-corpora.toml`.
#[derive(Clone, Debug)]
pub struct FamilySpec {
    pub name: String,
    pub waived_until: Option<Date>,
    pub waiver: String,
    /// Recorded real changes that show no change in the engine's tree (case, reason): known gaps
    /// of the engine, kept as evidence. A `case-without-change` drop not listed here is
    /// unexplained.
    pub no_change: Vec<(String, String)>,
}

pub fn parse_manifest(src: &str) -> Result<Vec<FamilySpec>> {
    let doc: DocumentMut = src
        .parse()
        .context("cutover-corpora.toml is not valid TOML")?;
    let mut out = Vec::new();
    if let Some(tables) = doc.get("family").and_then(|i| i.as_array_of_tables()) {
        for t in tables {
            let s = |k: &str| t.get(k).and_then(|v| v.as_str()).unwrap_or_default();
            if s("name").is_empty() {
                bail!("cutover-corpora.toml: a family has no name");
            }
            let waived_until = match s("waived_until") {
                "" => None,
                d => Some(
                    Date::parse(d)
                        .with_context(|| format!("family {}: bad waived_until", s("name")))?,
                ),
            };
            if waived_until.is_some() && s("waiver").trim().is_empty() {
                bail!("family {}: a waiver needs a reason", s("name"));
            }
            let mut no_change = Vec::new();
            if let Some(list) = t.get("no_change").and_then(|i| i.as_array_of_tables()) {
                for n in list {
                    let g = |k: &str| n.get(k).and_then(|v| v.as_str()).unwrap_or_default();
                    if g("case").is_empty() || g("reason").trim().is_empty() {
                        bail!(
                            "family {}: a no_change entry needs a case and a reason",
                            s("name")
                        );
                    }
                    no_change.push((g("case").to_owned(), g("reason").to_owned()));
                }
            }
            out.push(FamilySpec {
                name: s("name").to_owned(),
                waived_until,
                waiver: s("waiver").to_owned(),
                no_change,
            });
        }
    }
    Ok(out)
}

/// Coverage verdict for one family.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Coverage {
    Covered {
        corpora: usize,
    },
    Waived(String),
    /// Required, absent, no valid waiver.
    Missing,
}

pub fn coverage(specs: &[FamilySpec], recs: &[Rec], today: Date) -> Vec<(String, Coverage)> {
    let mut corpora: BTreeMap<&str, BTreeSet<&str>> = BTreeMap::new();
    for r in recs.iter().filter(|r| r.outcome != "dropped") {
        corpora.entry(r.family()).or_default().insert(&r.corpus);
    }
    specs
        .iter()
        .map(|s| {
            let c = match corpora.get(s.name.as_str()) {
                Some(set) if !set.is_empty() => Coverage::Covered { corpora: set.len() },
                _ => match s.waived_until {
                    Some(d) if d >= today => Coverage::Waived(format!("until {d}: {}", s.waiver)),
                    _ => Coverage::Missing,
                },
            };
            (s.name.clone(), c)
        })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Determinism
// ---------------------------------------------------------------------------------------------

/// FNV-1a 64 of a byte string (stable across Rust versions, unlike `DefaultHasher`).
pub fn fnv64(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

/// The first lines where two record files differ (empty when byte-identical).
pub fn first_differences(a: &str, b: &str, limit: usize) -> Vec<String> {
    if a == b {
        return Vec::new();
    }
    let (la, lb): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let mut out = Vec::new();
    for i in 0..la.len().max(lb.len()) {
        let (x, y) = (la.get(i).copied(), lb.get(i).copied());
        if x != y {
            out.push(format!(
                "line {}: `{}` vs `{}`",
                i + 1,
                x.unwrap_or("<missing>"),
                y.unwrap_or("<missing>")
            ));
            if out.len() >= limit {
                break;
            }
        }
    }
    if out.is_empty() {
        out.push("files differ in line endings or trailing bytes".to_owned());
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The verdict and the report
// ---------------------------------------------------------------------------------------------

/// One reason a replay did not compare a node or a case, and why that is acceptable. A reason that
/// is not listed here is an *unexplained* drop and fails the gate, whatever else passes.
pub const DROP_REASONS: &[(&str, &str)] = &[
    (
        "unreadable-node",
        "the recorder's calls on the node failed (dead object or timeout): there is no role to present",
    ),
    (
        "native-node-in-web-family",
        "application or browser chrome around the web content (menus, frame buttons): the family is fused with the web tables, so a native comparison would be unsound",
    ),
    (
        "web-node-in-native-family",
        "web content embedded in a native application",
    ),
    (
        "static-leaf-no-entry",
        "a text leaf that is merged into its parent's content name and is never a focus target",
    ),
    (
        "mutation-no-target",
        "synthetic mutation: no focusable node of the page has a state of that kind to flip",
    ),
    (
        "mutation-not-focusable",
        "synthetic mutation: the state could flip on a node that is not focusable; a change utterance is spoken for the focused node, so it is not replayed",
    ),
    (
        "trigger-button",
        "the do:* button the recorder pressed to make the change; its own focus change is not the change under test",
    ),
    (
        "node-added",
        "the real change added the node (children of an expanded item, a popup); a new node is not a change of an existing one",
    ),
    (
        "node-removed",
        "the real change removed the node; there is nothing left to speak about",
    ),
    (
        "not-a-window-role",
        "a node that is no window, dialog, alert or application top level: the Reader builds a window lead-in only for the activated window",
    ),
    (
        "not-a-notice-role",
        "a node that is neither an alert nor a dialog: a notice is read for an alert, and for a dialog when the application's policy says so",
    ),
    (
        "ancestor-no-entry",
        "an ancestor of the node is a text leaf or unreadable and has no fused entry, so the ancestry cannot be composed",
    ),
    (
        "hidden-node",
        "a hidden node: quick navigation never lands on one, so it has no description to compare",
    ),
];

/// The explanation of a drop reason, `None` when the gate has none.
pub fn drop_explanation(reason: &str) -> Option<&'static str> {
    DROP_REASONS
        .iter()
        .find(|(r, _)| *r == reason)
        .map(|(_, why)| *why)
}

/// Drops of one (family, reason).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DropRow {
    pub family: String,
    pub reason: String,
    pub count: u64,
    /// The explanation, `None` for an unexplained drop.
    pub explanation: Option<String>,
    pub example: String,
}

/// Everything the report needs for one language.
#[derive(Clone)]
pub struct Verdict {
    pub surface: String,
    /// `en` or `ru`: the catalog language the replay ran in.
    pub lang: String,
    pub date: Date,
    pub total: Tally,
    pub per_family: BTreeMap<String, Tally>,
    pub coverage: Vec<(String, Coverage)>,
    pub deterministic: bool,
    pub nondeterminism: Vec<String>,
    pub intended: BTreeMap<String, u64>,
    /// (case, surface, masked legacy, masked new, reason) -> (count, first example).
    pub unclassified: BTreeMap<String, (u64, String)>,
    /// Every node and case the replay did not compare.
    pub drops: Vec<DropRow>,
    pub not_ready: bool,
    pub run_hash: u64,
}

impl Verdict {
    pub fn dropped(&self) -> u64 {
        self.drops.iter().map(|d| d.count).sum()
    }

    /// Drops no listed reason (or manifest entry) explains: each one fails the gate.
    pub fn unexplained_drops(&self) -> u64 {
        self.drops
            .iter()
            .filter(|d| d.explanation.is_none())
            .map(|d| d.count)
            .sum()
    }

    pub fn pass(&self) -> bool {
        !self.not_ready
            && self.total.unclassified == 0
            && self.unexplained_drops() == 0
            && self.total.nodes > 0
            && self.deterministic
            && !self
                .coverage
                .iter()
                .any(|(_, c)| matches!(c, Coverage::Missing))
    }
}

/// The gate passes only when every language passes.
pub fn all_pass(vs: &[Verdict]) -> bool {
    !vs.is_empty() && vs.iter().all(Verdict::pass)
}

/// Mask page and app content so differences group by their wording, as the replay tables do.
fn mask(s: &str) -> String {
    s.split(" | ")
        .map(|t| match t.split_once(':') {
            Some((k @ ("Text" | "Value"), _)) => format!("{k}:*"),
            _ => t.to_owned(),
        })
        .collect::<Vec<_>>()
        .join(" | ")
}

/// Build the verdict from the plain run's records, the determinism comparison and the manifest.
#[allow(clippy::too_many_arguments)]
pub fn judge(
    surface: &str,
    lang: &str,
    recs: &[Rec],
    rows: &[Row],
    specs: &[FamilySpec],
    nondeterminism: Vec<String>,
    today: Date,
    run_hash: u64,
) -> Verdict {
    let mut total = Tally::default();
    let mut per_family: BTreeMap<String, Tally> = BTreeMap::new();
    let mut intended: BTreeMap<String, u64> = BTreeMap::new();
    let mut unclassified: BTreeMap<String, (u64, String)> = BTreeMap::new();
    let mut drops: BTreeMap<(String, String, String), DropRow> = BTreeMap::new();
    for r in recs {
        let c = classify(r, rows, today);
        if let Class::Dropped(reason) = &c {
            let n: u64 = r.legacy.parse().unwrap_or(1);
            let explanation = drop_explained(r, reason, specs);
            // A drop with a manifest explanation is keyed by its case so that the report names it.
            let case_key = if reason == "case-without-change" {
                r.corpus.clone()
            } else {
                String::new()
            };
            let e = drops
                .entry((r.family().to_owned(), reason.clone(), case_key))
                .or_insert_with(|| DropRow {
                    family: r.family().to_owned(),
                    reason: reason.clone(),
                    count: 0,
                    explanation,
                    example: format!("{} {}", r.corpus, r.new),
                });
            e.count += n;
            continue;
        }
        total.add(&c);
        per_family.entry(r.family().to_owned()).or_default().add(&c);
        match &c {
            Class::Intended(id) => *intended.entry(id.clone()).or_default() += 1,
            Class::Unclassified(why) => {
                let sig = format!(
                    "{} / {}: [{}] -> [{}] ({why})",
                    r.case,
                    r.surface,
                    mask(&r.legacy),
                    mask(&r.new)
                );
                let e = unclassified.entry(sig).or_insert_with(|| {
                    (
                        0,
                        format!(
                            "{}#{} legacy=`{}` new=`{}`",
                            r.corpus, r.node, r.legacy, r.new
                        ),
                    )
                });
                e.0 += 1;
            }
            Class::Equal | Class::Silent | Class::Dropped(_) => {}
        }
    }
    Verdict {
        surface: surface.to_owned(),
        lang: lang.to_owned(),
        date: today,
        total,
        per_family,
        coverage: coverage(specs, recs, today),
        deterministic: nondeterminism.is_empty(),
        nondeterminism,
        intended,
        unclassified,
        drops: drops.into_values().collect(),
        not_ready: false,
        run_hash,
    }
}

/// Why a drop is acceptable: the listed reason, or for a real case that changed nothing the
/// manifest entry that names the (family, case) and says why the engine shows no change.
fn drop_explained(r: &Rec, reason: &str, specs: &[FamilySpec]) -> Option<String> {
    if reason == "case-without-change" {
        let case = r.case.strip_prefix("real-").unwrap_or(&r.case);
        return specs
            .iter()
            .find(|s| s.name == r.family())
            .and_then(|s| s.no_change.iter().find(|(c, _)| c == case))
            .map(|(_, why)| format!("known: {why}"));
    }
    drop_explanation(reason).map(str::to_owned)
}

fn yes_no(b: bool) -> &'static str {
    if b { "yes" } else { "NO" }
}

fn covered_count(v: &Verdict) -> (usize, usize) {
    let covered = v
        .coverage
        .iter()
        .filter(|(_, c)| matches!(c, Coverage::Covered { .. }))
        .count();
    (covered, v.coverage.len())
}

/// The short table printed on the terminal and placed at the top of the report: one row per
/// language.
pub fn table(vs: &[Verdict]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| surface | lang | corpora | compared | equal | intended | unclassified | dropped | unexplained drops | deterministic |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|---|");
    for v in vs {
        let (c, n) = covered_count(v);
        let _ = writeln!(
            s,
            "| {} | {} | {c}/{n} families | {} | {} | {} | {} | {} | {} | {} |",
            v.surface,
            v.lang,
            v.total.nodes,
            v.total.equal + v.total.silent,
            v.total.intended,
            v.total.unclassified,
            v.dropped(),
            v.unexplained_drops(),
            yes_no(v.deterministic)
        );
    }
    s
}

/// Everything an `inputs` line hashes: the deviations, the manifest and every fixture file.
pub fn inputs_hash(root: &Path) -> Result<u64> {
    let mut files = vec![root.join(DEVIATIONS), root.join(CORPORA)];
    let mut stack = vec![root.join(FIXTURES), root.join(CHANGE_FIXTURES)];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).with_context(|| format!("reading {}", d.display()))? {
            let p = e?.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "jsonl") {
                files.push(p);
            }
        }
    }
    files.sort();
    files.dedup();
    let mut h = 0xcbf2_9ce4_8422_2325_u64;
    for f in files {
        let rel = f.strip_prefix(root).unwrap_or(&f).display().to_string();
        let body = std::fs::read(&f).with_context(|| format!("reading {rel}"))?;
        h ^= fnv64(rel.as_bytes());
        h = h.wrapping_mul(0x0000_0100_0000_01b3) ^ fnv64(&body);
    }
    Ok(h)
}

/// One language's sections of the report.
fn language_section(v: &Verdict, s: &mut String) {
    let _ = writeln!(
        s,
        "## Language: {} ({})",
        v.lang,
        if v.pass() { "PASS" } else { "FAIL" }
    );
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "compared {} nodes: {} equal (including {} where both presenters say nothing), {} intended, {} unclassified; {} dropped, {} unexplained.",
        v.total.nodes,
        v.total.equal + v.total.silent,
        v.total.silent,
        v.total.intended,
        v.total.unclassified,
        v.dropped(),
        v.unexplained_drops()
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "### Corpora ({})", v.lang);
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "| family | status | compared | equal | intended | unclassified |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|");
    for (name, c) in &v.coverage {
        let t = v.per_family.get(name).cloned().unwrap_or_default();
        let status = match c {
            Coverage::Covered { corpora } => format!("covered ({corpora} corpora)"),
            Coverage::Waived(why) => format!("WAIVED {why}"),
            Coverage::Missing => "MISSING".to_owned(),
        };
        let _ = writeln!(
            s,
            "| {name} | {status} | {} | {} | {} | {} |",
            t.nodes,
            t.equal + t.silent,
            t.intended,
            t.unclassified
        );
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "### Dropped nodes and cases ({})", v.lang);
    let _ = writeln!(s);
    if v.drops.is_empty() {
        let _ = writeln!(s, "None.");
    } else {
        let _ = writeln!(s, "| family | reason | count | explanation |");
        let _ = writeln!(s, "|---|---|---|---|");
        for d in &v.drops {
            let why = d.explanation.as_deref().unwrap_or("UNEXPLAINED");
            let _ = writeln!(s, "| {} | {} | {} | {} |", d.family, d.reason, d.count, why);
        }
        let _ = writeln!(s);
        let _ = writeln!(
            s,
            "total dropped: {}; unexplained: {} (must be 0 to PASS).",
            v.dropped(),
            v.unexplained_drops()
        );
        for d in v.drops.iter().filter(|d| d.explanation.is_none()) {
            let _ = writeln!(
                s,
                "- unexplained `{}` in {}: e.g. {}",
                d.reason, d.family, d.example
            );
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "### Determinism ({})", v.lang);
    let _ = writeln!(s);
    if v.deterministic {
        let _ = writeln!(
            s,
            "Two plain runs and one run with shuffled arrival order and batching produced \
             byte-identical records."
        );
    } else {
        let _ = writeln!(s, "NONDETERMINISM, the gate fails. First differences:");
        let _ = writeln!(s);
        for l in &v.nondeterminism {
            let _ = writeln!(s, "- {l}");
        }
    }
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "### Intended differences ({}, deviations.toml, epoch change)",
        v.lang
    );
    let _ = writeln!(s);
    if v.intended.is_empty() {
        let _ = writeln!(s, "None.");
    }
    for (id, n) in &v.intended {
        let _ = writeln!(s, "- `{id}`: {n}");
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "### Unclassified differences ({})", v.lang);
    let _ = writeln!(s);
    if v.unclassified.is_empty() {
        let _ = writeln!(s, "None.");
    }
    let mut rows: Vec<_> = v.unclassified.iter().collect();
    rows.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(b.0)));
    for (sig, (n, ex)) in rows.iter().take(60) {
        let _ = writeln!(s, "- {n}x {sig}\n  - e.g. {ex}");
    }
    if rows.len() > 60 {
        let _ = writeln!(
            s,
            "- ... and {} more groups (see the .unclassified.tsv)",
            rows.len() - 60
        );
    }
    let _ = writeln!(s);
}

/// The Markdown report over every language. `meta` carries the conditions (rev, load, governor,
/// seed). The overall result is PASS only when every language passes.
pub fn report(vs: &[Verdict], meta: &[(String, String)]) -> String {
    let mut s = String::new();
    let surface = vs.first().map_or("", |v| v.surface.as_str());
    let _ = writeln!(s, "# Cut-over gate: {surface}");
    let _ = writeln!(s);
    let _ = writeln!(s, "result: {}", if all_pass(vs) { "PASS" } else { "FAIL" });
    let _ = writeln!(s, "surface: {surface}");
    if let Some(v) = vs.first() {
        let _ = writeln!(s, "date: {}", v.date);
    }
    let _ = writeln!(
        s,
        "languages: {}",
        vs.iter()
            .map(|v| v.lang.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    for (k, val) in meta {
        let _ = writeln!(s, "{k}: {val}");
    }
    for v in vs {
        let _ = writeln!(s, "records-{}: {:016x}", v.lang, v.run_hash);
    }
    let _ = writeln!(s);
    if vs.iter().any(|v| v.not_ready) {
        let _ = writeln!(
            s,
            "This surface has no `gorca-present` path yet, so there is nothing to replay. The gate \
             fails by design until its slice (docs/reports/m22b1-plan.md) lands the path and the replay."
        );
        let _ = writeln!(s);
    }
    s.push_str(&table(vs));
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "The gate runs the replay once per UI language (English and Russian, the owner's) and \
         passes only when both pass. Nodes the replay did not compare are counted per reason \
         below; a drop with no explanation fails the gate. A row that rests on a fused model fact \
         the source record contradicts is not accepted as intended."
    );
    let _ = writeln!(s);
    for v in vs {
        language_section(v, &mut s);
    }
    s
}

/// A free report path for `surface` on `date`: never an existing file.
pub fn report_path(dir: &Path, surface: &str, date: Date) -> PathBuf {
    let first = dir.join(format!("{surface}-{date}.md"));
    if !first.exists() {
        return first;
    }
    (2..10_000)
        .map(|n| dir.join(format!("{surface}-{date}-{n}.md")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

// ---------------------------------------------------------------------------------------------
// Running the replay
// ---------------------------------------------------------------------------------------------

fn build_test_binary(root: &Path, package: &str) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args([
            "test",
            "-p",
            package,
            "--lib",
            "--no-run",
            "--message-format=json-render-diagnostics",
        ])
        .stderr(Stdio::inherit())
        .output()
        .context("running cargo test --no-run")?;
    if !out.status.success() {
        bail!("building the replay test binary failed");
    }
    let mut found = None;
    for msg in cargo_metadata::Message::parse_stream(out.stdout.as_slice()) {
        if let cargo_metadata::Message::CompilerArtifact(a) = msg.context("cargo output")?
            && a.target.name == package.replace('-', "_")
            && a.profile.test
            && let Some(exe) = a.executable
        {
            found = Some(PathBuf::from(exe.as_str()));
        }
    }
    found.with_context(|| format!("cargo did not report the test executable of {package}"))
}

/// One replay inside a private session; returns the record file's text.
/// The UI languages the gate replays in. The owner's language is Russian; both must pass.
pub const LANGS: &[&str] = &["en", "ru"];

#[allow(clippy::too_many_arguments)]
fn replay_once(
    root: &Path,
    bin: &Path,
    test: &str,
    surface: &str,
    lang: &str,
    seed: u64,
    scratch: &Path,
    tag: &str,
) -> Result<String> {
    let out = scratch.join(format!("{surface}-{lang}-{tag}.tsv"));
    let status = Command::new(root.join("tests/harness/run-isolated.sh"))
        .current_dir(root)
        .arg("--")
        .arg(bin)
        .args(["--exact", test, "--nocapture"])
        .env(
            "GORCA_PASSTHROUGH",
            "GORCA_CUTOVER_SURFACE GORCA_CUTOVER_OUT GORCA_CUTOVER_SHUFFLE GORCA_CUTOVER_LANG",
        )
        .env("GORCA_CUTOVER_LANG", lang)
        .env("GORCA_CUTOVER_SURFACE", surface)
        .env("GORCA_CUTOVER_OUT", &out)
        .env("GORCA_CUTOVER_SHUFFLE", seed.to_string())
        .status()
        .context("starting tests/harness/run-isolated.sh")?;
    if !status.success() {
        bail!("replay `{lang}/{tag}` failed ({status}) inside the private session");
    }
    std::fs::read_to_string(&out).with_context(|| format!("replay `{lang}/{tag}` wrote no records"))
}

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_owned())
}

fn conditions(root: &Path, seed: u64, inputs: u64) -> Vec<(String, String)> {
    let rev = git(root, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".to_owned());
    let dirty = git(root, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let load = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let governor = std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .unwrap_or_else(|_| "n/a".to_owned());
    vec![
        ("rev".to_owned(), rev),
        (
            "dirty".to_owned(),
            if dirty {
                "yes (gate ran before the commit)"
            } else {
                "no"
            }
            .to_owned(),
        ),
        ("inputs".to_owned(), format!("{inputs:016x}")),
        ("shuffle_seed".to_owned(), format!("{seed:#x}")),
        ("loadavg".to_owned(), load.trim().to_owned()),
        ("governor".to_owned(), governor.trim().to_owned()),
    ]
}

fn surface_of(name: &str) -> Result<&'static Surface> {
    SURFACES.iter().find(|s| s.name == name).with_context(|| {
        format!(
            "unknown surface `{name}` (want one of: {})",
            SURFACES
                .iter()
                .map(|s| s.name)
                .collect::<Vec<_>>()
                .join(", ")
        )
    })
}

fn write_report(
    root: &Path,
    vs: &[Verdict],
    meta: &[(String, String)],
    unclassified_tsv: &str,
) -> Result<PathBuf> {
    let dir = root.join(REPORT_DIR);
    std::fs::create_dir_all(&dir)?;
    let first = vs.first().context("no verdict to report")?;
    let path = report_path(&dir, &first.surface, first.date);
    std::fs::write(&path, report(vs, meta))?;
    if !unclassified_tsv.is_empty() {
        std::fs::write(path.with_extension("unclassified.tsv"), unclassified_tsv)?;
    }
    Ok(path)
}

/// `cargo xtask cutover <surface> [--seed N]` or `cargo xtask cutover --check`.
pub fn run(mut args: impl Iterator<Item = String>) -> Result<bool> {
    let first = args.next().context(
        "usage: cargo xtask cutover <surface> [--seed N] | --check   (surfaces: focus, change, browse, quicknav, review, window, notification, whereami, docflags, explain)",
    )?;
    if first == "--check" {
        return Ok(check());
    }
    let surface = surface_of(&first)?;
    let mut seed = DEFAULT_SEED;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--seed" => {
                seed = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .filter(|s| *s != 0)
                    .context("--seed wants a non-zero integer")?;
            }
            other => bail!("unknown option `{other}`"),
        }
    }
    let root = repo_root();
    let today = Date::today()?;
    let inputs = inputs_hash(&root)?;
    let meta = conditions(&root, seed, inputs);

    if !surface.ready {
        let v = Verdict {
            surface: surface.name.to_owned(),
            lang: "en".to_owned(),
            date: today,
            total: Tally::default(),
            per_family: BTreeMap::new(),
            coverage: Vec::new(),
            deterministic: true,
            nondeterminism: Vec::new(),
            intended: BTreeMap::new(),
            unclassified: BTreeMap::new(),
            drops: Vec::new(),
            not_ready: true,
            run_hash: 0,
        };
        let vs = [v];
        let p = write_report(&root, &vs, &meta, "")?;
        print!("{}", table(&vs));
        eprintln!(
            "cutover {}: FAIL, no gorca-present path yet; report {}",
            surface.name,
            p.display()
        );
        return Ok(false);
    }

    let rows = parse_rows(&std::fs::read_to_string(root.join(DEVIATIONS))?)?;
    let specs = parse_manifest(&std::fs::read_to_string(root.join(CORPORA))?)?;
    if surface.package.is_empty() {
        bail!(
            "the replay of `{}` was deleted with its legacy presenter (the cut-over commit); its goldens are the guard now",
            surface.name
        );
    }
    let specs: Vec<FamilySpec> = specs
        .into_iter()
        .filter(|f| surface.families.is_empty() || surface.families.contains(&f.name.as_str()))
        .collect();
    let bin = build_test_binary(&root, surface.package)?;
    let scratch = std::env::temp_dir().join(format!("gorca-cutover-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let mut verdicts = Vec::new();
    let mut bad = String::new();
    for lang in LANGS {
        let a = replay_once(
            &root,
            &bin,
            surface.test,
            surface.name,
            lang,
            0,
            &scratch,
            "a",
        )?;
        let b = replay_once(
            &root,
            &bin,
            surface.test,
            surface.name,
            lang,
            0,
            &scratch,
            "b",
        )?;
        let c = replay_once(
            &root,
            &bin,
            surface.test,
            surface.name,
            lang,
            seed,
            &scratch,
            "shuffled",
        )?;
        let mut nondeterminism = Vec::new();
        for l in first_differences(&a, &b, 10) {
            nondeterminism.push(format!("two plain runs: {l}"));
        }
        for l in first_differences(&a, &c, 10) {
            nondeterminism.push(format!("shuffled arrival (seed {seed:#x}): {l}"));
        }
        let recs = parse_records(&a)?;
        verdicts.push(judge(
            surface.name,
            lang,
            &recs,
            &rows,
            &specs,
            nondeterminism,
            today,
            fnv64(a.as_bytes()),
        ));
        for r in &recs {
            if matches!(classify(r, &rows, today), Class::Unclassified(_)) {
                let _ = writeln!(
                    bad,
                    "{lang}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                    r.corpus, r.node, r.case, r.surface, r.outcome, r.legacy, r.new, r.witness
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&scratch);
    let p = write_report(&root, &verdicts, &meta, &bad)?;
    print!("{}", table(&verdicts));
    let pass = all_pass(&verdicts);
    println!(
        "cutover {}: {} (report {})",
        surface.name,
        if pass { "PASS" } else { "FAIL" },
        p.display()
    );
    Ok(pass)
}

// ---------------------------------------------------------------------------------------------
// The CI hook
// ---------------------------------------------------------------------------------------------

/// What the CI hook reads from a report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReportMeta {
    pub file: String,
    pub surface: String,
    pub pass: bool,
    pub rev: String,
}

/// Read the front matter of a report; `None` when it is not a gate report.
pub fn parse_report(file: &str, text: &str) -> Option<ReportMeta> {
    let field = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k).and_then(|r| r.strip_prefix(": ")))
            .map(|v| v.trim().to_owned())
    };
    Some(ReportMeta {
        file: file.to_owned(),
        surface: field("surface")?,
        pass: field("result")? == "PASS",
        rev: field("rev")?,
    })
}

/// Every surface is cut over: each needs a passing report. `tracked` says whether git tracks a
/// report and `ancestor` whether a rev is an ancestor of HEAD.
pub fn judge_reports(
    reports: &[ReportMeta],
    tracked: &dyn Fn(&str) -> bool,
    ancestor: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for s in SURFACES {
        let ok = reports
            .iter()
            .any(|r| r.surface == s.name && r.pass && tracked(&r.file) && ancestor(&r.rev));
        if !ok {
            out.push(format!(
                "cut-over: surface `{}` is cut over, but {REPORT_DIR}/ has no committed PASS report \
                 made on an ancestor of HEAD; run `cargo xtask cutover {}` before the cut-over \
                 commit and commit the report",
                s.name, s.name
            ));
        }
    }
    out
}

/// Rule 12's extension: violations as one line each (empty when the hook is satisfied).
pub fn check_reports(root: &Path) -> Vec<String> {
    let dir = root.join(REPORT_DIR);
    let mut reports = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.extension().is_some_and(|x| x == "md")
                && let Ok(text) = std::fs::read_to_string(&p)
                && let Some(m) = parse_report(
                    &format!("{REPORT_DIR}/{}", e.file_name().to_string_lossy()),
                    &text,
                )
            {
                reports.push(m);
            }
        }
    }
    reports.sort_by(|a, b| a.file.cmp(&b.file));
    let tracked = |f: &str| git(root, &["ls-files", "--error-unmatch", f]).is_some();
    let ancestor = |rev: &str| {
        Command::new("git")
            .current_dir(root)
            .args(["merge-base", "--is-ancestor", rev, "HEAD"])
            .status()
            .is_ok_and(|s| s.success())
    };
    judge_reports(&reports, &tracked, &ancestor)
}

fn check() -> bool {
    let root = repo_root();
    let problems = check_reports(&root);
    for p in &problems {
        eprintln!("rule 12: {p}");
    }
    if problems.is_empty() {
        println!("cutover check: ok (no cut-over surface lacks a passing gate report)");
    }
    problems.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Date = Date(2026, 10, 24);

    fn rec(outcome: &str, dev: &str, surface: &str) -> Rec {
        Rec {
            corpus: "gtk4/controls".to_owned(),
            node: 7,
            case: "focus".to_owned(),
            surface: surface.to_owned(),
            outcome: outcome.to_owned(),
            deviation: dev.to_owned(),
            legacy: "Text:Save | Role:button".to_owned(),
            new: "Text:Save | Role:button".to_owned(),
            witness: String::new(),
        }
    }

    fn dropped(family: &str, reason: &str, n: u64) -> Rec {
        Rec {
            corpus: format!("{family}/page"),
            node: 0,
            case: "focus".to_owned(),
            surface: "focus".to_owned(),
            outcome: "dropped".to_owned(),
            deviation: reason.to_owned(),
            legacy: n.to_string(),
            new: "example".to_owned(),
            witness: String::new(),
        }
    }

    fn row(id: &str, surface: &str, until: Date, epoch: &str, reason: &str) -> Row {
        Row {
            id: id.to_owned(),
            surface: surface.to_owned(),
            until,
            epoch: epoch.to_owned(),
            reason: reason.to_owned(),
        }
    }

    fn good() -> Row {
        row(
            "role-from-model",
            "focus",
            Date(2026, 11, 7),
            "m22b1",
            "intended",
        )
    }

    #[test]
    fn equal_and_silent_are_equal() {
        assert_eq!(
            classify(&rec("equal", "", "focus"), &[], TODAY),
            Class::Equal
        );
        assert_eq!(
            classify(&rec("silent", "", "focus"), &[], TODAY),
            Class::Silent
        );
    }

    #[test]
    fn a_difference_is_intended_only_with_a_narrow_dated_tagged_row() {
        let r = rec("allowed", "role-from-model", "focus");
        assert!(matches!(classify(&r, &[good()], TODAY), Class::Intended(_)));
        // The same id on another surface does not count.
        let other = row(
            "role-from-model",
            "focus-web",
            Date(2026, 11, 7),
            "m22b1",
            "x",
        );
        assert!(matches!(
            classify(&r, &[other], TODAY),
            Class::Unclassified(_)
        ));
        // No row at all.
        assert!(matches!(classify(&r, &[], TODAY), Class::Unclassified(_)));
    }

    #[test]
    fn an_expired_or_untagged_or_unreasoned_row_does_not_make_a_difference_intended() {
        let r = rec("allowed", "role-from-model", "focus");
        let expired = row("role-from-model", "focus", Date(2026, 10, 23), "m22b1", "r");
        let untagged = row("role-from-model", "focus", Date(2026, 11, 7), "", "r");
        let unreasoned = row("role-from-model", "focus", Date(2026, 11, 7), "m22b1", " ");
        for bad in [expired, untagged, unreasoned] {
            assert!(
                matches!(classify(&r, &[bad], TODAY), Class::Unclassified(_)),
                "a defective row must not classify"
            );
        }
        // The last day of a row is still valid.
        let edge = row("role-from-model", "focus", TODAY, "m22b1", "r");
        assert!(matches!(classify(&r, &[edge], TODAY), Class::Intended(_)));
    }

    #[test]
    fn different_and_order_only_are_unclassified() {
        for o in ["different", "order-only", "surprise"] {
            assert!(matches!(
                classify(&rec(o, "", "focus"), &[good()], TODAY),
                Class::Unclassified(_)
            ));
        }
    }

    #[test]
    fn records_round_trip_and_reject_bad_lines() {
        let t = "gtk4/controls\t7\tfocus\tfocus\tequal\t\tText:a\tText:a\t\n";
        let r = parse_records(t).expect("parses");
        assert_eq!((r[0].node, r[0].family()), (7, "gtk4"));
        assert!(parse_records("a\tb\n").is_err());
        assert!(parse_records("c\tx\tfocus\tfocus\tequal\t\t\t\t\n").is_err());
        // The 8-field format of the old replay is refused: a replay without the witness column
        // must not be judged as if it had passed the model-fact check.
        assert!(parse_records("gtk4/c\t7\tfocus\tfocus\tequal\t\tText:a\tText:a\n").is_err());
    }

    #[test]
    fn rows_are_read_with_their_epoch_tag() {
        let src = "[[deviation]]\nid = \"a\"\nsurface = \"focus\"\nepoch = \"m22b1\"\nreason = \"r\"\nuntil = \"2026-11-07\"\n\n[[deviation]]\nid = \"b\"\nsurface = \"focus\"\nreason = \"r\"\nuntil = \"2026-11-07\"\n";
        let rows = parse_rows(src).expect("parses");
        assert_eq!(rows[0].epoch, "m22b1");
        assert_eq!(rows[1].epoch, "");
        assert_eq!(rows[0].until, Date(2026, 11, 7));
    }

    #[test]
    fn every_checked_in_row_is_usable_by_the_gate() {
        let src = std::fs::read_to_string(repo_root().join(DEVIATIONS)).expect("deviations.toml");
        let rows = parse_rows(&src).expect("parses");
        assert!(!rows.is_empty());
        for r in &rows {
            assert!(
                !r.epoch.is_empty(),
                "row {}@{} has no `epoch` tag (the gate would call it unclassified)",
                r.id,
                r.surface
            );
        }
    }

    #[test]
    fn the_checked_in_manifest_parses_and_names_the_replay_families() {
        let src = std::fs::read_to_string(repo_root().join(CORPORA)).expect("manifest");
        let specs = parse_manifest(&src).expect("parses");
        for want in [
            "gtk3",
            "gtk4",
            "firefox",
            "chromium",
            "electron",
            "telegram",
            "gnome-shell",
            "vte",
        ] {
            assert!(specs.iter().any(|s| s.name == want), "family {want}");
        }
    }

    #[test]
    fn coverage_needs_corpora_or_a_live_waiver() {
        let specs = vec![
            FamilySpec {
                name: "gtk4".into(),
                waived_until: None,
                waiver: String::new(),
                no_change: Vec::new(),
            },
            FamilySpec {
                name: "telegram".into(),
                waived_until: Some(Date(2026, 11, 7)),
                waiver: "no fixture".into(),
                no_change: Vec::new(),
            },
            FamilySpec {
                name: "electron".into(),
                waived_until: Some(Date(2026, 10, 1)),
                waiver: "stale".into(),
                no_change: Vec::new(),
            },
            FamilySpec {
                name: "gtk3".into(),
                waived_until: None,
                waiver: String::new(),
                no_change: Vec::new(),
            },
        ];
        let recs = vec![rec("equal", "", "focus")];
        let c = coverage(&specs, &recs, TODAY);
        assert_eq!(c[0].1, Coverage::Covered { corpora: 1 });
        assert!(matches!(c[1].1, Coverage::Waived(_)));
        assert_eq!(c[2].1, Coverage::Missing, "an expired waiver is no waiver");
        assert_eq!(c[3].1, Coverage::Missing);
    }

    #[test]
    fn a_waiver_without_a_reason_is_refused() {
        let src = "[[family]]\nname = \"x\"\nwaived_until = \"2026-11-07\"\n";
        assert!(parse_manifest(src).is_err());
    }

    #[test]
    fn determinism_means_byte_identity() {
        assert!(first_differences("a\nb\n", "a\nb\n", 5).is_empty());
        let d = first_differences("a\nb\nc\n", "a\nX\nc\nd\n", 5);
        assert_eq!(d.len(), 2);
        assert!(d[0].contains("line 2"));
        assert!(
            !first_differences("a\n", "a", 5).is_empty(),
            "a trailing newline is a difference"
        );
    }

    fn verdict_in(lang: &str, recs: &[Rec], nondet: Vec<String>) -> Verdict {
        let specs = vec![FamilySpec {
            name: "gtk4".into(),
            waived_until: None,
            waiver: String::new(),
            no_change: vec![("slider".into(), "the engine does not republish it".into())],
        }];
        judge("focus", lang, recs, &[good()], &specs, nondet, TODAY, 1)
    }

    fn verdict(recs: &[Rec], nondet: Vec<String>) -> Verdict {
        verdict_in("en", recs, nondet)
    }

    #[test]
    fn the_gate_passes_with_zero_unclassified_and_determinism() {
        let v = verdict(
            &[
                rec("equal", "", "focus"),
                rec("allowed", "role-from-model", "focus"),
            ],
            Vec::new(),
        );
        assert!(v.pass());
        assert_eq!((v.total.nodes, v.total.equal, v.total.intended), (2, 1, 1));
        assert!(
            table(&[v]).contains("| focus | en | 1/1 families | 2 | 1 | 1 | 0 | 0 | 0 | yes |"),
            "{}",
            table(&[verdict(&[rec("equal", "", "focus")], Vec::new())])
        );
    }

    #[test]
    fn one_unclassified_node_one_nondeterministic_byte_or_a_missing_family_fails() {
        assert!(!verdict(&[rec("different", "", "focus")], Vec::new()).pass());
        assert!(!verdict(&[rec("equal", "", "focus")], vec!["x".into()]).pass());
        assert!(
            !verdict(&[], Vec::new()).pass(),
            "an empty replay is no evidence"
        );
        let mut other = rec("equal", "", "focus");
        other.corpus = "firefox/forms".into();
        assert!(
            !verdict(&[other], Vec::new()).pass(),
            "the required family gtk4 is not covered"
        );
    }

    #[test]
    fn the_report_names_result_table_and_groups_unclassified_by_wording() {
        let mut a = rec("different", "", "focus");
        a.new = "Text:Other | Role:link".to_owned();
        let mut b = a.clone();
        b.node = 8;
        b.legacy = "Text:Elsewhere | Role:button".to_owned();
        b.new = "Text:Again | Role:link".to_owned();
        let v = verdict(&[a, b], Vec::new());
        let text = report(&[v], &[("rev".into(), "abc".into())]);
        assert!(text.contains("result: FAIL"));
        assert!(text.contains("rev: abc"));
        assert!(
            text.contains("2x focus / focus"),
            "content is masked so the groups merge:\n{text}"
        );
        let m = parse_report("x.md", &text).expect("front matter");
        assert_eq!(
            (m.surface.as_str(), m.pass, m.rev.as_str()),
            ("focus", false, "abc")
        );
    }

    #[test]
    fn a_row_that_rests_on_a_contradicted_model_fact_is_not_intended() {
        // The row exists, is dated, tagged and reasoned, and the output differs exactly as the row
        // says: without the witness it is INTENDED.
        let mut r = rec("allowed", "role-from-model", "focus");
        assert!(matches!(classify(&r, &[good()], TODAY), Class::Intended(_)));
        // With a witness (the source record contradicts the fused fact the predicate read) the same
        // record is a model defect, and the gate fails on it.
        r.witness = "select-not-applicable: the source record says selectable".to_owned();
        match classify(&r, &[good()], TODAY) {
            Class::Unclassified(why) => {
                assert!(why.contains("rests on a fused model fact"), "{why}");
                assert!(why.contains("source record says selectable"), "{why}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
        let v = verdict(&[rec("equal", "", "focus"), r], Vec::new());
        assert!(!v.pass(), "a row cannot justify its own regression");
        assert_eq!(v.total.unclassified, 1);
        assert_eq!(v.total.intended, 0);
    }

    #[test]
    fn every_dropped_node_is_counted_and_an_unexplained_reason_fails_the_gate() {
        let mut recs = vec![rec("equal", "", "focus")];
        recs.push(dropped("gtk4", "trigger-button", 3));
        recs.push(dropped("gtk4", "node-added", 2));
        let v = verdict(&recs, Vec::new());
        assert!(v.pass(), "explained drops do not fail: {:?}", v.drops);
        assert_eq!(v.dropped(), 5);
        assert_eq!(v.unexplained_drops(), 0);
        assert_eq!(
            v.total.nodes, 1,
            "drops are not compared nodes and are not counted as equal"
        );
        let text = report(&[v], &[]);
        assert!(text.contains("| gtk4 | trigger-button | 3 |"), "{text}");
        assert!(text.contains("total dropped: 5; unexplained: 0"), "{text}");
        // A reason nobody explained, and the two the replay raises for broken corpora.
        for reason in [
            "mystery",
            "empty-corpus",
            "page-unparsable",
            "no-fused-entry",
        ] {
            let mut recs = vec![rec("equal", "", "focus")];
            recs.push(dropped("gtk4", reason, 1));
            let v = verdict(&recs, Vec::new());
            assert!(!v.pass(), "unexplained drop `{reason}` must fail");
            assert_eq!(v.unexplained_drops(), 1);
            let text = report(&[v], &[]);
            assert!(text.contains("UNEXPLAINED"), "{text}");
        }
    }

    #[test]
    fn a_real_case_without_a_change_is_explained_only_by_the_manifest() {
        let mut listed = dropped("gtk4", "case-without-change", 1);
        listed.corpus = "gtk4/change:slider".to_owned();
        listed.case = "real-slider".to_owned();
        let mut unlisted = listed.clone();
        unlisted.corpus = "gtk4/change:spin".to_owned();
        unlisted.case = "real-spin".to_owned();
        let ok = verdict(&[rec("equal", "", "focus"), listed], Vec::new());
        assert!(ok.pass(), "{:?}", ok.drops);
        assert!(
            ok.drops[0]
                .explanation
                .as_deref()
                .unwrap()
                .contains("known")
        );
        let bad = verdict(&[rec("equal", "", "focus"), unlisted], Vec::new());
        assert!(!bad.pass());
        assert_eq!(bad.unexplained_drops(), 1);
    }

    #[test]
    fn dropped_records_do_not_count_as_corpus_coverage() {
        // A family whose every page was dropped has nothing compared: it is not covered.
        let specs = vec![FamilySpec {
            name: "vte".into(),
            waived_until: None,
            waiver: String::new(),
            no_change: Vec::new(),
        }];
        let c = coverage(&specs, &[dropped("vte", "empty-corpus", 1)], TODAY);
        assert_eq!(c[0].1, Coverage::Missing);
    }

    #[test]
    fn the_gate_passes_only_when_every_language_passes() {
        let ok = verdict_in("en", &[rec("equal", "", "focus")], Vec::new());
        let ru_bad = verdict_in("ru", &[rec("different", "", "focus")], Vec::new());
        assert!(ok.pass() && !ru_bad.pass());
        assert!(!all_pass(&[ok.clone(), ru_bad.clone()]));
        assert!(all_pass(&[ok.clone(), ok.clone()]));
        assert!(!all_pass(&[]), "no verdict is no evidence");
        let text = report(&[ok, ru_bad], &[("rev".into(), "abc".into())]);
        assert!(text.contains("result: FAIL"), "{text}");
        assert!(text.contains("languages: en, ru"), "{text}");
        assert!(text.contains("## Language: en (PASS)"), "{text}");
        assert!(text.contains("## Language: ru (FAIL)"), "{text}");
        let m = parse_report("x.md", &text).expect("front matter");
        assert!(!m.pass);
    }

    #[test]
    fn a_no_change_entry_needs_a_reason() {
        let ok = "[[family]]\nname = \"x\"\n[[family.no_change]]\ncase = \"slider\"\nreason = \"engine\"\n";
        let specs = parse_manifest(ok).expect("parses");
        assert_eq!(specs[0].no_change, vec![("slider".into(), "engine".into())]);
        let bad = "[[family]]\nname = \"x\"\n[[family.no_change]]\ncase = \"slider\"\n";
        assert!(parse_manifest(bad).is_err());
    }

    #[test]
    fn every_drop_reason_the_replay_raises_is_either_explained_here_or_meant_to_fail() {
        // The replay's reason codes (a surface's replay in the tree). The explained set is closed:
        // adding a reason there without explaining it here makes the gate fail, on purpose. The
        // replays of a surface that is cut over are deleted with its legacy presenter (the goldens
        // replace them); a surface that is cut over later brings its own.
        let Ok(src) =
            std::fs::read_to_string(repo_root().join("crates/gorca-core/src/browse/gate_tests.rs"))
        else {
            return;
        };
        assert!(src.contains("\"hidden-node\""));
        assert!(drop_explanation("hidden-node").is_some());
        for failing in ["page-unparsable", "empty-corpus"] {
            assert!(src.contains(&format!("\"{failing}\"")), "{failing}");
            assert!(
                drop_explanation(failing).is_none(),
                "{failing} must stay unexplained"
            );
        }
    }

    #[test]
    fn a_report_never_overwrites_an_older_one() {
        let dir = std::env::temp_dir().join(format!("gorca-cutover-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let p1 = report_path(&dir, "focus", TODAY);
        std::fs::write(&p1, "x").expect("write");
        let p2 = report_path(&dir, "focus", TODAY);
        assert_ne!(p1, p2);
        assert!(p2.to_string_lossy().ends_with("focus-2026-10-24-2.md"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn meta(surface: &str, pass: bool, rev: &str) -> ReportMeta {
        ReportMeta {
            file: format!("r/{surface}-{rev}.md"),
            surface: surface.to_owned(),
            pass,
            rev: rev.to_owned(),
        }
    }

    #[test]
    fn a_surface_needs_a_committed_passing_report_on_this_history() {
        let yes = |_: &str| true;
        let n = SURFACES.len();
        assert_eq!(judge_reports(&[], &yes, &yes).len(), n);
        let ok: Vec<ReportMeta> = SURFACES.iter().map(|s| meta(s.name, true, "a")).collect();
        assert!(judge_reports(&ok, &yes, &yes).is_empty());
        let mut failing = ok.clone();
        failing[4] = meta(SURFACES[4].name, false, "a");
        assert_eq!(judge_reports(&failing, &yes, &yes).len(), 1);
        let untracked = |_: &str| false;
        assert_eq!(judge_reports(&ok, &untracked, &yes).len(), n);
        let foreign = |_: &str| false;
        assert_eq!(judge_reports(&ok, &yes, &foreign).len(), n);
    }
}
