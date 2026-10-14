//! Minimal JSON writer and sample statistics for the harness output (no serde_json in the
//! workspace; the output is flat and small).

use std::fmt::Write as _;

#[derive(Clone, Debug)]
pub enum J {
    Null,
    B(bool),
    N(f64),
    S(String),
    A(Vec<J>),
    O(Vec<(String, J)>),
}

pub fn obj<const K: usize>(pairs: [(&str, J); K]) -> J {
    J::O(pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect())
}

pub fn s(v: impl Into<String>) -> J {
    J::S(v.into())
}

pub fn n(v: impl Into<f64>) -> J {
    J::N(v.into())
}

#[allow(clippy::cast_precision_loss)]
pub fn u(v: usize) -> J {
    J::N(v as f64)
}

impl J {
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write(&mut out, 0);
        out.push('\n');
        out
    }

    fn write(&self, out: &mut String, depth: usize) {
        match self {
            J::Null => out.push_str("null"),
            J::B(b) => out.push_str(if *b { "true" } else { "false" }),
            J::N(x) => {
                if x.is_finite() {
                    if (x.fract()).abs() < 1e-9 && x.abs() < 1e15 {
                        let _ = write!(out, "{}", *x as i64);
                    } else {
                        let _ = write!(out, "{x:.4}");
                    }
                } else {
                    out.push_str("null");
                }
            }
            J::S(t) => {
                out.push('"');
                for c in t.chars() {
                    match c {
                        '"' => out.push_str("\\\""),
                        '\\' => out.push_str("\\\\"),
                        '\n' => out.push_str("\\n"),
                        '\t' => out.push_str("\\t"),
                        c if (c as u32) < 0x20 => {
                            let _ = write!(out, "\\u{:04x}", c as u32);
                        }
                        c => out.push(c),
                    }
                }
                out.push('"');
            }
            J::A(v) => {
                if v.iter().all(|x| !matches!(x, J::O(_) | J::A(_))) {
                    out.push('[');
                    for (i, x) in v.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        x.write(out, depth + 1);
                    }
                    out.push(']');
                } else {
                    out.push_str("[\n");
                    for (i, x) in v.iter().enumerate() {
                        out.push_str(&"  ".repeat(depth + 1));
                        x.write(out, depth + 1);
                        if i + 1 < v.len() {
                            out.push(',');
                        }
                        out.push('\n');
                    }
                    out.push_str(&"  ".repeat(depth));
                    out.push(']');
                }
            }
            J::O(kv) => {
                out.push_str("{\n");
                for (i, (k, x)) in kv.iter().enumerate() {
                    out.push_str(&"  ".repeat(depth + 1));
                    let _ = write!(out, "\"{k}\": ");
                    x.write(out, depth + 1);
                    if i + 1 < kv.len() {
                        out.push(',');
                    }
                    out.push('\n');
                }
                out.push_str(&"  ".repeat(depth));
                out.push('}');
            }
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub max: f64,
    pub mean: f64,
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
pub fn summarize(samples: &[f64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let mut v = samples.to_vec();
    v.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let q = |p: f64| {
        let i = ((v.len() as f64 - 1.0) * p).round() as usize;
        v[i.min(v.len() - 1)]
    };
    Summary {
        n: v.len(),
        min: v[0],
        p50: q(0.5),
        p95: q(0.95),
        p99: q(0.99),
        max: v[v.len() - 1],
        mean: v.iter().sum::<f64>() / v.len() as f64,
    }
}

impl Summary {
    pub fn to_json(&self) -> J {
        obj([
            ("n", u(self.n)),
            ("min", n(self.min)),
            ("p50", n(self.p50)),
            ("p95", n(self.p95)),
            ("p99", n(self.p99)),
            ("max", n(self.max)),
            ("mean", n(self.mean)),
        ])
    }
}

pub fn ms(d: std::time::Duration) -> f64 {
    d.as_secs_f64() * 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_renders_nested_values() {
        let j = obj([
            ("a", n(1.0)),
            ("b", J::A(vec![n(1.5), s("x\"y")])),
            ("c", obj([("d", J::B(true))])),
        ]);
        let t = j.render();
        assert!(t.contains("\"a\": 1"));
        assert!(t.contains("[1.5000, \"x\\\"y\"]"));
        assert!(t.contains("\"d\": true"));
    }

    #[test]
    fn summary_percentiles() {
        let v: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = summarize(&v);
        assert_eq!(s.n, 100);
        assert!((s.p50 - 51.0).abs() < 1.5 && (s.p99 - 99.0).abs() < 1.5);
    }
}
