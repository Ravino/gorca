// SPDX-License-Identifier: GPL-3.0-or-later
//! Everything that needs `unsafe` or a `#[target_feature]` attribute lives here (ADR-0027 CD-9).

#[cfg(target_arch = "aarch64")]
pub(crate) mod aarch64;
#[cfg(target_arch = "x86_64")]
pub(crate) mod x86;

#[cfg(target_arch = "x86_64")]
pub(crate) mod common_prefix_x86;
#[cfg(target_arch = "x86_64")]
pub(crate) mod utf8_extend_x86;
#[cfg(target_arch = "x86_64")]
pub(crate) mod utf8_scalar_count_x86;
