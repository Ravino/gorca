//! Exact percentiles over collected samples, and spread across repeated runs.

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Summary {
    pub n: usize,
    pub min: f64,
    pub p50: f64,
    pub p95: f64,
    pub p99: f64,
    pub p999: f64,
    pub max: f64,
    pub mean: f64,
    pub stddev: f64,
}

/// Summarise samples (any unit). Sorts a copy; exact, no histogram error.
pub fn summarize(samples: &[f64]) -> Summary {
    if samples.is_empty() {
        return Summary::default();
    }
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let pick = |q: f64| {
        let idx = ((n as f64 - 1.0) * q).round() as usize;
        v[idx.min(n - 1)]
    };
    let mean = v.iter().sum::<f64>() / n as f64;
    let var = v.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / n as f64;
    Summary {
        n,
        min: v[0],
        p50: pick(0.50),
        p95: pick(0.95),
        p99: pick(0.99),
        p999: pick(0.999),
        max: v[n - 1],
        mean,
        stddev: var.sqrt(),
    }
}

/// Median, min and max of a scalar measured once per run.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spread {
    pub median: f64,
    pub min: f64,
    pub max: f64,
    pub runs: usize,
}

pub fn spread(values: &[f64]) -> Spread {
    if values.is_empty() {
        return Spread::default();
    }
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let median = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    Spread {
        median,
        min: v[0],
        max: v[n - 1],
        runs: n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_1_to_1000() {
        let s: Vec<f64> = (1..=1000).map(f64::from).collect();
        let r = summarize(&s);
        assert_eq!(r.n, 1000);
        assert!((r.p50 - 500.0).abs() <= 1.0);
        assert!((r.p99 - 990.0).abs() <= 1.0);
        assert!((r.p999 - 999.0).abs() <= 1.0);
        assert_eq!(r.max, 1000.0);
    }

    #[test]
    fn spread_even_and_odd() {
        assert_eq!(spread(&[3.0, 1.0, 2.0]).median, 2.0);
        assert_eq!(spread(&[4.0, 1.0, 2.0, 3.0]).median, 2.5);
    }
}
