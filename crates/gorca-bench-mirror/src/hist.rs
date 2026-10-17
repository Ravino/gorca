// SPDX-License-Identifier: GPL-3.0-or-later
//! Log-linear histogram over nanosecond values: 32 sub-buckets per power of two (about 3 %
//! relative error), mergeable, no allocation on record.

const SUB: u64 = 32;
const SUB_BITS: u32 = 5;
const BUCKETS: usize = (64 - SUB_BITS as usize + 1) * SUB as usize;

#[derive(Clone)]
pub struct Hist {
    counts: Vec<u64>,
    n: u64,
    max: u64,
}

impl std::fmt::Debug for Hist {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Hist(n={}, max={})", self.n, self.max)
    }
}

impl Default for Hist {
    fn default() -> Self {
        Self {
            counts: vec![0; BUCKETS],
            n: 0,
            max: 0,
        }
    }
}

fn index(v: u64) -> usize {
    if v < SUB {
        v as usize
    } else {
        let exp = 63 - v.leading_zeros();
        let sub = (v >> (exp - SUB_BITS)) & (SUB - 1);
        ((u64::from(exp - SUB_BITS) + 1) * SUB + sub) as usize
    }
}

/// Lower bound of a bucket.
fn lower(i: usize) -> u64 {
    let i = i as u64;
    if i < SUB {
        i
    } else {
        let exp = i / SUB + u64::from(SUB_BITS) - 1;
        let sub = i % SUB;
        (1u64 << exp) | (sub << (exp - u64::from(SUB_BITS)))
    }
}

impl Hist {
    pub fn record(&mut self, v: u64) {
        self.counts[index(v)] += 1;
        self.n += 1;
        self.max = self.max.max(v);
    }

    pub fn merge(&mut self, o: &Hist) {
        for (a, b) in self.counts.iter_mut().zip(&o.counts) {
            *a += b;
        }
        self.n += o.n;
        self.max = self.max.max(o.max);
    }

    pub fn count(&self) -> u64 {
        self.n
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// Value at quantile `q` (bucket lower bound, clamped to the observed max).
    pub fn q(&self, q: f64) -> u64 {
        if self.n == 0 {
            return 0;
        }
        let target = ((self.n as f64 * q).ceil() as u64).clamp(1, self.n);
        let mut acc = 0;
        for (i, c) in self.counts.iter().enumerate() {
            acc += c;
            if acc >= target {
                return lower(i).min(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_are_close() {
        let mut h = Hist::default();
        for v in 1..=100_000u64 {
            h.record(v * 10);
        }
        let p50 = h.q(0.5) as f64;
        assert!((p50 - 500_000.0).abs() / 500_000.0 < 0.04, "{p50}");
        let p99 = h.q(0.99) as f64;
        assert!((p99 - 990_000.0).abs() / 990_000.0 < 0.04, "{p99}");
        assert_eq!(h.max(), 1_000_000);
    }

    #[test]
    fn index_is_monotone() {
        let mut last = 0;
        for v in (0..5_000_000u64).step_by(7) {
            let i = index(v);
            assert!(i >= last);
            assert!(lower(i) <= v);
            last = i;
        }
    }
}
