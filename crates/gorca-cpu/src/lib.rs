// SPDX-License-Identifier: GPL-3.0-or-later
//! Runtime CPU feature dispatch (ADR-0027).
//!
//! The binary is built for the baseline (x86-64-v1, aarch64 with NEON). This crate detects the
//! CPU **once** ([`init`]), resolves every kernel to a function pointer in a `'static` table
//! ([`Kernels`]) and logs one INFO line for each feature a kernel could use but the machine
//! lacks (or the operator disabled with `GORCA_CPU_DISABLE`). Every kernel has a safe portable
//! function that is the specification: it is what runs on a CPU without the extension, and the
//! reference of every equivalence test.
//!
//! # Using it
//! - Daemon: call [`init`] as the first statement of `main`, then [`log_report`] once the logger
//!   is up. Report with [`active`]`().describe()`.
//! - Library code: call the wrappers in [`text`] (they take the portable path inline below the
//!   kernel's `min_len` and otherwise make one indirect call), or copy the pointers out of
//!   [`kernels`] into your own struct.
//! - Adding a kernel: write `kernels/<name>.rs` (portable fn, `SPEC`, tests) and
//!   `arch/<name>_x86.rs` (`kernel_impl!`), add one line to `declare_kernels!` in `table.rs`
//!   and a bench group. The 2^k resolver test and the equivalence tests pick it up.
//!
//! `unsafe` exists only in `arch/` (CD-9).

mod cap;
pub mod detect;
pub mod disable;
pub mod feature;
pub mod registry;
pub mod report;
mod table;

mod arch;
mod defs;
pub mod text;

pub use detect::{CpuInfo, RawIsa, cpu_info, detect, raw_isa};
pub use disable::DisableList;
pub use cap::Cap;
pub use feature::{Arch, Feature, FeatureMarker, FeatureSet, marker, same_name};
pub use registry::{Choice, Impl, KernelInfo, KernelSpec, PORTABLE};
pub use table::{
    Active, InitError, Kernels, Source, active, init, kernel_infos, kernels, log_report,
};

/// Test and bench access to every kernel's specification, portable function included.
#[doc(hidden)]
pub mod specs {
    pub use crate::defs::common_prefix::{SPEC as COMMON_PREFIX, common_prefix_portable};
    pub use crate::defs::utf8_extend::{SPEC as UTF8_EXTEND, utf8_extend_portable};
    pub use crate::defs::utf8_scalar_count::{
        SPEC as UTF8_SCALAR_COUNT, utf8_scalar_count_portable,
    };
}
