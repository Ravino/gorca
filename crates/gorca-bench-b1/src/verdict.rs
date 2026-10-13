//! H-4: presentation and truth logs, and the verdict classifier of SPEC 6.2.
//!
//! Inputs are two JSON-lines files, both `CLOCK_MONOTONIC` nanoseconds:
//!
//! * truth log (written by the fake apps, SPEC 6.1). Two line shapes:
//!   `{"t_ns", "app", "path", "field", "old", "new", "evented"}` for a mutation and
//!   `{"t_ns", "focus": true, "path"}` for a focus change. Optionally
//!   `{"t_ns", "app", "path", "field", "init": <value>}` states the value before any mutation
//!   (otherwise the `old` of the first mutation of that field is used; a field with no history
//!   is *unconstrained*: any presented value matches);
//! * presentation log (PW-10): `{"t_ns", "utt_id", "epoch", "app_bus", "path", "source", "role",
//!   "states", "name", "value", "may_refine", "refines"}`; `states` is an array of strings (a
//!   single string is accepted), `source` is one of `hit|partial|stale|miss_reply|refine|followup`.
//!
//! Paths are the join key; the walk driver must give the fake apps disjoint path spaces (or an
//! `app` that equals `app_bus`, then use [`KeyMode::AppAndPath`]).

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::limits::{AUDIBLE_AFTER_NS, CLOCK_SKEW_NS, LATE_WINDOW_NS};

/// Fields compared by the verdict (SPEC 6.2: name, role, presented states, value).
pub const COMPARED: [&str; 4] = ["name", "role", "states", "value"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyMode {
    PathOnly,
    AppAndPath,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Hit,
    Partial,
    Stale,
    MissReply,
    Refine,
    Followup,
    /// The daemon's spoken change of a state or value of the node being presented (S1). It is
    /// a first utterance for the verdicts, but never answers a focus stimulus.
    S1,
}

impl Source {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "hit" => Self::Hit,
            "partial" => Self::Partial,
            "stale" => Self::Stale,
            "miss_reply" => Self::MissReply,
            "refine" => Self::Refine,
            "followup" => Self::Followup,
            "s1" => Self::S1,
            _ => return None,
        })
    }

    pub fn is_first(self) -> bool {
        !matches!(self, Self::Refine | Self::Followup)
    }
}

#[derive(Clone, Debug)]
pub struct Change {
    pub t_ns: u64,
    pub app: String,
    pub path: String,
    pub field: String,
    pub old: Value,
    pub new: Value,
    pub evented: bool,
    /// A secret field: the presentation log carries a keyed tag instead of its text.
    pub sensitive: bool,
}

#[derive(Clone, Debug)]
pub struct FocusLine {
    pub t_ns: u64,
    pub app: String,
    pub path: String,
}

#[derive(Clone, Debug, Default)]
pub struct Truth {
    pub changes: Vec<Change>,
    pub focus: Vec<FocusLine>,
    /// (key, field) -> value before any mutation.
    pub inits: BTreeMap<(String, String), Value>,
    /// Keys of `inits` that are secret.
    secret_inits: BTreeSet<(String, String)>,
}

impl Truth {
    /// Replace the text of secret fields by the tags the daemon logs for them (same key, same
    /// function: `gorca_model::RedactKey::tag`), so both logs still compare.
    pub fn redact(&mut self, key: &gorca_model::RedactKey) {
        let tag = |v: &Value| match v {
            Value::String(s) => Value::String(key.tag(s)),
            Value::Number(n) => Value::String(key.tag(&n.as_f64().unwrap_or(f64::NAN).to_string())),
            other => other.clone(),
        };
        for c in self.changes.iter_mut().filter(|c| c.sensitive) {
            c.old = tag(&c.old);
            c.new = tag(&c.new);
        }
        for (k, v) in &mut self.inits {
            if self.secret_inits.contains(k) {
                *v = tag(v);
            }
        }
    }
}

/// The key in the header line of a presentation log (`{"redaction_key":"<32 hex digits>"}`).
pub fn redaction_key(presentations: &str) -> Option<gorca_model::RedactKey> {
    let v: Value = serde_json::from_str(presentations.lines().next()?).ok()?;
    gorca_model::RedactKey::from_hex(v.get("redaction_key")?.as_str()?)
}

#[derive(Clone, Debug)]
pub struct Presentation {
    pub t_ns: u64,
    pub utt_id: u64,
    pub epoch: u64,
    pub app_bus: String,
    pub path: String,
    pub source: Source,
    pub fields: BTreeMap<&'static str, Value>,
    pub may_refine: bool,
    pub refines: Option<u64>,
}

fn s(v: &Value, k: &str) -> String {
    v.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn u(v: &Value, k: &str) -> Result<u64> {
    v.get(k)
        .and_then(Value::as_u64)
        .with_context(|| format!("missing or non-integer `{k}`"))
}

/// Parse a JSON-lines document; blank lines are skipped, a bad line is an error with its number.
fn lines(text: &str) -> Result<Vec<(usize, Value)>> {
    let mut out = Vec::new();
    for (i, l) in text.lines().enumerate() {
        if l.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(l).with_context(|| format!("line {}: not JSON", i + 1))?;
        out.push((i + 1, v));
    }
    Ok(out)
}

pub fn parse_truth(text: &str) -> Result<Truth> {
    let mut t = Truth::default();
    for (n, v) in lines(text)? {
        let ctx = || format!("truth line {n}");
        if v.get("focus").is_some() {
            t.focus.push(FocusLine {
                t_ns: u(&v, "t_ns").with_context(ctx)?,
                app: s(&v, "app"),
                path: s(&v, "path"),
            });
        } else if let Some(init) = v.get("init") {
            let key = (s(&v, "path"), s(&v, "field"));
            if v.get("sensitive").and_then(Value::as_bool) == Some(true) {
                t.secret_inits.insert(key.clone());
            }
            t.inits.insert(key, norm(&s(&v, "field"), init));
        } else {
            t.changes.push(Change {
                t_ns: u(&v, "t_ns").with_context(ctx)?,
                app: s(&v, "app"),
                path: s(&v, "path"),
                field: s(&v, "field"),
                old: v.get("old").cloned().unwrap_or(Value::Null),
                new: v.get("new").cloned().unwrap_or(Value::Null),
                evented: v.get("evented").and_then(Value::as_bool).unwrap_or(false),
                sensitive: v.get("sensitive").and_then(Value::as_bool) == Some(true),
            });
        }
    }
    t.changes.sort_by_key(|c| c.t_ns);
    t.focus.sort_by_key(|f| f.t_ns);
    Ok(t)
}

pub fn parse_presentations(text: &str) -> Result<Vec<Presentation>> {
    let mut out = Vec::new();
    for (n, v) in lines(text)? {
        let ctx = || format!("presentation line {n}");
        // The header line carries the per-run redaction key, not a presentation.
        if v.get("redaction_key").is_some() && v.get("source").is_none() {
            continue;
        }
        let src = s(&v, "source");
        let Some(source) = Source::parse(&src) else {
            bail!("presentation line {n}: unknown source `{src}`");
        };
        let mut fields = BTreeMap::new();
        for f in COMPARED {
            fields.insert(f, norm(f, v.get(f).unwrap_or(&Value::Null)));
        }
        out.push(Presentation {
            t_ns: u(&v, "t_ns").with_context(ctx)?,
            utt_id: u(&v, "utt_id").with_context(ctx)?,
            epoch: v.get("epoch").and_then(Value::as_u64).unwrap_or(0),
            app_bus: s(&v, "app_bus"),
            path: s(&v, "path"),
            source,
            fields,
            may_refine: v
                .get("may_refine")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            refines: v.get("refines").and_then(Value::as_u64),
        });
    }
    out.sort_by_key(|p| (p.t_ns, p.utt_id));
    Ok(out)
}

/// States that the harness never compares (the focus ring is not a mutation).
const IGNORED_STATES: [&str; 2] = ["focused", "focusable"];

/// Canonical form: states become a sorted array without ignored members; integers and floats
/// compare equal; everything else is untouched.
pub fn norm(field: &str, v: &Value) -> Value {
    match (field, v) {
        ("states", Value::Array(a)) => {
            let set: BTreeSet<&str> = a
                .iter()
                .filter_map(Value::as_str)
                .filter(|x| !IGNORED_STATES.contains(x))
                .collect();
            Value::Array(
                set.into_iter()
                    .map(|x| Value::String(x.to_owned()))
                    .collect(),
            )
        }
        ("states", Value::String(x)) => norm(field, &Value::Array(vec![Value::String(x.clone())])),
        ("states", Value::Null) => Value::Array(Vec::new()),
        (_, Value::Number(n)) => n
            .as_f64()
            .and_then(serde_json::Number::from_f64)
            .map_or_else(|| v.clone(), Value::Number),
        _ => v.clone(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Correct,
    Refined,
    Late,
    Stale,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The stimulus was a focus change.
    Focus,
    /// The stimulus was an evented mutation after the last focus change (S1, state change).
    StateChange,
    /// No stimulus on this path in the truth log (where-am-I, navigation): strict instant check.
    Other,
}

#[derive(Clone, Debug)]
pub struct Judged {
    pub utt_id: u64,
    pub path: String,
    pub source: Source,
    pub kind: Kind,
    pub verdict: Verdict,
    /// Compared fields that did not match any valid version.
    pub mismatched: Vec<&'static str>,
    /// Whether the mutation that made the first utterance wrong was evented (corrections and
    /// stale only).
    pub evented_cause: Option<bool>,
    pub stimulus_ns: u64,
    pub latency_ns: u64,
    /// Visit of a node whose truth did not change since its previous presentation.
    pub clean_revisit: bool,
    /// For `Refined` / `Late`: how long after the first utterance the correcting one came.
    pub fix_delay_ns: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub struct Report {
    pub judged: Vec<Judged>,
    pub focus_lines: usize,
    pub lost: usize,
    pub superseded: usize,
    /// Refines whose epoch differs from the refined utterance (a core bug, reported).
    pub refine_epoch_mismatch: usize,
    /// Presentations whose `refines` points nowhere.
    pub orphan_followups: usize,
}

impl Report {
    pub fn count(&self, v: Verdict) -> usize {
        self.judged.iter().filter(|j| j.verdict == v).count()
    }

    pub fn focus_presentations(&self) -> usize {
        self.judged.iter().filter(|j| j.kind == Kind::Focus).count()
    }

    pub fn corrections(&self) -> usize {
        self.count(Verdict::Refined) + self.count(Verdict::Late)
    }

    /// M-6 rate over focus presentations; split `(evented, silent)` counts as well.
    pub fn m6(&self) -> (f64, usize, usize) {
        let focus: Vec<&Judged> = self
            .judged
            .iter()
            .filter(|j| j.kind == Kind::Focus)
            .collect();
        let corr: Vec<&&Judged> = focus
            .iter()
            .filter(|j| matches!(j.verdict, Verdict::Refined | Verdict::Late))
            .collect();
        let ev = corr
            .iter()
            .filter(|j| j.evented_cause == Some(true))
            .count();
        let rate = if focus.is_empty() {
            0.0
        } else {
            corr.len() as f64 / focus.len() as f64
        };
        (rate, ev, corr.len() - ev)
    }

    /// Corrections the user can hear (SPEC 9, B1-M6.audible): `Late` ones, and `Refined` ones
    /// whose replacement came after the speech server could have started the wrong audio
    /// (`AUDIBLE_AFTER_NS`). Returns `(rate over focus presentations, count)`.
    pub fn m6_audible(&self) -> (f64, usize) {
        let focus = self.focus_presentations();
        let n = self
            .judged
            .iter()
            .filter(|j| j.kind == Kind::Focus)
            .filter(|j| match j.verdict {
                Verdict::Late => true,
                Verdict::Refined => j.fix_delay_ns.is_none_or(|d| d > AUDIBLE_AFTER_NS),
                Verdict::Correct | Verdict::Stale => false,
            })
            .count();
        (ratio(n, focus), n)
    }

    /// Delays (ms) from a refined first utterance to its replacement, focus presentations only.
    pub fn refine_delays_ms(&self) -> Vec<f64> {
        self.judged
            .iter()
            .filter(|j| j.kind == Kind::Focus && j.verdict == Verdict::Refined)
            .filter_map(|j| j.fix_delay_ns)
            .map(|d| d as f64 / 1e6)
            .collect()
    }

    /// `hit / focus presentations`.
    pub fn hit_all(&self) -> f64 {
        let f: Vec<&Judged> = self
            .judged
            .iter()
            .filter(|j| j.kind == Kind::Focus)
            .collect();
        ratio(
            f.iter().filter(|j| j.source == Source::Hit).count(),
            f.len(),
        )
    }

    /// Hit rate over focus presentations of nodes not mutated since their last presentation.
    pub fn hit_revisit(&self) -> (f64, usize) {
        let f: Vec<&Judged> = self
            .judged
            .iter()
            .filter(|j| j.kind == Kind::Focus && j.clean_revisit)
            .collect();
        (
            ratio(
                f.iter().filter(|j| j.source == Source::Hit).count(),
                f.len(),
            ),
            f.len(),
        )
    }

    /// Latencies (ms) of first utterances matching `pick`.
    pub fn latencies_ms(&self, pick: impl Fn(&Judged) -> bool) -> Vec<f64> {
        self.judged
            .iter()
            .filter(|j| pick(j))
            .map(|j| j.latency_ns as f64 / 1e6)
            .collect()
    }
}

fn ratio(n: usize, d: usize) -> f64 {
    if d == 0 { 0.0 } else { n as f64 / d as f64 }
}

struct Index<'a> {
    mode: KeyMode,
    by_key: BTreeMap<String, Vec<&'a Change>>,
    focus_by_key: BTreeMap<String, Vec<&'a FocusLine>>,
    truth: &'a Truth,
}

fn key(mode: KeyMode, app: &str, path: &str) -> String {
    match mode {
        KeyMode::PathOnly => path.to_owned(),
        KeyMode::AppAndPath => format!("{app}|{path}"),
    }
}

impl<'a> Index<'a> {
    fn new(truth: &'a Truth, mode: KeyMode) -> Self {
        let mut by_key: BTreeMap<String, Vec<&Change>> = BTreeMap::new();
        for c in &truth.changes {
            by_key
                .entry(key(mode, &c.app, &c.path))
                .or_default()
                .push(c);
        }
        let mut focus_by_key: BTreeMap<String, Vec<&FocusLine>> = BTreeMap::new();
        for f in &truth.focus {
            focus_by_key
                .entry(key(mode, &f.app, &f.path))
                .or_default()
                .push(f);
        }
        Self {
            mode,
            by_key,
            focus_by_key,
            truth,
        }
    }

    fn key_of(&self, p: &Presentation) -> String {
        key(self.mode, &p.app_bus, &p.path)
    }

    /// Value of `field` at instant `t`; `None` = unknown (unconstrained).
    fn value_at(&self, k: &str, path: &str, field: &str, t: u64) -> Option<Value> {
        let hist: Vec<&&Change> = self
            .by_key
            .get(k)
            .into_iter()
            .flatten()
            .filter(|c| c.field == field)
            .collect();
        if let Some(c) = hist.iter().rev().find(|c| c.t_ns <= t) {
            return Some(norm(field, &c.new));
        }
        if let Some(init) = self.truth.inits.get(&(path.to_owned(), field.to_owned())) {
            return Some(init.clone());
        }
        hist.first().map(|c| norm(field, &c.old))
    }

    /// Versions of `field` valid at some instant in `[lo, hi]`. `None` entries match anything.
    fn versions(&self, k: &str, path: &str, field: &str, lo: u64, hi: u64) -> Vec<Option<Value>> {
        let mut v = vec![self.value_at(k, path, field, lo)];
        for c in self.by_key.get(k).into_iter().flatten() {
            if c.field == field && c.t_ns > lo && c.t_ns <= hi {
                v.push(Some(norm(field, &c.new)));
            }
        }
        v
    }

    /// Compared fields of `p` that match no version valid in `[lo, hi]`.
    fn mismatches(&self, p: &Presentation, lo: u64, hi: u64) -> Vec<&'static str> {
        let k = self.key_of(p);
        let mut bad = Vec::new();
        for f in COMPARED {
            let have = p.fields.get(f).cloned().unwrap_or(Value::Null);
            let vs = self.versions(&k, &p.path, f, lo, hi);
            let ok = vs.iter().any(|x| x.as_ref().is_none_or(|x| *x == have));
            if !ok {
                bad.push(f);
            }
        }
        bad
    }

    /// Latest change touching one of `fields` at or before `t`.
    fn last_change(&self, k: &str, fields: &[&str], t: u64) -> Option<&'a Change> {
        self.by_key
            .get(k)?
            .iter()
            .rev()
            .find(|c| c.t_ns <= t && fields.contains(&c.field.as_str()))
            .copied()
    }
}

/// Classify every first utterance of the presentation log against the truth log.
pub fn classify(truth: &Truth, pres: &[Presentation], mode: KeyMode) -> Report {
    let ix = Index::new(truth, mode);
    let by_id: BTreeMap<u64, &Presentation> = pres.iter().map(|p| (p.utt_id, p)).collect();
    let mut report = Report {
        focus_lines: truth.focus.len(),
        ..Report::default()
    };
    // Last presentation time per key, for the clean-revisit test.
    let mut last_shown: BTreeMap<String, u64> = BTreeMap::new();

    for p in pres {
        if !p.source.is_first() {
            if p.refines.is_none_or(|r| !by_id.contains_key(&r)) {
                report.orphan_followups += 1;
            } else if let Some(orig) = p.refines.and_then(|r| by_id.get(&r))
                && p.source == Source::Refine
                && orig.epoch != p.epoch
            {
                report.refine_epoch_mismatch += 1;
            }
            continue;
        }
        let k = ix.key_of(p);
        let t_focus = ix
            .focus_by_key
            .get(&k)
            .and_then(|v| v.iter().rev().find(|f| f.t_ns <= p.t_ns))
            .map(|f| f.t_ns);
        // Silent mutations are not stimuli: only the latest *evented* change counts.
        let t_evented = ix
            .by_key
            .get(&k)
            .and_then(|v| v.iter().rev().find(|c| c.t_ns <= p.t_ns && c.evented))
            .map(|c| c.t_ns);
        let (kind, stimulus) = match (t_focus, t_evented) {
            (Some(f), Some(e)) if e > f => (Kind::StateChange, e),
            (Some(f), _) => (Kind::Focus, f),
            (None, Some(e)) => (Kind::StateChange, e),
            (None, None) => (Kind::Other, p.t_ns),
        };
        let lo = stimulus.saturating_sub(CLOCK_SKEW_NS);
        let bad = ix.mismatches(p, lo, p.t_ns);
        let cause = |bad: &[&'static str]| ix.last_change(&k, bad, p.t_ns).map(|c| c.evented);

        let mut verdict = Verdict::Correct;
        let mut fix_delay_ns = None;
        let mut evented_cause = None;
        if !bad.is_empty() {
            evented_cause = cause(&bad);
            verdict = Verdict::Stale;
            // A later utterance that replaces or follows it with the then-current truth.
            for q in pres.iter().filter(|q| q.refines == Some(p.utt_id)) {
                let right = ix
                    .mismatches(q, q.t_ns.saturating_sub(CLOCK_SKEW_NS), q.t_ns)
                    .is_empty();
                match q.source {
                    Source::Refine if right && q.epoch == p.epoch => {
                        verdict = Verdict::Refined;
                        fix_delay_ns = Some(q.t_ns.saturating_sub(p.t_ns));
                        break;
                    }
                    Source::Followup
                        if right && q.t_ns.saturating_sub(p.t_ns) <= LATE_WINDOW_NS =>
                    {
                        verdict = Verdict::Late;
                        fix_delay_ns = Some(q.t_ns.saturating_sub(p.t_ns));
                    }
                    _ => {}
                }
            }
        }

        let clean_revisit = last_shown.get(&k).is_some_and(|&prev| {
            !ix.by_key
                .get(&k)
                .into_iter()
                .flatten()
                .any(|c| c.t_ns > prev && c.t_ns <= p.t_ns)
        });
        last_shown.insert(k, p.t_ns);

        report.judged.push(Judged {
            utt_id: p.utt_id,
            path: p.path.clone(),
            source: p.source,
            kind,
            verdict,
            mismatched: bad,
            evented_cause,
            stimulus_ns: stimulus,
            latency_ns: p.t_ns.saturating_sub(stimulus),
            clean_revisit,
            fix_delay_ns,
        });
    }

    // Lost focus stimuli: no first utterance for the path within 2 s, and not superseded.
    let horizon = 2_000_000_000u64;
    for (i, f) in truth.focus.iter().enumerate() {
        let k = key(mode, &f.app, &f.path);
        let spoken = pres.iter().any(|p| {
            p.source.is_first()
                && p.source != Source::S1
                && ix.key_of(p) == k
                && p.t_ns + CLOCK_SKEW_NS >= f.t_ns
                && p.t_ns <= f.t_ns + horizon
        });
        if spoken {
            continue;
        }
        let superseded = truth.focus[i + 1..]
            .iter()
            .any(|g| g.t_ns > f.t_ns && g.t_ns <= f.t_ns + horizon);
        if superseded {
            report.superseded += 1;
        } else {
            report.lost += 1;
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: u64 = 1_000_000;

    fn truth(lines: &[String]) -> Truth {
        parse_truth(&lines.join("\n")).unwrap()
    }

    fn change(t: u64, path: &str, field: &str, old: &str, new: &str, evented: bool) -> String {
        format!(
            r#"{{"t_ns":{t},"app":"a","path":"{path}","field":"{field}","old":"{old}","new":"{new}","evented":{evented}}}"#
        )
    }

    fn focus(t: u64, path: &str) -> String {
        format!(r#"{{"t_ns":{t},"focus":true,"path":"{path}"}}"#)
    }

    fn pres(t: u64, id: u64, path: &str, source: &str, name: &str, extra: &str) -> String {
        format!(
            r#"{{"t_ns":{t},"utt_id":{id},"epoch":1,"app_bus":":1.2","path":"{path}","source":"{source}","role":"button","states":["enabled"],"name":"{name}","value":null,"may_refine":false{extra}}}"#
        )
    }

    fn run(t: &[String], p: &[String]) -> Report {
        classify(
            &truth(t),
            &parse_presentations(&p.join("\n")).unwrap(),
            KeyMode::PathOnly,
        )
    }

    #[test]
    fn a_secret_field_is_judged_through_the_keyed_tag_of_the_log_header() {
        let key = gorca_model::RedactKey::from_bytes([5; 16]);
        let header = format!(r#"{{"redaction_key":"{}"}}"#, key.to_hex());
        let mut c = change(10 * MS, "/pw", "name", "old secret", "new secret", true);
        c.truncate(c.len() - 1);
        c.push_str(r#","sensitive":true}"#);
        let t = [c, focus(100 * MS, "/pw")];
        let new = key.tag("new secret");
        let old = key.tag("old secret");
        let p = [
            header.clone(),
            pres(101 * MS, 1, "/pw", "hit", &new, ""),
            pres(201 * MS, 2, "/pw", "hit", &old, ""),
        ]
        .join("\n");
        // The header line is not a presentation; it carries the key.
        assert_eq!(redaction_key(&p), Some(key));
        let mut truth = truth(&t);
        truth.redact(&key);
        let r = classify(&truth, &parse_presentations(&p).unwrap(), KeyMode::PathOnly);
        assert_eq!(r.judged[0].verdict, Verdict::Correct);
        assert!(
            !format!("{truth:?}").contains("new secret"),
            "the truth is redacted too"
        );
    }

    #[test]
    fn correct_when_name_matches_current_version() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", true),
            focus(100 * MS, "/x"),
        ];
        let r = run(&t, &[pres(101 * MS, 1, "/x", "hit", "New", "")]);
        assert_eq!(r.judged[0].verdict, Verdict::Correct);
        assert_eq!(r.judged[0].kind, Kind::Focus);
        assert_eq!(r.judged[0].latency_ns, MS);
    }

    #[test]
    fn stale_when_old_name_is_spoken_and_never_corrected() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", false),
            focus(100 * MS, "/x"),
        ];
        let r = run(&t, &[pres(101 * MS, 1, "/x", "hit", "Old", "")]);
        assert_eq!(r.judged[0].verdict, Verdict::Stale);
        assert_eq!(r.judged[0].mismatched, vec!["name"]);
        assert_eq!(r.judged[0].evented_cause, Some(false));
    }

    #[test]
    fn mutation_inside_the_window_makes_both_values_valid() {
        // The name changes 1 ms before the speak: the old one was valid at the stimulus.
        let t = [
            focus(100 * MS, "/x"),
            change(105 * MS, "/x", "name", "Old", "New", true),
        ];
        let r = run(&t, &[pres(106 * MS, 1, "/x", "hit", "Old", "")]);
        assert_eq!(r.judged[0].verdict, Verdict::Correct);
        assert_eq!(r.judged[0].kind, Kind::StateChange);
    }

    #[test]
    fn an_s1_line_is_a_state_change_with_its_own_latency_and_never_answers_a_focus() {
        // The daemon logs the spoken change of the focused node as source `s1`.
        let t = [
            focus(100 * MS, "/x"),
            change(200 * MS, "/x", "name", "A", "B", true),
        ];
        let p = [
            pres(101 * MS, 1, "/x", "hit", "A", ""),
            pres(203 * MS, 2, "/x", "s1", "B", ""),
        ];
        let r = run(&t, &p);
        assert_eq!(r.judged.len(), 2);
        assert_eq!(r.judged[1].source, Source::S1);
        assert_eq!(r.judged[1].kind, Kind::StateChange);
        assert_eq!(r.judged[1].verdict, Verdict::Correct);
        assert_eq!(r.judged[1].latency_ns, 3 * MS);
        assert_eq!(r.focus_presentations(), 1, "S1 is not a focus presentation");
        assert_eq!(r.latencies_ms(|j| j.kind == Kind::StateChange), vec![3.0]);
        // An S1 line alone does not hide a focus that was never spoken.
        let lost = run(&t, &p[1..]);
        assert_eq!(lost.lost, 1);
    }

    #[test]
    fn a_refine_before_the_audio_can_start_is_a_correction_but_not_an_audible_one() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", false),
            focus(100 * MS, "/x"),
        ];
        let quick = run(
            &t,
            &[
                pres(101 * MS, 1, "/x", "hit", "Old", ""),
                pres(104 * MS, 2, "/x", "refine", "New", r#","refines":1"#),
            ],
        );
        assert_eq!(quick.corrections(), 1);
        assert_eq!(quick.m6_audible().1, 0);
        assert_eq!(quick.refine_delays_ms(), vec![3.0]);
        let slow = run(
            &t,
            &[
                pres(101 * MS, 1, "/x", "hit", "Old", ""),
                pres(140 * MS, 2, "/x", "refine", "New", r#","refines":1"#),
            ],
        );
        assert_eq!(slow.m6_audible().1, 1);
    }

    #[test]
    fn refine_replacing_it_is_refined() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", false),
            focus(100 * MS, "/x"),
        ];
        let r = run(
            &t,
            &[
                pres(101 * MS, 1, "/x", "hit", "Old", ""),
                pres(120 * MS, 2, "/x", "refine", "New", r#","refines":1"#),
            ],
        );
        assert_eq!(r.judged[0].verdict, Verdict::Refined);
        assert_eq!(r.corrections(), 1);
        assert_eq!(r.m6().2, 1, "silent mutation");
    }

    #[test]
    fn refine_with_a_wrong_value_or_epoch_does_not_save_it() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", true),
            focus(100 * MS, "/x"),
        ];
        let r = run(
            &t,
            &[
                pres(101 * MS, 1, "/x", "hit", "Old", ""),
                pres(120 * MS, 2, "/x", "refine", "Old", r#","refines":1"#),
            ],
        );
        assert_eq!(r.judged[0].verdict, Verdict::Stale);
    }

    #[test]
    fn followup_within_500ms_is_late_after_that_stale() {
        let t = [
            change(10 * MS, "/x", "name", "Old", "New", true),
            focus(100 * MS, "/x"),
        ];
        let mk = |t_follow: u64| {
            run(
                &t,
                &[
                    pres(101 * MS, 1, "/x", "hit", "Old", ""),
                    pres(t_follow, 2, "/x", "followup", "New", r#","refines":1"#),
                ],
            )
        };
        assert_eq!(mk(400 * MS).judged[0].verdict, Verdict::Late);
        assert_eq!(mk(700 * MS).judged[0].verdict, Verdict::Stale);
    }

    #[test]
    fn states_compare_as_sets_and_ignore_the_focus_ring() {
        let t = [
            change(10 * MS, "/x", "states", "x", "x", true),
            focus(100 * MS, "/x"),
        ];
        // History for `states` is the string "x" -> ["x"]; the presentation says ["enabled"].
        let r = run(&t, &[pres(101 * MS, 1, "/x", "hit", "N", "")]);
        assert_eq!(r.judged[0].mismatched, vec!["states"]);
        let t2 = [
            r#"{"t_ns":10,"app":"a","path":"/x","field":"states","old":["enabled","focused"],"new":["enabled"],"evented":true}"#.to_owned(),
            focus(100 * MS, "/x"),
        ];
        let r2 = run(&t2, &[pres(101 * MS, 1, "/x", "hit", "N", "")]);
        assert_eq!(r2.judged[0].verdict, Verdict::Correct);
    }

    #[test]
    fn clean_revisit_and_hit_rates() {
        let t = [
            focus(100 * MS, "/x"),
            focus(200 * MS, "/y"),
            focus(300 * MS, "/x"),
            change(350 * MS, "/y", "name", "a", "b", true),
            focus(400 * MS, "/y"),
            focus(500 * MS, "/x"),
        ];
        let r = run(
            &t,
            &[
                pres(101 * MS, 1, "/x", "miss_reply", "n", ""),
                pres(201 * MS, 2, "/y", "miss_reply", "a", ""),
                pres(301 * MS, 3, "/x", "hit", "n", ""),
                pres(401 * MS, 4, "/y", "hit", "b", ""),
                pres(501 * MS, 5, "/x", "partial", "n", ""),
            ],
        );
        // Revisits: #3 (clean), #4 (mutated, not clean), #5 (clean).
        let (rate, n) = r.hit_revisit();
        assert_eq!(n, 2);
        assert!((rate - 0.5).abs() < 1e-9);
        assert!((r.hit_all() - 0.4).abs() < 1e-9);
    }

    #[test]
    fn lost_superseded_and_spoken_focus_lines() {
        let t = [
            focus(100 * MS, "/a"),   // spoken
            focus(200 * MS, "/b"),   // never spoken, but /c follows within 2 s: superseded
            focus(210 * MS, "/c"),   // spoken
            focus(9_000 * MS, "/d"), // never spoken, nothing after: lost
        ];
        let r = run(
            &t,
            &[
                pres(101 * MS, 1, "/a", "hit", "n", ""),
                pres(212 * MS, 2, "/c", "hit", "n", ""),
            ],
        );
        assert_eq!((r.lost, r.superseded), (1, 1));
    }

    #[test]
    fn orphan_followups_and_bad_input_are_reported() {
        let r = run(
            &[],
            &[pres(5, 9, "/x", "followup", "n", r#","refines":77"#)],
        );
        assert_eq!(r.orphan_followups, 1);
        assert!(parse_presentations(r#"{"t_ns":1,"utt_id":1,"source":"nope"}"#).is_err());
        assert!(parse_truth("not json").is_err());
    }

    #[test]
    fn unconstrained_fields_match_anything_but_history_is_enforced() {
        let t = [focus(100 * MS, "/x")];
        let r = run(&t, &[pres(101 * MS, 1, "/x", "hit", "whatever", "")]);
        assert_eq!(r.judged[0].verdict, Verdict::Correct);
        let t = [
            r#"{"t_ns":1,"app":"a","path":"/x","field":"name","init":"Seed"}"#.to_owned(),
            focus(100 * MS, "/x"),
        ];
        let r = run(&t, &[pres(101 * MS, 1, "/x", "hit", "whatever", "")]);
        assert_eq!(r.judged[0].verdict, Verdict::Stale);
    }
}
