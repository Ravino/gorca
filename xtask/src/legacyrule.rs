// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 12: no paths back into the past (ADR-0022 section 8, the cut-over and no-legacy rule).
//!
//! Until the M22b cut-over, presentation facts are still read outside the fused semantic model
//! (`SemEntry`) and `PresIr`: the legacy presenters, the `aria` word tables, the mirror's
//! `NodeMeta` and its `AttrMap`, the reader-side `subtree_label` fallback, `Role::spoken`. This
//! rule lists those APIs ([`BANS`]) and fails on every use that is not in `xtask/legacy.allow`.
//!
//! The allowlist is a ratchet. Each row names a ban, a file, the exact number of uses, an expiry
//! date and the M22b slice that deletes the use. The rule fails when
//!
//! * a use is not covered by a row, or a file has more uses than its row allows (the list grows);
//! * a file has fewer uses than its row says (the row must be lowered or removed in the same
//!   change that deleted the use, so the list only shrinks);
//! * a row has expired (its date is before today) or its date is later than [`LAST_EXPIRY`].
//!
//! Scope: non-test sources under `crates/*/src` and `extensions/*/src`. Comments, text after the
//! first `#[cfg(test)]` block, files named `tests.rs` or `*_tests.rs`, and `tests/`, `benches/`
//! and `examples/` directories are not scanned: test code of a legacy path is deleted with it.
//! The bench crates and xtask are exempt. Extensions and plugins (ADR-0026) are in scope of every
//! ban that lists `extensions/` or the extension host: they get no legacy data, ever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

/// The latest date any allowlist row may carry: the end of the last M22b slice (b4).
pub const LAST_EXPIRY: Date = Date(2026, 12, 12);

/// Repo-relative allowlist file.
const ALLOW_FILE: &str = "xtask/legacy.allow";

/// A calendar date, ordered year, month, day.
#[derive(Copy, Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Date(pub u32, pub u32, pub u32);

impl Date {
    pub fn parse(s: &str) -> Option<Self> {
        let mut it = s.trim().split('-');
        let (y, m, d) = (it.next()?, it.next()?, it.next()?);
        if it.next().is_some() || y.len() != 4 {
            return None;
        }
        let date = Self(y.parse().ok()?, m.parse().ok()?, d.parse().ok()?);
        ((1..=12).contains(&date.1) && (1..=31).contains(&date.2)).then_some(date)
    }

    /// Today in UTC, from the system clock.
    pub fn today() -> Result<Self> {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .context("system clock before 1970")?
            .as_secs();
        Ok(Self::from_days(secs / 86_400))
    }

    /// Civil date of a day count since 1970-01-01 (Howard Hinnant's algorithm).
    fn from_days(days: u64) -> Self {
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = yoe + era * 400 + u64::from(m <= 2);
        Self(
            u32::try_from(y).unwrap_or(u32::MAX),
            u32::try_from(m).unwrap_or(1),
            u32::try_from(d).unwrap_or(1),
        )
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.0, self.1, self.2)
    }
}

/// One banned API or path.
pub struct Ban {
    /// Stable id used by allowlist rows.
    pub id: &'static str,
    /// The identifier or path fragment to find. Matched on identifier boundaries.
    pub needle: &'static str,
    /// A use is ignored when the character right after the match is one of these (a builder
    /// method `.meta(` is not the field `.meta`).
    pub not_followed_by: &'static str,
    /// Repo-relative path prefixes the ban applies to.
    pub scope: &'static [&'static str],
    /// What replaces it.
    pub replacement: &'static str,
}

const CORE: &[&str] = &["crates/gorca-core/"];
const ALL: &[&str] = &["crates/", "extensions/"];
/// Everything that consumes presentation facts, extensions and the extension host included.
const CONSUMERS: &[&str] = &[
    "crates/gorca-core/",
    "crates/gorca-daemon/",
    "crates/gorca-ext-host/",
    "crates/gorca-ext-api/",
    "crates/gorca-present/",
    "crates/gorca-present-ir/",
    "crates/gorca-output-api/",
    "crates/gorca-braille-brlapi/",
    "extensions/",
];

const fn ban(
    id: &'static str,
    needle: &'static str,
    scope: &'static [&'static str],
    replacement: &'static str,
) -> Ban {
    Ban {
        id,
        needle,
        not_followed_by: "",
        scope,
        replacement,
    }
}

const FUSED: &str = "gorca_present::{compose, render} on the fused entry";
const PROVISIONAL: &str = "provisional S4 fact in the model (ADR-0022 section 8.3)";
const SEMENTRY: &str =
    "typed SemEntry groups; raw attributes stay inside the connector (SrcRecord)";

/// The banned legacy APIs. A new surface is never allowed to start using one: add a row to the
/// allowlist only in a review that names the slice that deletes it.
pub const BANS: &[Ban] = &[
    ban("subtree-label", "subtree_label", ALL, PROVISIONAL),
    ban("web-overlay", "web_overlay", ALL, FUSED),
    ban("overlay-from-meta", "overlay_from_meta", ALL, FUSED),
    ban("legacy-name", "legacy_name", ALL, PROVISIONAL),
    ban("legacy-struct", "Legacy", CORE, PROVISIONAL),
    ban("present-focus", "present_focus", ALL, FUSED),
    ban("present-web", "present_web", ALL, FUSED),
    ban("present-selected-item", "present_selected_item", ALL, FUSED),
    ban("present-with", "present_with", ALL, FUSED),
    ban(
        "present-change",
        "present_change",
        ALL,
        "gorca_present compose with Reason::Change",
    ),
    ban(
        "aria-words",
        "aria::",
        CORE,
        "gorca-present role/state/value tables over SemEntry",
    ),
    ban(
        "aria-module",
        "mod aria",
        CORE,
        "gorca-present role/state/value tables over SemEntry",
    ),
    ban(
        "state-words",
        "state_words",
        CORE,
        "gorca-present state fields over SemEntry",
    ),
    ban(
        "role-spoken",
        ".spoken()",
        CONSUMERS,
        "gorca_present::roles::role_word(SemRole) (catalog word)",
    ),
    ban(
        "legacy-family",
        "legacy_family",
        ALL,
        "delete: the shadow compares against SemRole directly after the switch",
    ),
    ban(
        "node-meta",
        "NodeMeta",
        ALL,
        "SemEntry through Ivm::entry_view (the only per-node fact carrier)",
    ),
    Ban {
        id: "meta-field",
        needle: ".meta",
        not_followed_by: "(",
        scope: CORE,
        replacement: "SemEntry through Ivm::entry_view",
    },
    ban("attr-map", "AttrMap", CONSUMERS, SEMENTRY),
    ban("attrs-field", ".attrs", CONSUMERS, SEMENTRY),
    ban(
        "web-attrs",
        "web_attrs::",
        CONSUMERS,
        "typed SemEntry groups (role token, level, current, popup, error, live)",
    ),
    ban(
        "policy-facts",
        "PolicyFacts",
        CONSUMERS,
        "extension facts built from SemEntry (ADR-0026 accessors), never from NodeSnapshot",
    ),
    ban(
        "agree-shadow",
        "mod agree",
        CORE,
        "delete with the legacy values it compares against (b4); the PresIr shadow is gorca-present",
    ),
    ban(
        "snapshot-states",
        ".states",
        &["crates/gorca-core/src/review/"],
        "NodeSnapshot::struct_state() (StructState: showing, visible, defunct, focusable); every spoken state is a group of the fused entry",
    ),
    ban(
        "ext-speech",
        "BackgroundMsg::ExtSpeech",
        CORE,
        "extension speech (host.speak, command speak) as PresIr content through render (ADR-0022 section 2.9)",
    ),
    ban(
        "ext-rewrite",
        "ExtVerdict::Rewrite",
        &["crates/gorca-core/", "crates/gorca-ext-host/"],
        "an on-event rewrite as a PresIr replacement with an origin tag (ADR-0022 section 2.9)",
    ),
    ban(
        "ext-snapshot",
        "NodeSnapshot",
        &[
            "crates/gorca-ext-host/",
            "crates/gorca-ext-api/",
            "extensions/",
        ],
        "extensions read facts only through the ADR-0026 accessors over SemEntry",
    ),
];

/// One use of a banned API.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Hit {
    pub ban: &'static str,
    pub file: String,
    pub line: usize,
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Whether `line` contains `ban.needle` on identifier boundaries.
fn line_matches(line: &str, ban: &Ban) -> bool {
    let needle = ban.needle;
    let first_ident = needle.chars().next().is_some_and(is_ident);
    let last_ident = needle.chars().next_back().is_some_and(is_ident);
    let mut from = 0;
    while let Some(pos) = line[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = !first_ident || !line[..start].chars().next_back().is_some_and(is_ident);
        let after = line[end..].chars().next();
        let after_bad =
            after.is_some_and(|c| (last_ident && is_ident(c)) || ban.not_followed_by.contains(c));
        if before_ok && !after_bad {
            return true;
        }
        from = end;
    }
    false
}

/// The code part of a line: no trailing `//` comment.
fn code_of(line: &str) -> &str {
    line.find("//").map_or(line, |i| &line[..i])
}

/// Uses of the banned APIs in the non-test part of `source`.
pub fn scan(file: &str, source: &str) -> Vec<Hit> {
    let bans: Vec<&Ban> = BANS
        .iter()
        .filter(|b| b.scope.iter().any(|p| file.starts_with(p)))
        .collect();
    let mut hits = Vec::new();
    let mut lines = source.lines().enumerate().peekable();
    while let Some((i, line)) = lines.next() {
        let t = line.trim_start();
        if t.starts_with("#[cfg(test)]") {
            // `#[cfg(test)] mod x;` hides one declaration; a block ends the production text.
            match lines.peek() {
                Some((_, next)) if next.trim_end().ends_with(';') => {
                    lines.next();
                    continue;
                }
                _ => break,
            }
        }
        if t.starts_with("//") {
            continue;
        }
        let code = code_of(line);
        for b in &bans {
            if line_matches(code, b) {
                hits.push(Hit {
                    ban: b.id,
                    file: file.to_owned(),
                    line: i + 1,
                });
            }
        }
    }
    hits
}

fn skipped_file(rel: &str) -> bool {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    name == "tests.rs"
        || name.ends_with("_tests.rs")
        || ["/tests/", "/benches/", "/examples/"]
            .iter()
            .any(|d| rel.contains(d))
        || rel.starts_with("crates/gorca-bench")
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    for e in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let p = e?.path();
        if p.is_dir() {
            rust_files(&p, out)?;
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
    Ok(())
}

/// Scan the workspace under `root`.
pub fn scan_workspace(root: &Path) -> Result<Vec<Hit>> {
    let mut hits = Vec::new();
    for group in ["crates", "extensions"] {
        let dir = root.join(group);
        if !dir.exists() {
            continue;
        }
        for krate in std::fs::read_dir(&dir)? {
            let src = krate?.path().join("src");
            if !src.is_dir() {
                continue;
            }
            let mut files = Vec::new();
            rust_files(&src, &mut files)?;
            for f in files {
                let rel = f
                    .strip_prefix(root)
                    .unwrap_or(&f)
                    .to_string_lossy()
                    .into_owned();
                if skipped_file(&rel) {
                    continue;
                }
                let text = std::fs::read_to_string(&f)
                    .with_context(|| format!("reading {}", f.display()))?;
                hits.extend(scan(&rel, &text));
            }
        }
    }
    hits.sort();
    Ok(hits)
}

/// One allowlist row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Allow {
    pub ban: String,
    pub file: String,
    pub max: usize,
    pub expires: Date,
    pub slice: String,
}

/// Parse `ban | file | max | expires | slice` rows; `#` starts a comment.
pub fn parse_allow(text: &str) -> Result<Vec<Allow>> {
    let mut rows = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let n = i + 1;
        let f: Vec<&str> = line.split('|').map(str::trim).collect();
        let [ban, file, max, expires, slice] = f[..] else {
            bail!("{ALLOW_FILE}:{n}: want `ban | file | max | expires | slice`");
        };
        if !BANS.iter().any(|b| b.id == ban) {
            bail!("{ALLOW_FILE}:{n}: unknown ban `{ban}`");
        }
        let Some(expires) = Date::parse(expires) else {
            bail!("{ALLOW_FILE}:{n}: bad expiry date `{expires}` (want YYYY-MM-DD)");
        };
        let Ok(max) = max.parse::<usize>() else {
            bail!("{ALLOW_FILE}:{n}: bad count `{max}`");
        };
        if slice.is_empty() {
            bail!("{ALLOW_FILE}:{n}: every row names the slice that deletes the use");
        }
        rows.push(Allow {
            ban: ban.to_owned(),
            file: file.to_owned(),
            max,
            expires,
            slice: slice.to_owned(),
        });
    }
    Ok(rows)
}

/// Check `hits` against `allow` as of `today`. Returns the violations, one line each.
pub fn judge(hits: &[Hit], allow: &[Allow], today: Date) -> Vec<String> {
    let mut out = Vec::new();
    let replacement = |id: &str| {
        BANS.iter()
            .find(|b| b.id == id)
            .map_or("", |b| b.replacement)
    };
    let mut used: BTreeMap<(&str, &str), Vec<usize>> = BTreeMap::new();
    for h in hits {
        used.entry((h.ban, h.file.as_str()))
            .or_default()
            .push(h.line);
    }
    let mut seen = std::collections::BTreeSet::new();
    for a in allow {
        if !seen.insert((a.ban.as_str(), a.file.as_str())) {
            out.push(format!("allowlist: duplicate row `{} | {}`", a.ban, a.file));
        }
        if a.expires < today {
            out.push(format!(
                "allowlist: `{} | {}` expired on {}; delete the legacy path ({}) now",
                a.ban, a.file, a.expires, a.slice
            ));
        }
        if a.expires > LAST_EXPIRY {
            out.push(format!(
                "allowlist: `{} | {}` expires {} which is after {LAST_EXPIRY}",
                a.ban, a.file, a.expires
            ));
        }
        let n = used
            .get(&(a.ban.as_str(), a.file.as_str()))
            .map_or(0, Vec::len);
        if n < a.max {
            out.push(format!(
                "allowlist: `{} | {}` allows {} but the file has {n}; lower the row (it may only shrink)",
                a.ban, a.file, a.max
            ));
        }
    }
    for ((ban, file), lines) in &used {
        let max = allow
            .iter()
            .find(|a| a.ban == *ban && a.file == *file)
            .map_or(0, |a| a.max);
        if lines.len() > max {
            out.push(format!(
                "legacy use `{ban}` in {file} (lines {lines:?}): {max} allowed, {} found; use: {}",
                lines.len(),
                replacement(ban)
            ));
        }
    }
    out
}

/// Run rule 12 over the repository this crate lives in. `true` when clean. `--print` lists the
/// current uses as `ban | file | count` (to build or check the file; it changes nothing).
pub fn run(print: bool) -> Result<bool> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let hits = scan_workspace(&root)?;
    if print {
        let mut counts: BTreeMap<(&str, &str), usize> = BTreeMap::new();
        for h in &hits {
            *counts.entry((h.ban, h.file.as_str())).or_default() += 1;
        }
        for ((ban, file), n) in counts {
            println!("{ban} | {file} | {n}");
        }
        return Ok(true);
    }
    let text = std::fs::read_to_string(root.join(ALLOW_FILE))
        .with_context(|| format!("reading {ALLOW_FILE}"))?;
    let allow = parse_allow(&text)?;
    let mut problems = judge(&hits, &allow, Date::today()?);
    // A surface whose slice has no row left is cut over: it needs a passing gate report.
    let live: std::collections::BTreeSet<String> = allow.iter().map(|a| a.slice.clone()).collect();
    problems.extend(crate::cutover::check_reports(&root, &live));
    if problems.is_empty() {
        println!(
            "legacy rule: ok ({} allowlisted rows, {} uses; they only shrink)",
            allow.len(),
            hits.len()
        );
        return Ok(true);
    }
    for p in &problems {
        eprintln!("rule 12: {p}");
    }
    eprintln!("{} legacy rule violation(s)", problems.len());
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TODAY: Date = Date(2026, 10, 24);
    const F: &str = "crates/gorca-core/src/a.rs";

    fn row(ban: &str, file: &str, max: usize, expires: Date) -> Allow {
        Allow {
            ban: ban.to_owned(),
            file: file.to_owned(),
            max,
            expires,
            slice: "M22b1-5".to_owned(),
        }
    }

    fn hit(ban: &'static str, file: &str, line: usize) -> Hit {
        Hit {
            ban,
            file: file.to_owned(),
            line,
        }
    }

    #[test]
    fn needles_match_on_identifier_boundaries_and_skip_comments_and_tests() {
        let src = "fn a() { subtree_label(d); }\n\
                   // subtree_label is gone\n\
                   fn b() { my_subtree_label(d); subtree_labels(d); }\n\
                   fn c() { x(); } // subtree_label\n\
                   #[cfg(test)]\n\
                   mod tests { fn t() { subtree_label(d); } }\n";
        assert_eq!(scan(F, src), vec![hit("subtree-label", F, 1)]);
    }

    #[test]
    fn a_cfg_test_declaration_hides_only_itself() {
        let src = "#[cfg(test)]\nmod tests;\nfn a() { present_focus(n); }\n";
        assert_eq!(scan(F, src), vec![hit("present-focus", F, 3)]);
    }

    #[test]
    fn the_meta_field_is_not_the_builder_method() {
        let src = "let m = v.meta;\nn.meta(Arc::new(x));\nlet d = v.meta.map_or(1, f);\n";
        let lines: Vec<usize> = scan(F, src)
            .iter()
            .filter(|h| h.ban == "meta-field")
            .map(|h| h.line)
            .collect();
        assert_eq!(lines, vec![1, 3]);
    }

    #[test]
    fn bans_apply_only_inside_their_scope() {
        let src = "use crate::aria::Node;\nlet a = n.attrs;\n";
        assert!(scan("crates/gorca-model/src/x.rs", src).is_empty());
        assert_eq!(scan(F, src).len(), 2);
    }

    #[test]
    fn extensions_get_no_snapshot_and_no_legacy_data() {
        let src = "use gorca_model::NodeSnapshot;\nfn f(a: &AttrMap) {}\n";
        let ids: Vec<&str> = scan("extensions/ext-x/src/lib.rs", src)
            .iter()
            .map(|h| h.ban)
            .collect();
        assert_eq!(ids, vec!["ext-snapshot", "attr-map"]);
        assert!(!scan("crates/gorca-ext-host/src/convert.rs", src).is_empty());
    }

    #[test]
    fn a_new_use_fails() {
        let v = judge(&[hit("present-focus", F, 3)], &[], TODAY);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("present-focus"));
    }

    #[test]
    fn the_allowlist_may_not_grow_for_a_file() {
        let allow = [row("present-focus", F, 1, Date(2026, 11, 7))];
        let hits = [hit("present-focus", F, 3), hit("present-focus", F, 9)];
        let v = judge(&hits, &allow, TODAY);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("1 allowed, 2 found"));
    }

    #[test]
    fn a_deleted_use_forces_the_row_down() {
        let allow = [row("present-focus", F, 2, Date(2026, 11, 7))];
        let v = judge(&[hit("present-focus", F, 3)], &allow, TODAY);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("may only shrink"));
        assert_eq!(judge(&[], &allow, TODAY).len(), 1);
    }

    #[test]
    fn an_expired_row_fails_even_when_the_use_is_still_there() {
        let allow = [row("present-focus", F, 1, Date(2026, 10, 23))];
        let v = judge(&[hit("present-focus", F, 3)], &allow, TODAY);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("expired on 2026-10-23"));
        // The expiry day itself is still valid.
        let ok = [row("present-focus", F, 1, TODAY)];
        assert!(judge(&[hit("present-focus", F, 3)], &ok, TODAY).is_empty());
    }

    #[test]
    fn no_row_may_outlive_the_last_slice() {
        let allow = [row("present-focus", F, 1, Date(2026, 12, 13))];
        let v = judge(&[hit("present-focus", F, 3)], &allow, TODAY);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("after 2026-12-12"));
    }

    #[test]
    fn rows_parse_and_reject_garbage() {
        let ok = "# c\npresent-focus | crates/a.rs | 2 | 2026-11-07 | M22b1-5\n";
        assert_eq!(
            parse_allow(ok).unwrap(),
            vec![row("present-focus", "crates/a.rs", 2, Date(2026, 11, 7))]
        );
        assert!(parse_allow("nope | a.rs | 1 | 2026-11-07 | s").is_err());
        assert!(parse_allow("present-focus | a.rs | 1 | 15.12.2026 | s").is_err());
        assert!(parse_allow("present-focus | a.rs | x | 2026-11-07 | s").is_err());
        assert!(parse_allow("present-focus | a.rs | 1 | 2026-11-07 | ").is_err());
        assert!(parse_allow("present-focus | a.rs | 1 | 2026-11-07").is_err());
    }

    #[test]
    fn civil_dates_come_out_right() {
        assert_eq!(Date::from_days(0), Date(1970, 1, 1));
        assert_eq!(Date::from_days(19_723), Date(2024, 1, 1));
        assert_eq!(Date::from_days(20_750), Date(2026, 10, 24));
    }

    #[test]
    fn ban_ids_are_unique_and_scoped() {
        let mut ids: Vec<&str> = BANS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        let n = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), n);
        assert!(
            BANS.iter()
                .all(|b| !b.scope.is_empty() && !b.replacement.is_empty())
        );
    }

    /// The real workspace obeys the rule today: every legacy use is allowlisted with a live
    /// expiry. This is the check `cargo xtask deps` and `cargo xtask legacy` run.
    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let hits = scan_workspace(&root).unwrap();
        let allow = parse_allow(&std::fs::read_to_string(root.join(ALLOW_FILE)).unwrap()).unwrap();
        // The calendar is not under test here: judge as of the day the rows were written.
        let v = judge(&hits, &allow, TODAY);
        assert!(v.is_empty(), "{v:#?}");
    }
}
