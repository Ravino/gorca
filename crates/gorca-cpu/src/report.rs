// SPDX-License-Identifier: GPL-3.0-or-later
//! The start-up INFO lines (ADR-0027 section 4): one per missing or disabled feature, stating a
//! fact and the measured speed-up, never a warning.

use std::collections::BTreeMap;

use crate::disable::DisableList;
use crate::feature::{Feature, FeatureSet};
use crate::registry::{Choice, KernelInfo};
use crate::table::Active;

/// Measured speed-ups of each implementation over the portable function, from `hints.toml`
/// (written from the latest `cargo bench -p gorca-cpu` run on the reference machine). A kernel or
/// implementation with no entry has no hint, and the log clause is left out, never guessed.
const HINTS: &str = include_str!("../hints.toml");

/// Speed-up of `imp` of `kernel` over portable, if measured.
#[must_use]
pub fn hint(kernel: &str, imp: &str) -> Option<f64> {
    parse_hints(HINTS)
        .get(&(kernel.to_owned(), imp.to_owned()))
        .copied()
}

/// `kernel/impl = factor` lines; `#` starts a comment.
pub(crate) fn parse_hints(text: &str) -> BTreeMap<(String, String), f64> {
    let mut m = BTreeMap::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((key, val)) = line.split_once('=') else {
            continue;
        };
        let Some((k, i)) = key.trim().split_once('/') else {
            continue;
        };
        if let Ok(v) = val.trim().parse::<f64>() {
            m.insert((k.trim().to_owned(), i.trim().to_owned()), v);
        }
    }
    m
}

fn factor(x: f64) -> String {
    if x >= 10.0 {
        format!("~{x:.0}x")
    } else {
        let s = format!("{x:.1}");
        format!("~{}x", s.strip_suffix(".0").unwrap_or(&s))
    }
}

/// A kernel that would use a missing feature: id, whether it runs portable, measured hint.
type Wanted<'a> = (&'a str, bool, Option<f64>);

/// The lines `init` reports for the process.
#[must_use]
pub fn info_lines(a: &Active) -> Vec<String> {
    lines(
        a.detected,
        a.enabled.set(),
        &a.disabled,
        &crate::table::kernel_infos(),
        &a.choices,
        &hint,
    )
}

/// Pure line builder, testable with synthetic data.
#[must_use]
pub fn lines(
    detected: FeatureSet,
    enabled: FeatureSet,
    disabled: &DisableList,
    kernels: &[KernelInfo],
    choices: &[Choice],
    hint: &dyn Fn(&str, &str) -> Option<f64>,
) -> Vec<String> {
    let mut out = Vec::new();
    let pos_of = |k: &KernelInfo| -> usize {
        choices
            .iter()
            .find(|c| c.kernel == k.id)
            .and_then(|c| k.impls.iter().position(|(n, _)| *n == c.name))
            .unwrap_or(k.impls.len())
    };

    if disabled.all {
        let names: Vec<&str> = kernels
            .iter()
            .filter(|k| !k.impls.is_empty())
            .map(|k| k.id)
            .collect();
        if !names.is_empty() {
            out.push(format!(
                "cpu: all CPU extensions disabled by GORCA_CPU_DISABLE: {} run portable",
                names.join(", ")
            ));
        }
    } else {
        // Feature -> kernels that would use it: (kernel, chosen is portable, hint of the best impl).
        let mut by_feature: BTreeMap<Feature, Vec<Wanted<'_>>> = BTreeMap::new();
        for k in kernels {
            let pos = pos_of(k);
            for (name, requires) in &k.impls[..pos] {
                let Some(f) = requires.difference(enabled).iter().next() else {
                    continue;
                };
                let slot = by_feature.entry(f).or_default();
                if !slot.iter().any(|(id, _, _)| *id == k.id) {
                    slot.push((k.id, pos == k.impls.len(), hint(k.id, name)));
                }
            }
        }
        for (f, ks) in by_feature {
            let why = if detected.contains(f) {
                "disabled by GORCA_CPU_DISABLE"
            } else {
                "not available"
            };
            let names: Vec<&str> = ks.iter().map(|(id, _, _)| *id).collect();
            let run = if ks.iter().all(|(_, portable, _)| *portable) {
                "run portable"
            } else {
                "run a slower path"
            };
            let pron = if ks.len() == 1 {
                ("it", "")
            } else {
                ("they", "")
            };
            let known: Vec<(&str, f64)> = ks
                .iter()
                .filter_map(|(id, _, h)| h.map(|h| (*id, h)))
                .collect();
            let tail = match known.split_first() {
                None => String::new(),
                Some((first, rest))
                    if rest.iter().all(|(_, h)| factor(*h) == factor(first.1))
                        && known.len() == ks.len() =>
                {
                    format!(
                        "; with {} {} would be {} faster",
                        f.label(),
                        pron.0,
                        factor(first.1)
                    )
                }
                Some(_) => {
                    let each: Vec<String> = known
                        .iter()
                        .map(|(id, h)| format!("{id} {}", factor(*h)))
                        .collect();
                    format!(
                        "; with {} {} would be faster ({})",
                        f.label(),
                        pron.0,
                        each.join(", ")
                    )
                }
            };
            out.push(format!(
                "cpu: {} {why}: {} {run}{tail}",
                f.label(),
                names.join(", ")
            ));
        }
    }
    if !disabled.unknown.is_empty() {
        out.push(format!(
            "cpu: GORCA_CPU_DISABLE: unknown feature name(s) ignored: {}",
            disabled.unknown.join(", ")
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::PORTABLE;

    fn info(id: &'static str, impls: &[(&'static str, &[Feature])]) -> KernelInfo {
        KernelInfo {
            id,
            min_len: 0,
            impls: impls.iter().map(|(n, f)| (*n, FeatureSet::of(f))).collect(),
        }
    }

    fn choose(k: &KernelInfo, enabled: FeatureSet) -> Choice {
        for (n, r) in &k.impls {
            if r.is_subset_of(enabled) {
                return Choice {
                    kernel: k.id,
                    name: n,
                    requires: *r,
                };
            }
        }
        Choice {
            kernel: k.id,
            name: PORTABLE,
            requires: FeatureSet::EMPTY,
        }
    }

    fn run(
        detected: &[Feature],
        disable: &str,
        ks: &[KernelInfo],
        hint: &dyn Fn(&str, &str) -> Option<f64>,
    ) -> Vec<String> {
        let d = DisableList::parse(disable);
        let detected = FeatureSet::of(detected);
        let enabled = d.apply(detected);
        let choices: Vec<Choice> = ks.iter().map(|k| choose(k, enabled)).collect();
        lines(detected, enabled, &d, ks, &choices, hint)
    }

    fn kernels() -> Vec<KernelInfo> {
        vec![
            info("text-diff", &[("avx2", &[Feature::Avx2])]),
            info("utf16-offsets", &[("avx2", &[Feature::Avx2])]),
        ]
    }

    #[test]
    fn nothing_missing_no_line() {
        let l = run(&[Feature::Avx2], "", &kernels(), &|_, _| Some(3.0));
        assert!(l.is_empty(), "{l:?}");
    }

    #[test]
    fn one_missing_feature_one_line_with_aggregate_hint() {
        let l = run(&[], "", &kernels(), &|_, _| Some(3.0));
        assert_eq!(
            l,
            [
                "cpu: AVX2 not available: text-diff, utf16-offsets run portable; with AVX2 they would be ~3x faster"
            ]
        );
    }

    #[test]
    fn different_hints_are_listed_each() {
        let h = |k: &str, _: &str| Some(if k == "text-diff" { 3.0 } else { 5.5 });
        let l = run(&[], "", &kernels(), &h);
        assert_eq!(
            l,
            [
                "cpu: AVX2 not available: text-diff, utf16-offsets run portable; with AVX2 they would be faster (text-diff ~3x, utf16-offsets ~5.5x)"
            ]
        );
    }

    #[test]
    fn no_measurement_omits_the_clause() {
        let l = run(&[], "", &kernels(), &|_, _| None);
        assert_eq!(
            l,
            ["cpu: AVX2 not available: text-diff, utf16-offsets run portable"]
        );
    }

    #[test]
    fn disabled_by_list_says_so_and_singular_pronoun() {
        let ks = vec![info("text-diff", &[("avx2", &[Feature::Avx2])])];
        let l = run(&[Feature::Avx2], "avx2", &ks, &|_, _| Some(3.0));
        assert_eq!(
            l,
            [
                "cpu: AVX2 disabled by GORCA_CPU_DISABLE: text-diff run portable; with AVX2 it would be ~3x faster"
            ]
        );
    }

    #[test]
    fn all_collapses_to_one_line_and_unknown_names_are_reported() {
        let l = run(&[Feature::Avx2], "all,avx9", &kernels(), &|_, _| Some(3.0));
        assert_eq!(
            l,
            [
                "cpu: all CPU extensions disabled by GORCA_CPU_DISABLE: text-diff, utf16-offsets run portable",
                "cpu: GORCA_CPU_DISABLE: unknown feature name(s) ignored: avx9",
            ]
        );
    }

    #[test]
    fn partial_fallback_reports_the_missing_better_tier_only() {
        let ks = vec![info(
            "k",
            &[("avx2", &[Feature::Avx2]), ("sse42", &[Feature::Sse42])],
        )];
        let l = run(&[Feature::Sse42], "", &ks, &|_, i| {
            Some(if i == "avx2" { 4.0 } else { 2.0 })
        });
        assert_eq!(
            l,
            ["cpu: AVX2 not available: k run a slower path; with AVX2 it would be ~4x faster"]
        );
    }

    #[test]
    fn hints_file_parses_and_names_real_kernels() {
        let h = parse_hints(HINTS);
        let infos = crate::table::kernel_infos();
        for (k, i) in h.keys() {
            let info = infos
                .iter()
                .find(|x| x.id == k)
                .unwrap_or_else(|| panic!("hints.toml names unknown kernel {k}"));
            assert!(
                info.impls.iter().any(|(n, _)| n == i),
                "hints.toml names unknown impl {k}/{i}"
            );
        }
    }

    #[test]
    fn factor_formatting() {
        assert_eq!(factor(3.0), "~3x");
        assert_eq!(factor(2.46), "~2.5x");
        assert_eq!(factor(12.4), "~12x");
    }
}
