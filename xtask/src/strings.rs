// SPDX-License-Identifier: GPL-3.0-or-later
//! PR-7: no literal user-facing text in the speech paths (ADR-0010).
//!
//! Every spoken, brailled or displayed string goes through the message catalog by id
//! (`gorca-i18n`). This lint finds string literals in the non-test sources of the crates that
//! build speech and fails when one appears that is not in `xtask/strings.allow`.
//!
//! What counts as a candidate (the lint is a tripwire, not a parser):
//! * *prose*: a literal with a space between letters, a leading capital, or a non-ASCII letter
//!   (`"application not responding"`, `"Insert"`, `"ввод"`);
//! * *a word in a speech call*: any literal with letters on a line that calls `say(`,
//!   `announce(`, `say_app(`, `message(`, `seg(`, `Item::State(`, `Item::Role(`, `Item::Text(`,
//!   `Item::Lead(`, `Item::Info(`, `Item::Name(` or builds a `Segment` (`"enter"` in `say("enter")`).
//!
//! Skipped: items gated by `#[cfg(test)]` (a module, function, impl or `mod x;`; the rest of the
//! file is still scanned), `tests/` directories, comment
//! lines, `tracing` macros, `expect(`, `debug_struct`/`.field(` and `#[error(` lines (logs and
//! developer diagnostics are not speech).
//!
//! The allow-list holds what is deliberately not a message: log and debug text, protocol
//! names, attribute values, match patterns, internal ids. One entry per line,
//! `repo/path.rs | literal as written in the source`; `repo/path.rs | *` allows a whole file
//! (test-only modules, structured logs). Entries that no longer match are reported, not fatal.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Repo-relative roots scanned.
const ROOTS: &[&str] = &[
    "crates/gorca-core/src",
    "crates/gorca-speech/src",
    "crates/gorca-text/src",
    "crates/gorca-ext-host/src",
    "crates/gorca-daemon/src",
    "crates/gorca-input-api/src/keys.rs",
    "crates/gorca-model/src/role.rs",
    "extensions/ext-gnome-shell/src",
    "extensions/ext-vte/src",
];

/// Calls whose literal arguments are spoken.
const SPEECH_CALLS: &[&str] = &[
    "say(",
    "announce(",
    "say_app(",
    "message(",
    "seg(",
    "Item::State(",
    "Item::Role(",
    "Item::Text(",
    "Item::Lead(",
    "Item::Info(",
    "Item::Name(",
    "Segment {",
    "notice(",
    "Piece::phrase(",
    "Piece::unit(",
    "TermAction::Say",
];

/// Lines that are logs or diagnostics, never speech.
const NOT_SPEECH_LINES: &[&str] = &[
    "trace!(",
    "debug!(",
    "info!(",
    "warn!(",
    "error!(",
    "expect(",
    "debug_struct(",
    ".field(",
    "#[error(",
    "write_str(",
    "tracing::",
    "target:",
    "unreachable!",
];

const ALLOW_FILE: &str = "xtask/strings.allow";

#[derive(Debug, Eq, PartialEq, Ord, PartialOrd, Clone)]
struct Hit {
    path: String,
    line: usize,
    literal: String,
}

/// The string literals of one source line (escapes kept as written), ignoring `//` comments.
fn literals(line: &str) -> Vec<String> {
    let b: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => break,
            '\'' => {
                // A char literal ('"', '\'') or a lifetime: skip the quote and what it holds.
                if b.get(i + 1) == Some(&'\\') {
                    i += 4;
                } else if b.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' && i + 1 < b.len() {
                        s.push(b[i]);
                        i += 1;
                    }
                    s.push(b[i]);
                    i += 1;
                }
                out.push(s);
                i += 1;
            }
            _ => i += 1,
        }
    }
    out
}

fn has_letters(s: &str) -> bool {
    s.chars().filter(|c| c.is_alphabetic()).count() >= 2
}

/// A space between letters, a leading capital, or a non-ASCII letter.
fn is_prose(s: &str) -> bool {
    let t = s.trim();
    if !has_letters(t) {
        return false;
    }
    let spaced = t
        .split(' ')
        .filter(|w| w.chars().any(char::is_alphabetic))
        .count()
        >= 2;
    let capital = t.chars().next().is_some_and(char::is_uppercase);
    let non_ascii = t.chars().any(|c| c.is_alphabetic() && !c.is_ascii());
    // `\u{..}` escapes stand for non-ASCII letters.
    spaced || capital || non_ascii || t.contains("\\u{")
}

/// The source of a line without string and char literals and without a trailing `//` comment
/// (what is left can be searched for braces).
fn code_only(line: &str) -> String {
    let b: Vec<char> = line.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '/' if b.get(i + 1) == Some(&'/') => break,
            '\'' => {
                if b.get(i + 1) == Some(&'\\') {
                    i += 4;
                } else if b.get(i + 2) == Some(&'\'') {
                    i += 3;
                } else {
                    i += 1;
                }
            }
            '"' => {
                i += 1;
                while i < b.len() && b[i] != '"' {
                    if b[i] == '\\' {
                        i += 1;
                    }
                    i += 1;
                }
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

/// Tracks the item that follows a `#[cfg(test)]` attribute so that only it is skipped.
#[derive(Default)]
struct TestSkip {
    /// A `#[cfg(test)]` was seen and its item has not ended yet (no body brace seen).
    pending: bool,
    /// Brace depth inside the skipped item (0 = not inside one).
    depth: usize,
}

impl TestSkip {
    /// Whether the (trimmed) line belongs to a test-gated item.
    fn skips(&mut self, l: &str) -> bool {
        if self.depth > 0 {
            self.track(l);
            return true;
        }
        if l.starts_with("#[cfg(test)]") {
            self.pending = true;
            return true;
        }
        if self.pending {
            if l.starts_with("#[") || l.starts_with("//") || l.is_empty() {
                return true;
            }
            self.track(l);
            return true;
        }
        false
    }

    /// Count the braces of a line of the skipped item; an item without a body ends at `;`.
    fn track(&mut self, l: &str) {
        let code = code_only(l);
        let opens = code.matches('{').count();
        let closes = code.matches('}').count();
        if self.depth == 0 && opens == 0 {
            // `mod tests;`, `use x;`, or the head of a multi-line signature.
            self.pending = !code.trim_end().ends_with(';');
            return;
        }
        self.pending = false;
        self.depth = (self.depth + opens).saturating_sub(closes);
    }
}

fn scan_file(root: &Path, path: &Path, out: &mut Vec<Hit>) -> Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned();
    let mut skip = TestSkip::default();
    for (i, raw) in text.lines().enumerate() {
        let l = raw.trim();
        if skip.skips(l) {
            continue;
        }
        if l.starts_with("//") || l.starts_with("#[") && !l.starts_with("#[error(") {
            continue;
        }
        if NOT_SPEECH_LINES.iter().any(|p| l.contains(p)) {
            continue;
        }
        let in_call = SPEECH_CALLS.iter().any(|p| l.contains(p));
        for lit in literals(l) {
            if is_prose(&lit) || (in_call && has_letters(&lit)) {
                out.push(Hit {
                    path: rel.clone(),
                    line: i + 1,
                    literal: lit,
                });
            }
        }
    }
    Ok(())
}

fn collect(root: &Path, rel: &str, out: &mut Vec<Hit>) -> Result<()> {
    let p = root.join(rel);
    if p.is_file() {
        return scan_file(root, &p, out);
    }
    let mut stack: Vec<PathBuf> = vec![p];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        let mut entries: Vec<PathBuf> = rd.filter_map(Result::ok).map(|e| e.path()).collect();
        entries.sort();
        for e in entries {
            if e.is_dir() {
                if e.file_name()
                    .is_some_and(|n| n == "tests" || n == "benches")
                {
                    continue;
                }
                stack.push(e);
            } else if e.extension().is_some_and(|x| x == "rs") {
                let name = e.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "tests.rs" || name.ends_with("_tests.rs") {
                    continue;
                }
                scan_file(root, &e, out)?;
            }
        }
    }
    Ok(())
}

/// The allow-list: `(path, literal)` pairs and whole-file entries.
type Allow = (BTreeSet<(String, String)>, BTreeSet<String>);

fn load_allow(root: &Path) -> Result<Allow> {
    let text = std::fs::read_to_string(root.join(ALLOW_FILE))
        .with_context(|| format!("read {ALLOW_FILE}"))?;
    let mut pairs = BTreeSet::new();
    let mut files = BTreeSet::new();
    for l in text.lines() {
        let l = l.trim();
        if l.is_empty() || l.starts_with('#') {
            continue;
        }
        let Some((p, lit)) = l.split_once(" | ") else {
            continue;
        };
        if lit == "*" {
            files.insert(p.to_owned());
        } else {
            pairs.insert((p.to_owned(), lit.to_owned()));
        }
    }
    Ok((pairs, files))
}

fn workspace_root() -> Result<PathBuf> {
    let meta = cargo_metadata::MetadataCommand::new()
        .no_deps()
        .exec()
        .context("cargo metadata")?;
    Ok(PathBuf::from(&meta.workspace_root))
}

/// Run the lint; `update` rewrites the allow-list from the current sources (a reviewer reads the
/// diff: every new line is a string someone decided is not a message).
pub fn run(update: bool) -> Result<bool> {
    let root = workspace_root()?;
    let mut hits = Vec::new();
    for r in ROOTS {
        collect(&root, r, &mut hits)?;
    }
    hits.sort();
    if update {
        let (_, files) = load_allow(&root)?;
        let mut body = std::fs::read_to_string(root.join(ALLOW_FILE))?
            .lines()
            .take_while(|l| l.starts_with('#') || l.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        body.push('\n');
        for f in &files {
            let _ = writeln!(body, "{f} | *");
        }
        let mut seen = BTreeSet::new();
        for h in &hits {
            if files.contains(&h.path) {
                continue;
            }
            if seen.insert((h.path.clone(), h.literal.clone())) {
                let _ = writeln!(body, "{} | {}", h.path, h.literal);
            }
        }
        std::fs::write(root.join(ALLOW_FILE), body)?;
        println!("xtask strings: wrote {ALLOW_FILE} ({} entries)", seen.len());
        return Ok(true);
    }
    let (pairs, files) = load_allow(&root)?;
    let mut used: BTreeSet<(String, String)> = BTreeSet::new();
    let mut bad = Vec::new();
    for h in &hits {
        if files.contains(&h.path) {
            continue;
        }
        let key = (h.path.clone(), h.literal.clone());
        if pairs.contains(&key) {
            used.insert(key);
        } else {
            bad.push(h);
        }
    }
    for (p, l) in pairs.difference(&used) {
        if !files.contains(p) {
            eprintln!("xtask strings: stale allow-list entry: {p} | {l}");
        }
    }
    if bad.is_empty() {
        println!(
            "xtask strings: ok ({} literals, {} allowed)",
            hits.len(),
            used.len()
        );
        return Ok(true);
    }
    eprintln!("xtask strings: PR-7 violation, literal user-facing text outside the catalog:");
    for h in &bad {
        eprintln!("  {}:{}: \"{}\"", h.path, h.line, h.literal);
    }
    eprintln!(
        "move the text to crates/gorca-i18n/catalog/{{en,ru}}/*.msg and use msg!(Id); if it is not \
         speech (a log line, a protocol name, an internal id), add `path | literal` to {ALLOW_FILE}"
    );
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_are_found_outside_comments_and_chars() {
        assert_eq!(literals(r#"x.say("a b", '"'); // "c""#), ["a b"]);
        assert_eq!(literals(r#"f("a\"b")"#), [r#"a\"b"#]);
    }

    #[test]
    fn prose_has_spaces_capitals_or_non_ascii_letters() {
        assert!(is_prose("application not responding"));
        assert!(is_prose("Insert"));
        assert!(is_prose("ввод"));
        assert!(!is_prose("speech.rate_up"));
        assert!(!is_prose("document_web"));
        assert!(!is_prose(", "));
    }

    fn scan_text(text: &str) -> Vec<usize> {
        let mut skip = TestSkip::default();
        let mut hits = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let l = raw.trim();
            if skip.skips(l) {
                continue;
            }
            if literals(l).iter().any(|x| is_prose(x)) {
                hits.push(i + 1);
            }
        }
        hits
    }

    #[test]
    fn only_test_gated_items_are_skipped() {
        let src = "fn a() { f(\"first one\"); }\n\
#[cfg(test)]\n\
fn helper() {\n    let _ = \"test only\";\n    if x { y }\n}\n\
fn b() { f(\"second one\"); }\n\
#[cfg(test)]\nmod tests;\n\
fn c(\n    a: u8,\n) { f(\"third one\"); }\n\
#[cfg(test)]\nmod tests {\n    fn t() { \"in tests\"; }\n}\n\
fn d() { f(\"fourth one\"); }\n";
        assert_eq!(scan_text(src), [1, 7, 12, 17]);
    }

    #[test]
    fn braces_in_literals_do_not_confuse_the_item_tracker() {
        let src = "#[cfg(test)]\nfn h() {\n    let _ = \"}\";\n    let _ = '}';\n}\nfn z() { f(\"after it\"); }\n";
        assert_eq!(scan_text(src), [6]);
    }
}
