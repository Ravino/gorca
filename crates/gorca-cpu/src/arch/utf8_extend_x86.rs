// SPDX-License-Identifier: GPL-3.0-or-later
//! `utf8-extend-chars` on x86-64: the shared safe body compiled with AVX2 enabled (tier 2).
#![allow(unsafe_code)]

use crate::defs::utf8_extend::extend_body;
use crate::kernel_impl;

kernel_impl! {
    /// ASCII runs of 32 bytes are checked and widened to `u32` lanes (`vpmovzxbd`).
    pub mod avx2 ["avx2" => Avx2] (s: &str, out: &mut Vec<char>) -> () {
        extend_body(s, out);
    }
}
