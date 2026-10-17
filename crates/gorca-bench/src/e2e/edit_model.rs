//! The editing model's expectations per matrix step (placeholder while probing).

use super::edit::Target;

pub fn check(
    _t: Target,
    _chromium: bool,
    _step: &str,
    _got: &[String],
) -> Option<Result<(), String>> {
    None
}
