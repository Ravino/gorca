//! The bake-off hook (B-1 SPEC 10): `gorca_mirror::fill::Candidate`.
//!
//! On `main` this is a stand-in that behaves exactly like R0 ([`super::NullPolicy`]), so a build
//! with `--bench-fill-policy=candidate` must reproduce R0's numbers: the harness checks itself
//! with it. An arm **replaces this file** (and `candidate/**`) with its policy and touches nothing
//! else for wiring. The daemon (`--bench-fill-policy=candidate`) and the step bench
//! (`crates/gorca-bench-b1/benches/step.rs`, `GORCA_STEP_SUBJECT=candidate`) both use it through
//! the three things below, and only those:
//!
//! * `Candidate: FillPolicy` (the trait of SPEC 4.4);
//! * `Candidate: Default` (one fresh policy per application);
//! * `Candidate: Send + 'static`.

use super::{FillCtx, FillEffects, FillInput, FillPolicy, NullPolicy};

/// Stand-in: R0 under another name.
#[derive(Debug, Default)]
pub struct Candidate(NullPolicy);

impl FillPolicy for Candidate {
    fn name(&self) -> &'static str {
        "candidate-standin"
    }

    fn step(&mut self, cx: &FillCtx<'_>, input: FillInput<'_>, out: &mut FillEffects) {
        self.0.step(cx, input, out);
    }

    fn memory_bytes(&self) -> usize {
        self.0.memory_bytes()
    }
}
