// SPDX-License-Identifier: GPL-3.0-or-later
//! The resolved kernel table, `init`, and the active-feature report (ADR-0027 CD-1).

use std::fmt::Write as _;
use std::sync::{Once, OnceLock};

use crate::defs;
use crate::detect::{CpuInfo, cpu_info, detect};
use crate::disable::DisableList;
use crate::feature::{Feature, FeatureSet};
use crate::registry::{Choice, KernelInfo};
use crate::report;

/// Declare the kernels of the table: a field per kernel, resolved from its [`crate::KernelSpec`].
macro_rules! declare_kernels {
    ($( $(#[$m:meta])* $field:ident : $fty:ty = $spec:path ),+ $(,)?) => {
        /// Every dispatched kernel as a plain function pointer, resolved once. Copy the pointers
        /// you need into your own struct at construction; the hot path then has no table access.
        #[derive(Clone, Copy, Debug)]
        pub struct Kernels {
            $( $(#[$m])* pub $field: $fty ),+
        }

        impl Kernels {
            /// Resolve every kernel against `enabled`.
            #[must_use]
            pub fn resolve(enabled: FeatureSet) -> (Kernels, Vec<Choice>) {
                let mut choices = Vec::new();
                $(
                    let ($field, c) = $spec.resolve(enabled);
                    choices.push(c);
                )+
                (Kernels { $($field),+ }, choices)
            }
        }

        /// The registry in type-erased form: ids, implementations, thresholds.
        #[must_use]
        pub fn kernel_infos() -> Vec<KernelInfo> {
            vec![ $( $spec.info() ),+ ]
        }
    };
}

declare_kernels! {
    /// Number of Unicode scalars in a UTF-8 string (`str::chars().count()`).
    utf8_scalar_count: fn(&str) -> usize = defs::utf8_scalar_count::SPEC,
    /// Length of the common prefix of two `char` slices.
    common_prefix_chars: fn(&[char], &[char]) -> usize = defs::common_prefix::SPEC,
    /// Append the scalars of a UTF-8 string to a `Vec<char>`.
    utf8_extend_chars: fn(&str, &mut Vec<char>) = defs::utf8_extend::SPEC,
}

/// Why the table was built.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Source {
    /// `init()` ran before any kernel was used (what the daemon does).
    Init,
    /// A kernel or `active()` was used first and the table resolved lazily from the environment.
    Lazy,
}

/// The resolved state of the process: what the hardware has, what was disabled, which
/// implementation each kernel runs.
#[derive(Debug)]
pub struct Active {
    pub detected: FeatureSet,
    pub disabled: DisableList,
    /// `detected` minus `disabled`: what the resolver may use.
    pub enabled: FeatureSet,
    pub info: CpuInfo,
    pub source: Source,
    pub kernels: Kernels,
    pub choices: Vec<Choice>,
}

impl Active {
    fn build(disable: &DisableList, source: Source) -> Active {
        let detected = detect();
        let enabled = disable.apply(detected);
        let (kernels, choices) = Kernels::resolve(enabled);
        Active {
            detected,
            disabled: disable.clone(),
            enabled,
            info: cpu_info(),
            source,
            kernels,
            choices,
        }
    }

    /// Every kernel runs its portable function.
    #[must_use]
    pub fn all_portable(&self) -> bool {
        self.choices.iter().all(Choice::is_portable)
    }

    /// The implementation chosen for kernel `id`.
    #[must_use]
    pub fn choice(&self, id: &str) -> Option<&Choice> {
        self.choices.iter().find(|c| c.kernel == id)
    }

    /// `GORCA_CPU_DISABLE=all` is in force: the plugin engine compiles for the baseline too
    /// (ADR-0027 CD-W4). A partial list changes the Rust kernels only.
    #[must_use]
    pub fn portable_requested(&self) -> bool {
        self.disabled.all
    }

    /// The multi-line diagnostic dump (`gorca --version --verbose`, crash reports).
    #[must_use]
    pub fn describe(&self) -> String {
        let mut s = String::new();
        let i = &self.info;
        if i.brand.is_empty() {
            let _ = writeln!(s, "cpu: {}", std::env::consts::ARCH);
        } else {
            let _ = writeln!(
                s,
                "cpu: {} ({}, family {} model {} stepping {})",
                i.brand, i.vendor, i.family, i.model, i.stepping
            );
        }
        let list = |set: FeatureSet| {
            let v: Vec<&str> = set.iter().map(Feature::name).collect();
            if v.is_empty() {
                "-".to_owned()
            } else {
                v.join(" ")
            }
        };
        let _ = writeln!(s, "features detected: {}", list(self.detected));
        let _ = writeln!(s, "disabled by list: {}", self.disabled.describe());
        let _ = writeln!(s, "features in use: {}", list(self.enabled));
        let _ = writeln!(
            s,
            "table resolved by: {}",
            match self.source {
                Source::Init => "init",
                Source::Lazy => "lazy (init was late or never called)",
            }
        );
        for c in &self.choices {
            let _ = writeln!(s, "kernel {}: {}", c.kernel, c.name);
        }
        s
    }
}

/// `init` was given a list but the table is already resolved with another.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InitError {
    pub requested: String,
    pub active: String,
}

impl std::fmt::Display for InitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "cpu table already resolved with disable list [{}]; requested [{}] ignored",
            self.active, self.requested
        )
    }
}

impl std::error::Error for InitError {}

static ACTIVE: OnceLock<Active> = OnceLock::new();
static LOGGED: Once = Once::new();

/// Detect the CPU once and resolve every kernel. **The first statement of `main`.**
///
/// `disable` is the already-merged `GORCA_CPU_DISABLE` / `--cpu-disable` text (library code never
/// reads the environment, ADR-0001 rule 8; the lazy path below is the one exception, for tests).
/// Logs nothing about the CPU: the logger is not up yet, so call [`log_report`] afterwards.
///
/// Idempotent: a second call with the same list returns the same table. A different list after
/// the table resolved is an [`InitError`] and one INFO line; the table is unchanged.
///
/// # Errors
/// [`InitError`] when the table was already resolved with a different disable list.
pub fn init(disable: Option<&str>) -> Result<&'static Active, InitError> {
    let list = DisableList::parse(disable.unwrap_or(""));
    let a = ACTIVE.get_or_init(|| Active::build(&list, Source::Init));
    if a.disabled == list {
        return Ok(a);
    }
    let err = InitError {
        requested: list.describe(),
        active: a.disabled.describe(),
    };
    tracing::info!(target: "gorca::cpu", "cpu: {err}");
    Err(err)
}

/// The resolved report. Resolves lazily from `GORCA_CPU_DISABLE` if [`init`] has not run (tests
/// and tools); the lazy path is visible in [`Active::source`].
pub fn active() -> &'static Active {
    ACTIVE.get_or_init(|| {
        // The one environment read of the crate: so `GORCA_CPU_DISABLE=all cargo test` reaches
        // every crate that uses a kernel without wiring (exempt in xtask envrule).
        let env = std::env::var("GORCA_CPU_DISABLE").unwrap_or_default();
        Active::build(&DisableList::parse(&env), Source::Lazy)
    })
}

/// The kernel table (see [`active`]).
pub fn kernels() -> &'static Kernels {
    &active().kernels
}

/// Emit the start-up INFO lines (one per missing or disabled feature, ADR-0027 section 4) and a
/// DEBUG line per kernel. Once per process; later calls do nothing.
pub fn log_report() {
    LOGGED.call_once(|| {
        let a = active();
        for line in report::info_lines(a) {
            tracing::info!(target: "gorca::cpu", "{line}");
        }
        for c in &a.choices {
            tracing::debug!(target: "gorca::cpu", "cpu: kernel {} runs {}", c.kernel, c.name);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn describe_lists_every_kernel() {
        let a = active();
        let text = a.describe();
        for k in kernel_infos() {
            assert!(text.contains(&format!("kernel {}:", k.id)), "{text}");
        }
        assert!(text.contains("features in use:"));
    }

    /// CD-T5: over all 2^k subsets of the features a kernel uses, the resolver never installs
    /// an implementation whose features are missing, falls back to portable only when nothing
    /// fits, and every registered implementation is reachable.
    #[test]
    fn resolver_over_all_feature_masks() {
        for info in kernel_infos() {
            let used: Vec<Feature> = info
                .impls
                .iter()
                .fold(FeatureSet::EMPTY, |a, (_, r)| a.union(*r))
                .iter()
                .collect();
            assert!(
                used.len() <= 8,
                "{}: too many features for exhaustive masks",
                info.id
            );
            let mut reached = std::collections::BTreeSet::new();
            for m in 0u32..(1 << used.len()) {
                let mask: FeatureSet = used
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| m & (1 << i) != 0)
                    .map(|(_, f)| *f)
                    .collect();
                let (_, choices) = Kernels::resolve(mask);
                let c = choices.iter().find(|c| c.kernel == info.id).unwrap();
                assert!(
                    c.requires.is_subset_of(mask),
                    "{} installed {} on {mask:?}",
                    info.id,
                    c.name
                );
                let first_fit = info.impls.iter().find(|(_, r)| r.is_subset_of(mask));
                match first_fit {
                    Some((n, _)) => assert_eq!(c.name, *n),
                    None => assert!(c.is_portable(), "{} not portable on {mask:?}", info.id),
                }
                reached.insert(c.name);
            }
            for (n, _) in &info.impls {
                assert!(
                    reached.contains(n),
                    "{}: impl {n} is never selected (shadowed)",
                    info.id
                );
            }
            assert!(reached.contains(crate::PORTABLE));
        }
    }

    #[test]
    fn disabling_everything_gives_portable_choices() {
        let (_, choices) = Kernels::resolve(DisableList::parse("all").apply(detect()));
        assert!(choices.iter().all(Choice::is_portable));
    }

    #[test]
    fn choices_match_infos() {
        let infos = kernel_infos();
        let a = active();
        assert_eq!(a.choices.len(), infos.len());
        for (c, i) in a.choices.iter().zip(&infos) {
            assert_eq!(c.kernel, i.id);
        }
    }
}
