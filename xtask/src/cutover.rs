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
//! 2. runs it three times inside a private session (`tests/harness/run-isolated.sh`): twice
//!    plainly and once with a seeded shuffle of the arrival order and batching, and requires the
//!    three record files to be byte-identical (nondeterminism fails the gate);
//! 3. classifies each record: EQUAL, INTENDED (a deviation row for exactly this surface, not
//!    expired, with an `epoch` tag and a reason) or UNCLASSIFIED; it passes with 0 UNCLASSIFIED
//!    and every required corpus family covered (or waived with a dated reason);
//! 4. prints the table and writes `bench/results/cutover/<surface>-<date>.md`, never overwriting
//!    an older report (a failing report is evidence and stays).
//!
//! `cargo xtask cutover --check` (also run by `cargo xtask legacy`) is the CI hook: once no
//! `xtask/legacy.allow` row names the slice that switches a surface, a passing, tracked report
//! made on an ancestor of `HEAD` must exist for that surface.

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
    /// The `legacy.allow` slice column that deletes this surface's legacy path.
    pub slice: &'static str,
    /// The `gorca-present` path and the replay exist.
    pub ready: bool,
}

pub const SURFACES: &[Surface] = &[
    Surface {
        name: "focus",
        slice: "M22b1-5",
        ready: true,
    },
    Surface {
        name: "change",
        slice: "M22b1-5",
        ready: true,
    },
    Surface {
        name: "browse",
        slice: "M22b2",
        ready: false,
    },
    Surface {
        name: "quicknav",
        slice: "M22b2",
        ready: false,
    },
    Surface {
        name: "review",
        slice: "M22b3",
        ready: false,
    },
    Surface {
        name: "window",
        slice: "M22b4",
        ready: false,
    },
    Surface {
        name: "notification",
        slice: "M22b4",
        ready: false,
    },
    Surface {
        name: "explain",
        slice: "M22b4",
        ready: false,
    },
];

const REPORT_DIR: &str = "bench/results/cutover";
const DEVIATIONS: &str = "crates/gorca-present/deviations.toml";
const CORPORA: &str = "tests/fixtures/sem/cutover-corpora.toml";
const FIXTURES: &str = "tests/fixtures/sem";
/// The seed of the shuffled-arrival run (override with `--seed`).
const DEFAULT_SEED: u64 = 0x5eed;
const REPLAY_TEST: &str = "cutover_gate_tests::cutover_emit";

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
        if f.len() != 8 {
            bail!("record {}: want 8 fields, got {}", n + 1, f.len());
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
        self.nodes += 1;
        match c {
            Class::Equal => self.equal += 1,
            Class::Silent => self.silent += 1,
            Class::Intended(_) => self.intended += 1,
            Class::Unclassified(_) => self.unclassified += 1,
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
            out.push(FamilySpec {
                name: s("name").to_owned(),
                waived_until,
                waiver: s("waiver").to_owned(),
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
    for r in recs {
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

/// Everything the report needs.
pub struct Verdict {
    pub surface: String,
    pub date: Date,
    pub total: Tally,
    pub per_family: BTreeMap<String, Tally>,
    pub coverage: Vec<(String, Coverage)>,
    pub deterministic: bool,
    pub nondeterminism: Vec<String>,
    pub intended: BTreeMap<String, u64>,
    /// (case, surface, masked legacy, masked new, reason) -> (count, first example).
    pub unclassified: BTreeMap<String, (u64, String)>,
    pub not_ready: bool,
    pub run_hash: u64,
}

impl Verdict {
    pub fn pass(&self) -> bool {
        !self.not_ready
            && self.total.unclassified == 0
            && self.total.nodes > 0
            && self.deterministic
            && !self
                .coverage
                .iter()
                .any(|(_, c)| matches!(c, Coverage::Missing))
    }
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
pub fn judge(
    surface: &str,
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
    for r in recs {
        let c = classify(r, rows, today);
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
            Class::Equal | Class::Silent => {}
        }
    }
    Verdict {
        surface: surface.to_owned(),
        date: today,
        total,
        per_family,
        coverage: coverage(specs, recs, today),
        deterministic: nondeterminism.is_empty(),
        nondeterminism,
        intended,
        unclassified,
        not_ready: false,
        run_hash,
    }
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

/// The short table printed on the terminal and placed at the top of the report.
pub fn table(v: &Verdict) -> String {
    let (c, n) = covered_count(v);
    let mut s = String::new();
    let _ = writeln!(
        s,
        "| surface | corpora | nodes | equal | intended | unclassified | deterministic |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|");
    let _ = writeln!(
        s,
        "| {} | {c}/{n} families | {} | {} | {} | {} | {} |",
        v.surface,
        v.total.nodes,
        v.total.equal + v.total.silent,
        v.total.intended,
        v.total.unclassified,
        yes_no(v.deterministic)
    );
    s
}

/// Everything an `inputs` line hashes: the deviations, the manifest and every fixture file.
pub fn inputs_hash(root: &Path) -> Result<u64> {
    let mut files = vec![root.join(DEVIATIONS), root.join(CORPORA)];
    let mut stack = vec![root.join(FIXTURES)];
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

/// The Markdown report. `meta` carries the conditions (rev, load, governor, seed).
pub fn report(v: &Verdict, meta: &[(String, String)]) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Cut-over gate: {}", v.surface);
    let _ = writeln!(s);
    let _ = writeln!(s, "result: {}", if v.pass() { "PASS" } else { "FAIL" });
    let _ = writeln!(s, "surface: {}", v.surface);
    let _ = writeln!(s, "date: {}", v.date);
    for (k, val) in meta {
        let _ = writeln!(s, "{k}: {val}");
    }
    let _ = writeln!(s, "records: {:016x}", v.run_hash);
    let _ = writeln!(s);
    if v.not_ready {
        let _ = writeln!(
            s,
            "This surface has no `gorca-present` path yet, so there is nothing to replay. The gate \
             fails by design until its slice (docs/reports/m22b1-plan.md) lands the path and the replay."
        );
        let _ = writeln!(s);
    }
    s.push_str(&table(v));
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "equal includes {} nodes where both presenters say nothing.",
        v.total.silent
    );
    let _ = writeln!(s);
    let _ = writeln!(s, "## Corpora");
    let _ = writeln!(s);
    let _ = writeln!(
        s,
        "| family | status | nodes | equal | intended | unclassified |"
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
    let _ = writeln!(s, "## Determinism");
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
    let _ = writeln!(s, "## Intended differences (deviations.toml, epoch change)");
    let _ = writeln!(s);
    if v.intended.is_empty() {
        let _ = writeln!(s, "None.");
    }
    for (id, n) in &v.intended {
        let _ = writeln!(s, "- `{id}`: {n}");
    }
    let _ = writeln!(s);
    let _ = writeln!(s, "## Unclassified differences");
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

fn build_test_binary(root: &Path) -> Result<PathBuf> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .current_dir(root)
        .args([
            "test",
            "-p",
            "gorca-connector-atspi",
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
            && a.target.name == "gorca_connector_atspi"
            && a.profile.test
            && let Some(exe) = a.executable
        {
            found = Some(PathBuf::from(exe.as_str()));
        }
    }
    found.context("cargo did not report the test executable of gorca-connector-atspi")
}

/// One replay inside a private session; returns the record file's text.
fn replay_once(
    root: &Path,
    bin: &Path,
    surface: &str,
    seed: u64,
    scratch: &Path,
    tag: &str,
) -> Result<String> {
    let out = scratch.join(format!("{surface}-{tag}.tsv"));
    let status = Command::new(root.join("tests/harness/run-isolated.sh"))
        .current_dir(root)
        .arg("--")
        .arg(bin)
        .args(["--exact", REPLAY_TEST, "--nocapture"])
        .env(
            "GORCA_PASSTHROUGH",
            "GORCA_CUTOVER_SURFACE GORCA_CUTOVER_OUT GORCA_CUTOVER_SHUFFLE",
        )
        .env("GORCA_CUTOVER_SURFACE", surface)
        .env("GORCA_CUTOVER_OUT", &out)
        .env("GORCA_CUTOVER_SHUFFLE", seed.to_string())
        .status()
        .context("starting tests/harness/run-isolated.sh")?;
    if !status.success() {
        bail!("replay `{tag}` failed ({status}) inside the private session");
    }
    std::fs::read_to_string(&out).with_context(|| format!("replay `{tag}` wrote no records"))
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
    v: &Verdict,
    meta: &[(String, String)],
    unclassified_tsv: &str,
) -> Result<PathBuf> {
    let dir = root.join(REPORT_DIR);
    std::fs::create_dir_all(&dir)?;
    let path = report_path(&dir, &v.surface, v.date);
    std::fs::write(&path, report(v, meta))?;
    if !unclassified_tsv.is_empty() {
        std::fs::write(path.with_extension("unclassified.tsv"), unclassified_tsv)?;
    }
    Ok(path)
}

/// `cargo xtask cutover <surface> [--seed N]` or `cargo xtask cutover --check`.
pub fn run(mut args: impl Iterator<Item = String>) -> Result<bool> {
    let first = args.next().context(
        "usage: cargo xtask cutover <surface> [--seed N] | --check   (surfaces: focus, change, browse, quicknav, review, window, notification, explain)",
    )?;
    if first == "--check" {
        return check();
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
            date: today,
            total: Tally::default(),
            per_family: BTreeMap::new(),
            coverage: Vec::new(),
            deterministic: true,
            nondeterminism: Vec::new(),
            intended: BTreeMap::new(),
            unclassified: BTreeMap::new(),
            not_ready: true,
            run_hash: 0,
        };
        let p = write_report(&root, &v, &meta, "")?;
        print!("{}", table(&v));
        eprintln!(
            "cutover {}: FAIL, no gorca-present path yet ({}); report {}",
            surface.name,
            surface.slice,
            p.display()
        );
        return Ok(false);
    }

    let rows = parse_rows(&std::fs::read_to_string(root.join(DEVIATIONS))?)?;
    let specs = parse_manifest(&std::fs::read_to_string(root.join(CORPORA))?)?;
    let bin = build_test_binary(&root)?;
    let scratch = std::env::temp_dir().join(format!("gorca-cutover-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let a = replay_once(&root, &bin, surface.name, 0, &scratch, "a")?;
    let b = replay_once(&root, &bin, surface.name, 0, &scratch, "b")?;
    let c = replay_once(&root, &bin, surface.name, seed, &scratch, "shuffled")?;
    let _ = std::fs::remove_dir_all(&scratch);

    let mut nondeterminism = Vec::new();
    for l in first_differences(&a, &b, 10) {
        nondeterminism.push(format!("two plain runs: {l}"));
    }
    for l in first_differences(&a, &c, 10) {
        nondeterminism.push(format!("shuffled arrival (seed {seed:#x}): {l}"));
    }
    let recs = parse_records(&a)?;
    let v = judge(
        surface.name,
        &recs,
        &rows,
        &specs,
        nondeterminism,
        today,
        fnv64(a.as_bytes()),
    );
    let mut bad = String::new();
    for r in &recs {
        if matches!(classify(r, &rows, today), Class::Unclassified(_)) {
            let _ = writeln!(
                bad,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                r.corpus, r.node, r.case, r.surface, r.outcome, r.legacy, r.new
            );
        }
    }
    let p = write_report(&root, &v, &meta, &bad)?;
    print!("{}", table(&v));
    println!(
        "cutover {}: {} (report {})",
        surface.name,
        if v.pass() { "PASS" } else { "FAIL" },
        p.display()
    );
    Ok(v.pass())
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

/// The slices that still have a `legacy.allow` row; a surface whose slice has none is cut over.
/// `tracked` says whether git tracks a report and `ancestor` whether a rev is an ancestor of HEAD.
pub fn judge_reports(
    live_slices: &BTreeSet<String>,
    reports: &[ReportMeta],
    tracked: &dyn Fn(&str) -> bool,
    ancestor: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    for s in SURFACES {
        if live_slices.contains(s.slice) {
            continue;
        }
        let ok = reports
            .iter()
            .any(|r| r.surface == s.name && r.pass && tracked(&r.file) && ancestor(&r.rev));
        if !ok {
            out.push(format!(
                "cut-over: no legacy.allow row names {} any more, so surface `{}` is cut over, but \
                 {REPORT_DIR}/ has no committed PASS report made on an ancestor of HEAD; run \
                 `cargo xtask cutover {}` before the cut-over commit and commit the report",
                s.slice, s.name, s.name
            ));
        }
    }
    out
}

/// Rule 12's extension: violations as one line each (empty when the hook is satisfied).
pub fn check_reports(root: &Path, live_slices: &BTreeSet<String>) -> Vec<String> {
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
    judge_reports(live_slices, &reports, &tracked, &ancestor)
}

fn check() -> Result<bool> {
    let root = repo_root();
    let text = std::fs::read_to_string(root.join("xtask/legacy.allow")).unwrap_or_default();
    let live: BTreeSet<String> = crate::legacyrule::parse_allow(&text)?
        .into_iter()
        .map(|a| a.slice)
        .collect();
    let problems = check_reports(&root, &live);
    for p in &problems {
        eprintln!("rule 12: {p}");
    }
    if problems.is_empty() {
        println!("cutover check: ok (no cut-over surface lacks a passing gate report)");
    }
    Ok(problems.is_empty())
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
        let t = "gtk4/controls\t7\tfocus\tfocus\tequal\t\tText:a\tText:a\n";
        let r = parse_records(t).expect("parses");
        assert_eq!((r[0].node, r[0].family()), (7, "gtk4"));
        assert!(parse_records("a\tb\n").is_err());
        assert!(parse_records("c\tx\tfocus\tfocus\tequal\t\t\t\n").is_err());
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
            "gtk3", "gtk4", "firefox", "chromium", "electron", "telegram",
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
            },
            FamilySpec {
                name: "telegram".into(),
                waived_until: Some(Date(2026, 11, 7)),
                waiver: "no fixture".into(),
            },
            FamilySpec {
                name: "electron".into(),
                waived_until: Some(Date(2026, 10, 1)),
                waiver: "stale".into(),
            },
            FamilySpec {
                name: "gtk3".into(),
                waived_until: None,
                waiver: String::new(),
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

    fn verdict(recs: &[Rec], nondet: Vec<String>) -> Verdict {
        let specs = vec![FamilySpec {
            name: "gtk4".into(),
            waived_until: None,
            waiver: String::new(),
        }];
        judge("focus", recs, &[good()], &specs, nondet, TODAY, 1)
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
        assert!(table(&v).contains("| focus | 1/1 families | 2 | 1 | 1 | 0 | yes |"));
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
        let text = report(&v, &[("rev".into(), "abc".into())]);
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
    fn a_cut_over_surface_needs_a_committed_passing_report_on_this_history() {
        let live: BTreeSet<String> = ["M22b2", "M22b3", "M22b4"].map(String::from).into();
        let yes = |_: &str| true;
        // M22b1-5 has no rows left: focus and change are cut over.
        assert_eq!(judge_reports(&live, &[], &yes, &yes).len(), 2);
        let ok = [meta("focus", true, "a"), meta("change", true, "a")];
        assert!(judge_reports(&live, &ok, &yes, &yes).is_empty());
        let failing = [meta("focus", false, "a"), meta("change", true, "a")];
        assert_eq!(judge_reports(&live, &failing, &yes, &yes).len(), 1);
        let untracked = |_: &str| false;
        assert_eq!(judge_reports(&live, &ok, &untracked, &yes).len(), 2);
        let foreign = |_: &str| false;
        assert_eq!(judge_reports(&live, &ok, &yes, &foreign).len(), 2);
    }

    #[test]
    fn surfaces_whose_slice_still_has_rows_need_no_report() {
        let live: BTreeSet<String> = ["M22b1-5", "M22b2", "M22b3", "M22b4"]
            .map(String::from)
            .into();
        let no = |_: &str| false;
        assert!(judge_reports(&live, &[], &no, &no).is_empty());
    }

    #[test]
    fn every_surface_has_a_slice_that_legacy_allow_knows() {
        let text = std::fs::read_to_string(repo_root().join("xtask/legacy.allow")).expect("allow");
        for s in SURFACES {
            assert!(text.contains(s.slice), "{} names slice {}", s.name, s.slice);
        }
    }
}
