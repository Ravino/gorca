// SPDX-License-Identifier: GPL-3.0-or-later
//! Decayed log-histograms (ADR-0026 section 2.4.7.1). Integer only.
//!
//! Each sample adds [`WEIGHT`]; every [`HALF`] samples all counts halve (half-life 64 calls).
//! Quantiles are **upper** bucket edges (never under-estimate a deadline); the hit rate counts
//! only buckets whose upper edge is at most `t` (never over-estimates it). Both are conservative
//! on purpose.

/// Q16 one.
pub const Q: i64 = 1 << 16;
/// Number of buckets.
pub const NB: usize = 16;
/// Lower bucket edges in microseconds; bucket `b` is `[EDGES[b], EDGES[b + 1])`, the last
/// `[1024, inf)`.
pub const EDGES: [u32; NB] = [
    0, 4, 8, 12, 16, 24, 32, 48, 64, 96, 128, 192, 256, 384, 512, 1024,
];
/// The upper edge used for the open last bucket.
pub const INF_US: u32 = 4096;
/// Weight of one sample.
pub const WEIGHT: u32 = 16;
/// All counts halve after this many samples.
pub const HALF: u32 = 64;

/// Upper edge of bucket `b`.
#[must_use]
pub const fn upper(b: usize) -> u32 {
    if b + 1 < NB { EDGES[b + 1] } else { INF_US }
}

/// Bucket of a value.
#[must_use]
pub fn bucket(x_us: u32) -> usize {
    // EDGES[1..] are the bucket boundaries: the bucket is the number of boundaries <= x.
    EDGES[1..].partition_point(|e| *e <= x_us)
}

/// A decayed histogram.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Hist {
    pub(crate) c: [u32; NB],
    /// Samples seen (saturating).
    pub(crate) n: u32,
    since_half: u32,
    total: u32,
    /// `cum[k]`: Q16 mass of the first `k` buckets (`floor(acc * Q / total)`), 0 when empty.
    cum: [u32; NB + 1],
}

impl Hist {
    /// An empty histogram.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Samples seen.
    #[must_use]
    pub fn n(&self) -> u32 {
        self.n
    }

    /// Total weight.
    #[must_use]
    pub fn total(&self) -> u32 {
        self.total
    }

    /// Add one sample. Returns false when a counter would have overflowed (the caller falls back).
    pub fn add(&mut self, x_us: u32) -> bool {
        let b = bucket(x_us);
        let mut ok = true;
        match self.c[b].checked_add(WEIGHT) {
            Some(v) => self.c[b] = v,
            None => ok = false,
        }
        self.n = self.n.saturating_add(1);
        self.since_half += 1;
        if self.since_half >= HALF {
            self.since_half = 0;
            for v in &mut self.c {
                *v >>= 1;
            }
        }
        self.refresh();
        ok
    }

    /// Recompute the cached total and cumulative masses (after any change of `c`).
    pub(crate) fn refresh(&mut self) {
        let mut acc: u64 = 0;
        for v in &self.c {
            acc += u64::from(*v);
        }
        self.total = u32::try_from(acc).unwrap_or(u32::MAX);
        let tot = u64::from(self.total);
        let mut run: u64 = 0;
        self.cum[0] = 0;
        for (b, v) in self.c.iter().enumerate() {
            run += u64::from(*v);
            self.cum[b + 1] = (run * Q as u64)
                .checked_div(tot)
                .map_or(0, |v| u32::try_from(v).unwrap_or(u32::MAX));
        }
    }

    /// Smallest bucket upper edge `u` with `F(u) >= p` (p in Q16); [`INF_US`] when empty.
    #[must_use]
    pub fn quantile(&self, p_q16: i64) -> u32 {
        let tot = i64::from(self.total);
        if tot == 0 {
            return INF_US;
        }
        let mut acc: i64 = 0;
        for b in 0..NB {
            acc += i64::from(self.c[b]);
            if acc * Q >= p_q16 * tot {
                return upper(b);
            }
        }
        INF_US
    }

    /// Q16 lower bound of `P(X < t)`: the mass of buckets whose upper edge is `<= t`.
    #[must_use]
    pub fn cdf_at(&self, t_us: i64) -> i64 {
        if t_us < i64::from(EDGES[1]) {
            return 0;
        }
        // Upper edges are EDGES[1..] followed by INF_US, strictly increasing.
        let k = if t_us >= i64::from(INF_US) {
            NB
        } else {
            EDGES[1..].partition_point(|e| i64::from(*e) <= t_us)
        };
        i64::from(self.cum[k])
    }

    /// `(upper edge, Q16 mass)` pairs of the non-empty buckets.
    #[must_use]
    pub fn pmf(&self) -> Pmf {
        let mut out = Pmf::default();
        let tot = u64::from(self.total);
        if tot == 0 {
            out.push(INF_US, Q);
            return out;
        }
        for (b, v) in self.c.iter().enumerate() {
            if *v != 0 {
                out.push(upper(b), (u64::from(*v) * Q as u64 / tot) as i64);
            }
        }
        out
    }
}

/// A sparse probability mass function over bucket upper edges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pmf {
    len: u8,
    up: [u32; NB],
    m: [i64; NB],
}

impl Pmf {
    fn push(&mut self, up: u32, m: i64) {
        let i = usize::from(self.len);
        if i < NB {
            self.up[i] = up;
            self.m[i] = m;
            self.len += 1;
        }
    }

    /// The entries.
    pub fn iter(&self) -> impl Iterator<Item = (u32, i64)> + '_ {
        (0..usize::from(self.len)).map(|i| (self.up[i], self.m[i]))
    }
}

/// `old + (x - old) / 2^shift` with an arithmetic shift (floor), identical in `sps.py`.
#[must_use]
pub fn ewma_q16(old: i64, x_q16: i64, shift: u32) -> i64 {
    old + ((x_q16 - old) >> shift)
}
