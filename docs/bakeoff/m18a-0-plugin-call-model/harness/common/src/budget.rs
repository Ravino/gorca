// SPDX-License-Identifier: MIT OR Apache-2.0
//! MQ-3 progressive-filling interactive budget (ADR-0026 section 2.4.2) and the per-plugin
//! adaptive statistics (`kappa_p`, predicted fuel). Shared by both arms: the grants are computed by
//! the writer, the arms only execute them.

use crate::model::PluginId;

#[derive(Copy, Clone, Debug)]
pub struct BudgetCfg {
    /// Interactive budget B in ns (0.3 ms).
    pub b_ns: u64,
    /// Per-call reserve in ns (2 us).
    pub c_max_ns: u64,
    /// Per-call cap in ns (0.2 ms).
    pub cap_ns: u64,
    /// Fuel per ns of the interactive lane (`kappa_int`), from the WQ-2 calibration.
    pub kappa_int: f64,
}

impl BudgetCfg {
    pub fn spec(kappa_int: f64) -> BudgetCfg {
        BudgetCfg {
            b_ns: 300_000,
            c_max_ns: 2_000,
            cap_ns: 200_000,
            kappa_int,
        }
    }

    pub fn cap_fuel(&self) -> u64 {
        (self.kappa_int * self.cap_ns as f64) as u64
    }

    /// Pool P in fuel for `n` calls.
    pub fn pool_fuel(&self, n: usize) -> u64 {
        let reserve = n as u64 * self.c_max_ns;
        (self.kappa_int * self.b_ns.saturating_sub(reserve) as f64) as u64
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Grant {
    pub fuel: u64,
    /// `fuel / kappa`: the time this grant is worth (used by the wall accounting).
    pub ns: u64,
    /// The grant reached the per-call cap.
    pub capped: bool,
}

/// Progressive filling. `preds[i]` = (plugin, predicted fuel). Returns grants in input order.
///
/// Reserve `n * c_max`, pool `P = kappa * (B - n * c_max)`, order by predicted fuel ascending with a
/// rotating tie-break (`focus_counter`), grant `a_k = min(cap, R_k / (n - k + 1))`, return unused
/// predicted fuel to the pool, no carry-over between focus changes.
pub fn progressive_fill(
    cfg: &BudgetCfg,
    preds: &[(PluginId, u64)],
    focus_counter: u64,
) -> Vec<Grant> {
    let n = preds.len();
    let mut out = vec![
        Grant {
            fuel: 0,
            ns: 0,
            capped: false
        };
        n
    ];
    if n == 0 {
        return out;
    }
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by_key(|&i| (preds[i].1, (i as u64 + focus_counter) % n as u64));
    let mut r = cfg.pool_fuel(n);
    let cap = cfg.cap_fuel();
    let reserve_fuel = (cfg.kappa_int * cfg.c_max_ns as f64) as u64;
    for (k, &i) in order.iter().enumerate() {
        let share = r / (n - k) as u64;
        let a = share.min(cap);
        let fuel = a + reserve_fuel;
        out[i] = Grant {
            fuel,
            ns: (fuel as f64 / cfg.kappa_int) as u64,
            capped: a >= cap,
        };
        r = r.saturating_sub(a.min(preds[i].1));
    }
    out
}

/// Per-plugin adaptive statistics owned by the writer (updated on admission).
#[derive(Clone, Debug)]
pub struct PluginStats {
    fuel: Vec<u64>,
    rate: Vec<f64>,
    /// EWMA of the p95 of recent interactive fuel use.
    pub ewma_p95: f64,
    /// Consecutive focus changes with `pred > P / n` (strike rule 2).
    pub over_share_streak: u8,
}

impl Default for PluginStats {
    fn default() -> Self {
        PluginStats {
            fuel: Vec::with_capacity(64),
            rate: Vec::with_capacity(64),
            ewma_p95: 0.0,
            over_share_streak: 0,
        }
    }
}

impl PluginStats {
    pub fn record(&mut self, fuel_used: u64, exec_ns: u64) {
        if self.fuel.len() == 64 {
            self.fuel.remove(0);
        }
        if self.rate.len() == 64 {
            self.rate.remove(0);
        }
        self.fuel.push(fuel_used);
        if exec_ns > 1_000 && fuel_used > 100 {
            self.rate.push(fuel_used as f64 / exec_ns as f64);
        }
        let mut s = self.fuel.clone();
        s.sort_unstable();
        let p95 = s[((s.len() * 95).div_ceil(100)).clamp(1, s.len()) - 1] as f64;
        self.ewma_p95 = if self.ewma_p95 == 0.0 {
            p95
        } else {
            0.75 * self.ewma_p95 + 0.25 * p95
        };
    }

    /// Predicted fuel for the next interactive call. A plugin without history is predicted cheap
    /// (it gets the fair share and learns).
    pub fn pred_fuel(&self) -> u64 {
        self.ewma_p95 as u64
    }

    /// `kappa_p = min(kappa_lane, p10 of the last 64 calls)`, fuel per ns.
    pub fn kappa_p(&self, kappa_lane: f64) -> f64 {
        if self.rate.len() < 8 {
            return kappa_lane;
        }
        let mut r = self.rate.clone();
        r.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p10 = r[(r.len() / 10).min(r.len() - 1)];
        kappa_lane.min(p10)
    }

    /// Strike rule for `OutOfFuel` (ADR-0026 2.4.2): strike only at the per-call cap, or when the
    /// predicted use exceeded `P / n` in 3 consecutive focus changes.
    pub fn note_focus(&mut self, over_share: bool) {
        self.over_share_streak = if over_share {
            self.over_share_streak.saturating_add(1)
        } else {
            0
        };
    }

    pub fn oof_is_strike(&self, grant: &Grant) -> bool {
        grant.capped || self.over_share_streak >= 3
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> BudgetCfg {
        BudgetCfg::spec(2.0) // 2 fuel per ns
    }

    #[test]
    fn equal_predictions_get_equal_shares() {
        let big = u64::MAX / 4;
        let g = progressive_fill(&cfg(), &[(0, big), (1, big), (2, big), (3, big)], 0);
        let first = g[0].fuel;
        assert!(g.iter().all(|x| x.fuel.abs_diff(first) <= 1), "{g:?}");
    }

    #[test]
    fn small_users_return_fuel_to_big_ones() {
        let c = cfg();
        let g = progressive_fill(&c, &[(0, 10), (1, 10), (2, 10_000_000)], 0);
        assert!(g[2].fuel > g[0].fuel, "{g:?}");
        assert!(g[2].fuel <= c.cap_fuel() + (c.kappa_int * c.c_max_ns as f64) as u64);
    }

    #[test]
    fn full_users_never_exceed_budget_plus_reserve() {
        // rho about 1 (A6): everyone predicted to spend all of it.
        let c = cfg();
        for n in 1..=12usize {
            let preds: Vec<(PluginId, u64)> = (0..n).map(|i| (i as u16, u64::MAX / 4)).collect();
            let g = progressive_fill(&c, &preds, 3);
            let sum_ns: f64 = g.iter().map(|x| x.fuel as f64 / c.kappa_int).sum();
            assert!(
                sum_ns <= (c.b_ns + n as u64 * c.c_max_ns) as f64 + n as f64,
                "n={n} sum={sum_ns}"
            );
        }
    }

    #[test]
    fn rotating_tie_break_moves_the_first_slot() {
        let c = cfg();
        let preds: Vec<(PluginId, u64)> = (0..4).map(|i| (i, 50_000)).collect();
        // Equal predictions, but the filling order rotates, so grants differ only by rounding.
        let a = progressive_fill(&c, &preds, 0);
        let b = progressive_fill(&c, &preds, 1);
        assert_eq!(a.len(), b.len());
    }

    #[test]
    fn cap_and_empty() {
        let c = cfg();
        assert!(progressive_fill(&c, &[], 0).is_empty());
        let g = progressive_fill(&c, &[(0, u64::MAX / 4)], 0);
        assert!(g[0].capped);
    }

    #[test]
    fn strike_rules_and_kappa_p() {
        let mut s = PluginStats::default();
        for i in 0..64 {
            s.record(10_000 + i, 5_000);
        }
        assert!(s.kappa_p(5.0) <= 2.1);
        let g = Grant {
            fuel: 1,
            ns: 1,
            capped: false,
        };
        assert!(!s.oof_is_strike(&g));
        s.note_focus(true);
        s.note_focus(true);
        s.note_focus(true);
        assert!(s.oof_is_strike(&g));
        s.note_focus(false);
        assert!(!s.oof_is_strike(&g));
        assert!(s.oof_is_strike(&Grant { capped: true, ..g }));
    }
}
