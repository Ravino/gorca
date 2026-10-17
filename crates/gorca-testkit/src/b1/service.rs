// SPDX-License-Identifier: GPL-3.0-or-later
//! Seeded randomness and the serial service-time model (SPEC 8.1, D-1).

use super::proto::ServiceSpec;

/// SplitMix64: tiny, fast, good enough, and a seed reproduces a run exactly.
#[derive(Clone, Debug)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }

    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)`.
    #[allow(clippy::cast_precision_loss)]
    pub fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform integer in `lo..=hi` (`lo` when `hi <= lo`).
    pub fn range(&mut self, lo: u64, hi: u64) -> u64 {
        if hi <= lo {
            lo
        } else {
            lo + self.next_u64() % (hi - lo + 1)
        }
    }

    /// Uniform float in `[lo, hi)`.
    pub fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.unit() * (hi - lo)
    }

    /// Standard normal (Box-Muller).
    pub fn normal(&mut self) -> f64 {
        let u1 = self.unit().max(1e-12);
        let u2 = self.unit();
        (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
    }

    pub fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }

    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        let i = usize::try_from(self.next_u64() % items.len() as u64).unwrap_or(0);
        items.get(i)
    }
}

/// Log-normal service time with the given median and 99th percentile.
#[derive(Clone, Debug)]
pub struct ServiceModel {
    median_ns: f64,
    sigma: f64,
    rng: Rng,
    /// Hard cap: 10 x p99, so one outlier cannot run away.
    cap_ns: f64,
}

/// z-score of the 99th percentile of the standard normal.
const Z99: f64 = 2.326_347_874_040_841;

impl ServiceModel {
    pub fn new(spec: ServiceSpec, seed: u64) -> Self {
        let p50 = spec.p50_ms.max(1e-6);
        let p99 = spec.p99_ms.max(p50);
        Self {
            median_ns: p50 * 1e6,
            sigma: (p99 / p50).ln() / Z99,
            rng: Rng::new(seed),
            cap_ns: p99 * 1e6 * 10.0,
        }
    }

    /// One service time in nanoseconds.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn sample_ns(&mut self) -> u64 {
        let z = self.rng.normal();
        (self.median_ns * (self.sigma * z).exp()).min(self.cap_ns) as u64
    }

    /// Per-item cost added to a bulk call (`GetItems`).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    pub fn bulk_ns(items: usize, us_per_item: f64) -> u64 {
        (items as f64 * us_per_item * 1e3) as u64
    }

    pub fn rng(&mut self) -> &mut Rng {
        &mut self.rng
    }
}

/// Percentile of an unsorted sample (nearest rank), `p` in `0.0..=100.0`.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
pub fn percentile(samples: &[u64], p: f64) -> u64 {
    if samples.is_empty() {
        return 0;
    }
    let mut v = samples.to_vec();
    v.sort_unstable();
    let rank = ((p / 100.0) * v.len() as f64).ceil().max(1.0) as usize;
    v[rank.min(v.len()) - 1]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_deterministic() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert_ne!(Rng::new(7).next_u64(), Rng::new(8).next_u64());
    }

    #[test]
    fn unit_is_in_range() {
        let mut r = Rng::new(1);
        for _ in 0..10_000 {
            let u = r.unit();
            assert!((0.0..1.0).contains(&u));
        }
    }

    #[test]
    fn service_times_match_the_spec_percentiles() {
        let mut m = ServiceModel::new(
            ServiceSpec {
                p50_ms: 0.15,
                p99_ms: 1.0,
            },
            42,
        );
        let s: Vec<u64> = (0..200_000).map(|_| m.sample_ns()).collect();
        let p50 = percentile(&s, 50.0) as f64 / 1e6;
        let p99 = percentile(&s, 99.0) as f64 / 1e6;
        assert!((p50 - 0.15).abs() / 0.15 < 0.05, "p50 {p50}");
        assert!((p99 - 1.0).abs() / 1.0 < 0.10, "p99 {p99}");
    }

    #[test]
    fn bulk_cost_is_linear() {
        assert_eq!(ServiceModel::bulk_ns(1000, 15.0), 15_000_000);
        assert_eq!(ServiceModel::bulk_ns(0, 15.0), 0);
    }

    #[test]
    fn percentile_nearest_rank() {
        let v: Vec<u64> = (1..=100).collect();
        assert_eq!(percentile(&v, 50.0), 50);
        assert_eq!(percentile(&v, 99.0), 99);
        assert_eq!(percentile(&[], 50.0), 0);
    }
}
