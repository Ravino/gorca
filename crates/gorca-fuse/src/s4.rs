// SPDX-License-Identifier: GPL-3.0-or-later
//! Stratum S4 (inferred) and the S3 `title` evidence pick (ADR-0024 section 2.2).
//!
//! Ranking happens once, in the side table ([`crate::side`]), by a total order on the class after
//! the tier cap, the static kind order, the stable source name, direct fact before rule output,
//! value, anchor and seq: never by `SourceId` (slots are recycled, so their order depends on
//! allocation order) and never by raw `p_milli` (FS-4). The fill rule (A6) decides whether the top
//! candidate may be written: S3 and S4 fill only an `Unknown`, observed-empty or default value.

use crate::side::{Entry, TrailStatus};

/// Trail status of one Inferred entry. `won` tells whether an earlier entry of the group won.
pub fn inferred_status(e: &Entry, can_fill: bool, won: bool) -> TrailStatus {
    match e.trail().status {
        TrailStatus::Ok if can_fill && !won => TrailStatus::Won,
        TrailStatus::Ok => TrailStatus::Shadowed,
        other => other,
    }
}

/// Trail status of one `title` entry (`title_ok`: the name is fillable and the role is named
/// from a title).
pub fn title_status(e: &Entry, title_ok: bool, won: bool) -> TrailStatus {
    let empty = e.cand().is_some_and(|c| c.write().is_empty_like());
    match e.trail().status {
        TrailStatus::Ok if empty => TrailStatus::NoValue,
        TrailStatus::Ok if title_ok && !won => TrailStatus::Won,
        TrailStatus::Ok if !won => TrailStatus::NotApplicable,
        TrailStatus::Ok => TrailStatus::Shadowed,
        other => other,
    }
}
