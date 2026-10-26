// SPDX-License-Identifier: GPL-3.0-or-later
//! Text kernels with their length thresholds applied (ADR-0027 CD-2): below `min_len` the
//! portable function runs inline with no indirect call, above it the resolved implementation
//! runs through the table. Results are identical either way.

use crate::defs::{common_prefix, utf8_extend, utf8_scalar_count};
use crate::table::kernels;

/// Number of Unicode scalars in `s`; equal to `s.chars().count()`.
#[must_use]
pub fn utf8_scalar_count(s: &str) -> usize {
    if s.len() < utf8_scalar_count::SPEC.min_len {
        utf8_scalar_count::utf8_scalar_count_portable(s)
    } else {
        (kernels().utf8_scalar_count)(s)
    }
}

/// Length of the common prefix of two `char` slices.
#[must_use]
pub fn common_prefix_chars(a: &[char], b: &[char]) -> usize {
    if a.len().min(b.len()) < common_prefix::SPEC.min_len {
        common_prefix::common_prefix_portable(a, b)
    } else {
        (kernels().common_prefix_chars)(a, b)
    }
}

/// Append the scalars of `s` to `out`; equal to `out.extend(s.chars())`.
pub fn utf8_extend_chars(s: &str, out: &mut Vec<char>) {
    if s.len() < utf8_extend::SPEC.min_len {
        utf8_extend::utf8_extend_portable(s, out);
    } else {
        (kernels().utf8_extend_chars)(s, out);
    }
}

/// The scalars of `s` as a new vector; equal to `s.chars().collect()`.
#[must_use]
pub fn utf8_to_chars(s: &str) -> Vec<char> {
    // Exact capacity (the count kernel is cheaper than a regrow): one allocation, no slack.
    let mut v = Vec::with_capacity(utf8_scalar_count(s));
    utf8_extend_chars(s, &mut v);
    v
}
