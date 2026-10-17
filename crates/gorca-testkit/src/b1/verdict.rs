// SPDX-License-Identifier: GPL-3.0-or-later
//! Verdict per presentation (SPEC 6.2), the small in-testkit version used by the conformance cases.
//!
//! The runner's classifier (`gorca-bench-b1::verdict`) works on the on-disk logs and is the
//! authority for the gates; this one works on [`Presentation`]s and [`TruthLine`]s in memory so a
//! case can ask "was this utterance right?" without a file round trip. Both implement the same
//! definitions: the set `V` of truth versions valid at some instant in `[t_stimulus, t_speak]`.

use std::collections::BTreeMap;

use serde_json::Value;

use gorca_model::RedactKey;

use super::proto::TruthLine;
use super::sut::{Presentation, Source};
use super::world::state;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    Correct,
    Refined,
    Late,
    Stale,
}

/// Value timeline of one (app, path, field): `(start_ns, value)` ascending; the first entry is
/// the value before the first mutation (`start 0`).
#[derive(Debug, Default)]
struct Timeline(Vec<(u64, Value)>);

impl Timeline {
    /// Values valid at some instant in `[from, to]`.
    fn valid(&self, from: u64, to: u64) -> Vec<&Value> {
        let mut out = Vec::new();
        for (i, (start, v)) in self.0.iter().enumerate() {
            let end = self.0.get(i + 1).map_or(u64::MAX, |n| n.0);
            if *start <= to && end > from {
                out.push(v);
            }
        }
        out
    }
}

/// Truth versions per (app, path, field).
#[derive(Debug, Default)]
pub struct TruthDb {
    by: BTreeMap<(String, String, String), Timeline>,
}

impl TruthDb {
    pub fn new(lines: &[TruthLine]) -> Self {
        let mut db = Self::default();
        let mut sorted: Vec<&TruthLine> = lines.iter().filter(|l| !l.focus).collect();
        sorted.sort_by_key(|l| l.t_ns);
        for l in sorted {
            let tl = db
                .by
                .entry((l.app.clone(), l.path.clone(), l.field.clone()))
                .or_default();
            if tl.0.is_empty() {
                tl.0.push((0, l.old.clone()));
            }
            tl.0.push((l.t_ns, l.new.clone()));
        }
        db
    }

    fn valid(&self, app: &str, path: &str, field: &str, from: u64, to: u64) -> Option<Vec<&Value>> {
        self.by
            .get(&(app.to_owned(), path.to_owned(), field.to_owned()))
            .map(|t| t.valid(from, to))
    }

    /// Does `p` match some truth version valid in `[from, to]` on every field that has a history?
    /// Fields that never mutated are not constrained (their value cannot be stale).
    pub fn matches(&self, p: &Presentation, from: u64, to: u64) -> bool {
        if let Some(v) = self.valid(&p.app, &p.path, "name", from, to)
            && !v.iter().any(|x| x.as_str() == Some(p.name.as_str()))
        {
            return false;
        }
        if let Some(v) = self.valid(&p.app, &p.path, "value", from, to) {
            // A secret field's value reaches the log as a keyed tag, and its truth is the tag too.
            let redacted = p.value.as_deref().filter(|x| x.starts_with("redacted("));
            let said = p.value.as_deref().and_then(|x| x.parse::<f64>().ok());
            let ok = match redacted {
                Some(tag) => v.iter().any(|x| x.as_str() == Some(tag)),
                None => v.iter().any(|x| x.as_f64() == said),
            };
            if !ok {
                return false;
            }
        }
        if let Some(v) = self.valid(&p.app, &p.path, "states", from, to) {
            let said = serde_json::json!(state::names_of(p.states));
            if !v.iter().any(|x| **x == said) {
                return false;
            }
        }
        true
    }

    /// Was the node destroyed (or its lease expired) before `t`?
    pub fn dead_before(&self, app: &str, path: &str, t: u64) -> bool {
        ["destroyed", "lease"].iter().any(|f| {
            self.by
                .get(&(app.to_owned(), path.to_owned(), (*f).to_owned()))
                .is_some_and(|tl| tl.0.iter().skip(1).any(|(s, _)| *s <= t))
        })
    }
}

/// The key of a presentation log: its header line (`{"redaction_key":"<hex>"}`).
pub fn redaction_key(log: &str) -> Option<RedactKey> {
    let first = log.lines().next()?;
    let v: Value = serde_json::from_str(first).ok()?;
    RedactKey::from_hex(v.get("redaction_key")?.as_str()?)
}

/// Redact the secret fields of a truth log the way the daemon redacts its presentation log, so
/// that the two still compare. Strings and numbers of a `sensitive` line become tags.
pub fn redact_truth(lines: &mut [TruthLine], key: &RedactKey) {
    let tag = |v: &Value| match v {
        Value::String(s) => Value::String(key.tag(s)),
        Value::Number(n) => Value::String(key.tag(&n.as_f64().unwrap_or(f64::NAN).to_string())),
        other => other.clone(),
    };
    for l in lines.iter_mut().filter(|l| l.sensitive && !l.focus) {
        l.old = tag(&l.old);
        l.new = tag(&l.new);
    }
}

/// Classify one utterance. `all` is the full presentation list (for refinements).
pub fn classify(p: &Presentation, all: &[Presentation], db: &TruthDb) -> Verdict {
    if matches!(p.source, Source::Refine | Source::Followup) {
        // Corrections are judged through the utterance they correct.
        return if db.matches(p, p.stimulus_ns, p.t_ns) {
            Verdict::Correct
        } else {
            Verdict::Stale
        };
    }
    if db.matches(p, p.stimulus_ns, p.t_ns) {
        return Verdict::Correct;
    }
    for r in all.iter().filter(|r| r.refines == Some(p.utt)) {
        if !db.matches(r, p.stimulus_ns, r.t_ns) {
            continue;
        }
        match r.source {
            Source::Refine => return Verdict::Refined,
            Source::Followup if r.t_ns.saturating_sub(p.t_ns) <= 500_000_000 => {
                return Verdict::Late;
            }
            _ => {}
        }
    }
    Verdict::Stale
}

/// Verdicts of every first utterance (corrections excluded), in order.
pub fn classify_all(all: &[Presentation], truth: &[TruthLine]) -> Vec<(Presentation, Verdict)> {
    let db = TruthDb::new(truth);
    all.iter()
        .filter(|p| !matches!(p.source, Source::Refine | Source::Followup))
        .map(|p| (p.clone(), classify(p, all, &db)))
        .collect()
}

/// Corrections (`REFINED` + `LATE`) over first utterances.
pub fn corrections(v: &[(Presentation, Verdict)]) -> usize {
    v.iter()
        .filter(|(_, x)| matches!(x, Verdict::Refined | Verdict::Late))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: u64, path: &str, field: &str, old: Value, new: Value) -> TruthLine {
        TruthLine {
            t_ns: t,
            app: "a".into(),
            path: path.into(),
            field: field.into(),
            old,
            new,
            evented: true,
            sensitive: false,
            focus: false,
        }
    }

    fn pres(
        utt: u64,
        t: u64,
        stim: u64,
        name: &str,
        src: Source,
        refines: Option<u64>,
    ) -> Presentation {
        Presentation {
            t_ns: t,
            stimulus_ns: stim,
            utt,
            app: "a".into(),
            app_bus: ":1.1".into(),
            path: "/p".into(),
            source: src,
            role: 43,
            states: 0,
            name: name.into(),
            value: None,
            may_refine: false,
            refines,
        }
    }

    #[test]
    fn a_secret_field_compares_through_the_same_keyed_tag_on_both_sides() {
        let key = RedactKey::from_bytes([9; 16]);
        let mut l = line(100, "/p", "name", "old pw".into(), "new pw".into());
        l.sensitive = true;
        let mut truth = [l];
        redact_truth(&mut truth, &key);
        assert!(!format!("{:?}", truth[0]).contains("new pw"));
        let db = TruthDb::new(&truth);
        // The daemon logged the tag of what it presented.
        let p = pres(1, 300, 200, &key.tag("new pw"), Source::MissReply, None);
        assert_eq!(
            classify(&p, std::slice::from_ref(&p), &db),
            Verdict::Correct
        );
        let stale = pres(2, 900, 800, &key.tag("old pw"), Source::Hit, None);
        assert_eq!(
            classify(&stale, std::slice::from_ref(&stale), &db),
            Verdict::Stale
        );
        // The key comes from the log's header.
        let log = format!("{{\"redaction_key\":\"{}\"}}\n{{}}\n", key.to_hex());
        assert_eq!(redaction_key(&log), Some(key));
        assert_eq!(redaction_key("{}\n"), None);
    }

    #[test]
    fn correct_when_the_name_is_a_valid_version() {
        let db = TruthDb::new(&[line(100, "/p", "name", "Old".into(), "New".into())]);
        let p = pres(1, 300, 200, "New", Source::MissReply, None);
        assert_eq!(
            classify(&p, std::slice::from_ref(&p), &db),
            Verdict::Correct
        );
    }

    #[test]
    fn a_version_replaced_inside_the_window_is_still_valid() {
        // Mutation at 250 is inside [200, 300]: both Old and New are acceptable.
        let db = TruthDb::new(&[line(250, "/p", "name", "Old".into(), "New".into())]);
        for n in ["Old", "New"] {
            let p = pres(1, 300, 200, n, Source::Hit, None);
            assert_eq!(
                classify(&p, std::slice::from_ref(&p), &db),
                Verdict::Correct,
                "{n}"
            );
        }
    }

    #[test]
    fn old_name_long_after_the_change_is_stale_unless_corrected() {
        let db = TruthDb::new(&[line(100, "/p", "name", "Old".into(), "New".into())]);
        let bad = pres(1, 300, 200, "Old", Source::Hit, None);
        assert_eq!(
            classify(&bad, std::slice::from_ref(&bad), &db),
            Verdict::Stale
        );
        let refine = pres(2, 350, 200, "New", Source::Refine, Some(1));
        assert_eq!(
            classify(&bad, &[bad.clone(), refine], &db),
            Verdict::Refined
        );
        let late = pres(3, 300 + 400_000_000, 200, "New", Source::Followup, Some(1));
        assert_eq!(classify(&bad, &[bad.clone(), late], &db), Verdict::Late);
        let too_late = pres(4, 300 + 600_000_000, 200, "New", Source::Followup, Some(1));
        assert_eq!(
            classify(&bad, &[bad.clone(), too_late], &db),
            Verdict::Stale
        );
    }

    #[test]
    fn tracked_state_bits_are_compared() {
        let db = TruthDb::new(&[line(
            100,
            "/p",
            "states",
            serde_json::json!([]),
            serde_json::json!(["checked"]),
        )]);
        let mut p = pres(1, 300, 200, "x", Source::Hit, None);
        assert_eq!(classify(&p, std::slice::from_ref(&p), &db), Verdict::Stale);
        p.states = 1 << state::CHECKED;
        assert_eq!(
            classify(&p, std::slice::from_ref(&p), &db),
            Verdict::Correct
        );
    }

    #[test]
    fn unmutated_fields_are_unconstrained_and_corrections_count() {
        let db = TruthDb::new(&[]);
        let p = pres(1, 3, 2, "anything", Source::Hit, None);
        assert_eq!(
            classify(&p, std::slice::from_ref(&p), &db),
            Verdict::Correct
        );
        let v = vec![(p, Verdict::Refined)];
        assert_eq!(corrections(&v), 1);
    }
}
