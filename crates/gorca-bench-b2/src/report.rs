//! Result rows: a fixed table printed for humans, and the same data as JSON for comparison
//! across commits (`bench/results/b2-ingress/<label>/`).

use std::fmt::Write as _;

#[derive(Clone, Debug)]
pub enum Verdict {
    Pass,
    Fail,
    /// Reported only; no threshold applies.
    Info,
}

impl Verdict {
    pub fn label(&self) -> &'static str {
        match self {
            Verdict::Pass => "PASS",
            Verdict::Fail => "FAIL",
            Verdict::Info => "info",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Row {
    /// Stable metric id, e.g. `T2.p99_us@10k`. The comparison script keys on it.
    pub id: String,
    pub scenario: String,
    pub bus: String,
    /// Median over runs.
    pub value: f64,
    pub min: f64,
    pub max: f64,
    pub runs: usize,
    pub unit: String,
    /// `<=` or `>=` or `==`.
    pub op: &'static str,
    pub threshold: Option<f64>,
    pub verdict: Verdict,
    pub note: String,
}

impl Row {
    pub fn info(id: &str, scenario: &str, bus: &str, unit: &str, vals: &[f64]) -> Self {
        let s = crate::stats::spread(vals);
        Self {
            id: id.to_owned(),
            scenario: scenario.to_owned(),
            bus: bus.to_owned(),
            value: s.median,
            min: s.min,
            max: s.max,
            runs: s.runs,
            unit: unit.to_owned(),
            op: "",
            threshold: None,
            verdict: Verdict::Info,
            note: String::new(),
        }
    }

    /// `value <= threshold` must hold (latency, CPU, memory).
    pub fn at_most(mut self, threshold: f64) -> Self {
        self.op = "<=";
        self.threshold = Some(threshold);
        self.verdict = if self.value <= threshold && self.max.is_finite() {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        self
    }

    /// The **worst repeat** must satisfy `max <= threshold`: for counts that must be exactly zero
    /// (lost events), where a median over repeats would hide one bad run.
    pub fn at_most_in_every_run(mut self, threshold: f64) -> Self {
        self.op = "<=";
        self.threshold = Some(threshold);
        self.verdict = if self.max <= threshold {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        "gated on the max over repeats".clone_into(&mut self.note);
        self
    }

    /// `value >= threshold` must hold (throughput).
    pub fn at_least(mut self, threshold: f64) -> Self {
        self.op = ">=";
        self.threshold = Some(threshold);
        self.verdict = if self.value >= threshold {
            Verdict::Pass
        } else {
            Verdict::Fail
        };
        self
    }

    pub fn note(mut self, n: impl Into<String>) -> Self {
        self.note = n.into();
        self
    }
}

fn num(v: f64) -> String {
    if v.abs() >= 1000.0 || v == v.trunc() {
        format!("{v:.0}")
    } else if v.abs() >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.3}")
    }
}

/// Fixed-width table, one metric per line.
pub fn table(rows: &[Row]) -> String {
    let mut s = String::new();
    let _ = writeln!(
        s,
        "{:<26} {:<12} {:>12} {:>12} {:>12} {:>2} {:<4} {:>10}  {:<5} note",
        "metric", "bus", "median", "min", "max", "n", "op", "threshold", "res"
    );
    for r in rows {
        let _ = writeln!(
            s,
            "{:<26} {:<12} {:>12} {:>12} {:>12} {:>2} {:<4} {:>10}  {:<5} {} [{}]",
            r.id,
            r.bus,
            num(r.value),
            num(r.min),
            num(r.max),
            r.runs,
            r.op,
            r.threshold.map_or(String::new(), num),
            r.verdict.label(),
            r.note,
            r.unit,
        );
    }
    s
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o
}

fn jnum(v: f64) -> String {
    if v.is_finite() {
        format!("{v}")
    } else {
        "null".to_owned()
    }
}

/// Machine-readable result file. One object per metric; `meta` identifies the build and host.
pub fn json(meta: &[(String, String)], rows: &[Row]) -> String {
    let mut s = String::from("{\n  \"meta\": {");
    for (i, (k, v)) in meta.iter().enumerate() {
        let _ = write!(
            s,
            "{}\"{}\": \"{}\"",
            if i == 0 { "" } else { ", " },
            esc(k),
            esc(v)
        );
    }
    s.push_str("},\n  \"rows\": [\n");
    for (i, r) in rows.iter().enumerate() {
        let _ = write!(
            s,
            "    {{\"id\": \"{}\", \"scenario\": \"{}\", \"bus\": \"{}\", \"value\": {}, \"min\": {}, \"max\": {}, \"runs\": {}, \"unit\": \"{}\", \"op\": \"{}\", \"threshold\": {}, \"verdict\": \"{}\", \"note\": \"{}\"}}",
            esc(&r.id),
            esc(&r.scenario),
            esc(&r.bus),
            jnum(r.value),
            jnum(r.min),
            jnum(r.max),
            r.runs,
            esc(&r.unit),
            r.op,
            r.threshold.map_or("null".to_owned(), jnum),
            r.verdict.label(),
            esc(&r.note),
        );
        s.push_str(if i + 1 == rows.len() { "\n" } else { ",\n" });
    }
    s.push_str("  ]\n}\n");
    s
}

#[cfg(test)]
mod tests {
    #[test]
    fn zero_gates_look_at_the_worst_repeat() {
        let r = super::Row::info("lost", "L3", "b", "events", &[0.0, 0.0, 1.0])
            .at_most_in_every_run(0.0);
        assert_eq!(r.value, 0.0, "the median alone would pass");
        assert!(matches!(r.verdict, super::Verdict::Fail));
        let ok = super::Row::info("lost", "L3", "b", "events", &[0.0, 0.0, 0.0])
            .at_most_in_every_run(0.0);
        assert!(matches!(ok.verdict, super::Verdict::Pass));
    }

    use super::*;

    #[test]
    fn thresholds_decide_the_verdict() {
        let r = Row::info("x", "s", "mock", "us", &[1.0, 2.0, 3.0]).at_most(2.5);
        assert_eq!(r.verdict.label(), "PASS");
        let r = Row::info("x", "s", "mock", "us", &[3.0]).at_most(2.5);
        assert_eq!(r.verdict.label(), "FAIL");
        let r = Row::info("x", "s", "mock", "eps", &[100.0]).at_least(100.0);
        assert_eq!(r.verdict.label(), "PASS");
    }

    #[test]
    fn json_is_escaped() {
        let r = Row::info("a\"b", "s", "m", "u", &[1.0]).note("line\nbreak");
        let j = json(&[("k".into(), "v".into())], &[r]);
        assert!(j.contains("a\\\"b") && j.contains("line\\nbreak"));
    }
}
