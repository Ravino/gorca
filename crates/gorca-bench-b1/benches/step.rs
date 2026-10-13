//! H-7 entry point: `cargo bench -p gorca-bench-b1 --bench step`.
//!
//! Writes `step.json` into `$B1_RUN_DIR` (or prints it). Subject selection:
//!
//! * `GORCA_STEP_SUBJECT=noop` (default until an arm's `Candidate` exists): the harness floor;
//! * `GORCA_STEP_SUBJECT=spin:<us>`: a deliberately slow policy (sensitivity check, must FAIL).
//!
//! WIRING (when PW-3 and an arm land): add an adapter here,
//!
//! ```ignore
//! struct CandidateSubject { policy: gorca_mirror::fill::Candidate, mirror: ..., ... }
//! impl StepSubject for CandidateSubject {
//!     fn prepare(&mut self, nodes: u32) { /* fill the mirror to `nodes` live nodes */ }
//!     fn step(&mut self, i: &StepInput) -> u64 { /* build FillInput from `i`, policy.step(..), return out.len() */ }
//! }
//! ```
//!
//! and select it with `GORCA_STEP_SUBJECT=candidate`. The harness (`gorca_bench_b1::step`) already
//! generates the MA-6 inputs and does the timing.

use gorca_bench_b1::step::{NoopSubject, SpinSubject, StepSubject, measure, scenarios};

fn main() {
    let which = std::env::var("GORCA_STEP_SUBJECT").unwrap_or_else(|_| "noop".into());
    let mut subject: Box<dyn StepSubject> = match which.split_once(':') {
        Some(("spin", us)) => Box::new(SpinSubject {
            us: us.parse().unwrap_or(400),
        }),
        _ => Box::new(NoopSubject),
    };
    let quick = std::env::args().any(|a| a == "--quick");
    let (rounds, iters) = if quick { (7, 20) } else { (7, 400) };
    let r = measure(subject.as_mut(), &scenarios(0xB1), rounds, iters, 20);
    let json = r.json();
    match std::env::var("B1_RUN_DIR") {
        Ok(d) => {
            if let Err(e) = std::fs::write(std::path::Path::new(&d).join("step.json"), &json) {
                eprintln!("step: cannot write step.json: {e}");
                std::process::exit(1);
            }
        }
        Err(_) => print!("{json}"),
    }
    eprintln!(
        "step[{}]: median over {rounds} rounds of the worst step = {:.1} us (gate 250 us)",
        r.subject,
        r.median_max_us()
    );
}
