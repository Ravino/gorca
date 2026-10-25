// SPDX-License-Identifier: GPL-3.0-or-later
//! Rule 12: no paths back into the past (ADR-0022 section 8, the cut-over and no-legacy rule).
//!
//! The M22b cut-over is complete: presentation facts are read from the fused semantic model
//! (`SemEntry`) and spoken from `PresIr`, and nothing else. This rule lists the legacy APIs that
//! existed before it ([`BANS`]: the legacy presenters, the `aria` word tables, the mirror's
//! `NodeMeta` and its `AttrMap`, the reader-side `subtree_label` fallback, `Role::spoken`, the
//! flat extension speech) and fails on every use. There is no allowlist: a use is a violation,
//! whatever the file.
//!
//! The cut-over hook is part of the rule: every surface of the cut-over plan needs a passing,
//! tracked gate report made on an ancestor of `HEAD` (`cargo xtask cutover --check`).
//!
//! Scope: non-test sources under `crates/*/src` and `extensions/*/src`. Comments, text after the
//! first `#[cfg(test)]` block, files named `tests.rs` or `*_tests.rs`, and `tests/`, `benches/`
//! and `examples/` directories are not scanned. The bench crates and xtask are exempt.
//! Extensions and plugins (ADR-0026) are in scope of every ban that lists `extensions/` or the
//! extension host: they get no legacy data, ever.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
    /// Stable id of the ban, named in the violation.
    pub id: &'static str,
    /// The identifier or path fragment to find. Matched on identifier boundaries.
    pub needle: &'static str,
    /// A use is ignored when the character right after the match is one of these (a builder
    /// method `.meta(` is not the field `.meta`).
    pub not_followed_by: &'static str,
    /// A use is ignored when the text right before the match ends with this (`.sem.role` is the
    /// fused role, `.role` alone is the legacy field).
    pub not_preceded_by: &'static str,
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
        not_preceded_by: "",
        scope,
        replacement,
    }
}

/// Modules already moved off the legacy node record (M22c ratchet, ADR-0022 section 8.7): they
/// read the fused entry only. A module joins this list in the commit that ports it and never
/// leaves; the rule then fails on every legacy `Role`, `StateSet` or interface read in it.
const PORTED: &[&str] = &[
    "crates/gorca-core/src/review/",
    "crates/gorca-core/src/text/",
    "crates/gorca-core/src/commands/",
];
const LEGACY_ROLE: &str =
    "SemRole from the fused entry (`review::zone::entry_of`, `Ivm::entry_view`)";
const LEGACY_STATES: &str = "StructState (`NodeSnapshot::struct_state`) or the semantic states and flags of the fused entry";
const LEGACY_IFACES: &str =
    "SemFlags (FOCUSABLE, EDITABLE, ...) and the typed groups of the fused entry";

const FUSED: &str = "gorca_present::{compose, render} on the fused entry";
const PROVISIONAL: &str = "provisional S4 fact in the model (ADR-0022 section 8.3)";
const SEMENTRY: &str =
    "typed SemEntry groups; raw attributes stay inside the connector (SrcRecord)";

/// The banned legacy APIs. Nothing may use one; port the code to the fused model instead.
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
        not_preceded_by: "",
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
    Ban {
        id: "legacy-role-type",
        needle: "Role",
        not_followed_by: "",
        // `SegmentKind::Role` is a speech segment kind, not the legacy role.
        not_preceded_by: "Kind::",
        scope: PORTED,
        replacement: LEGACY_ROLE,
    },
    ban("legacy-stateset", "StateSet", PORTED, LEGACY_STATES),
    ban("legacy-ifaces", "Ifaces", PORTED, LEGACY_IFACES),
    ban("legacy-iface", "Iface", PORTED, LEGACY_IFACES),
    ban("snap-interfaces", ".interfaces", PORTED, LEGACY_IFACES),
    Ban {
        id: "snap-role",
        needle: ".role",
        not_followed_by: "",
        not_preceded_by: "sem",
        scope: PORTED,
        replacement: LEGACY_ROLE,
    },
    ban(
        "snapshot-states",
        ".states",
        PORTED,
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
        let preceded_bad =
            !ban.not_preceded_by.is_empty() && line[..start].ends_with(ban.not_preceded_by);
        if before_ok && !after_bad && !preceded_bad {
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
            // Skip exactly the item the attribute is on: a declaration ends at `;`, a block at
            // its closing brace. The text after it is production code again.
            let (mut depth, mut open) = (0i64, false);
            for (_, l) in lines.by_ref() {
                let c = code_of(l);
                if c.trim_start().starts_with("#[") && !open {
                    continue;
                }
                for ch in c.chars() {
                    match ch {
                        '{' => {
                            depth += 1;
                            open = true;
                        }
                        '}' => depth -= 1,
                        _ => {}
                    }
                }
                if (open && depth <= 0) || (!open && c.trim_end().ends_with(';')) {
                    break;
                }
            }
            continue;
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

/// The violations among `hits`, one line each: every legacy use is one.
pub fn judge(hits: &[Hit]) -> Vec<String> {
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
    used.iter()
        .map(|((ban, file), lines)| {
            format!(
                "legacy use `{ban}` in {file} (lines {lines:?}); use: {}",
                replacement(ban)
            )
        })
        .collect()
}

/// Run rule 12 over the repository this crate lives in. `true` when clean. `--print` lists the
/// current uses as `ban | file | count` (it changes nothing).
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
    let mut problems = judge(&hits);
    // Every surface is cut over: each needs a passing gate report.
    problems.extend(crate::cutover::check_reports(&root));
    if problems.is_empty() {
        println!("legacy rule: ok (no use of a legacy API; no allowlist)");
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

    const F: &str = "crates/gorca-core/src/a.rs";

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
    fn a_cfg_test_item_in_the_middle_hides_only_itself() {
        let src = "fn a() { present_focus(n); }\n\
                   #[cfg(test)]\n\
                   fn only_tests() {\n    present_focus(n);\n}\n\
                   fn b() {\n    present_focus(n);\n}\n\
                   #[cfg(test)]\n\
                   mod tests { fn t() { present_focus(n); } }\n";
        let lines: Vec<usize> = scan(F, src).iter().map(|h| h.line).collect();
        assert_eq!(lines, vec![1, 7]);
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
    fn a_ported_module_cannot_read_the_legacy_role_states_or_interfaces() {
        const P: &str = "crates/gorca-core/src/review/x.rs";
        let src = "use gorca_model::{Role, StateSet};\n\
                   fn a(s: &NodeSnapshot) -> bool { s.role == Role::Window }\n\
                   fn b(s: &NodeSnapshot) -> bool { s.states.contains(x) || s.interfaces.is_empty() }\n\
                   fn c(e: &SemEntry) -> bool { e.sem.role == SemRole::Window }\n\
                   fn d() -> SegmentKind { SegmentKind::Role }\n";
        let mut ids: Vec<(&str, usize)> = scan(P, src).iter().map(|h| (h.ban, h.line)).collect();
        ids.sort_unstable();
        assert_eq!(
            ids,
            vec![
                ("legacy-role-type", 1),
                ("legacy-role-type", 2),
                ("legacy-stateset", 1),
                ("snap-interfaces", 3),
                ("snap-role", 2),
                ("snapshot-states", 3),
            ]
        );
        // The same text elsewhere is not in the ratchet yet.
        assert!(scan("crates/gorca-core/src/browse/x.rs", src).is_empty());
    }

    #[test]
    fn ported_modules_exist() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        for m in PORTED {
            assert!(
                root.join(m).exists(),
                "{m} was ported and then moved: update PORTED"
            );
        }
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
    fn every_use_fails_and_there_is_no_allowlist() {
        let v = judge(&[hit("present-focus", F, 3), hit("present-focus", F, 9)]);
        assert_eq!(v.len(), 1, "{v:?}");
        assert!(v[0].contains("present-focus") && v[0].contains("[3, 9]"));
        assert!(judge(&[]).is_empty());
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        assert!(
            !root.join("xtask/legacy.allow").exists(),
            "the allowlist mechanism is gone: port the code, do not allow it"
        );
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

    /// The real workspace obeys the rule: no legacy use anywhere. This is the check `cargo xtask
    /// deps` and `cargo xtask legacy` run.
    #[test]
    fn the_workspace_obeys_the_rule() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let hits = scan_workspace(&root).unwrap();
        let v = judge(&hits);
        assert!(v.is_empty(), "{v:#?}");
    }
}
