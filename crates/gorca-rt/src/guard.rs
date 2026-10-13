//! Test-isolation tripwire (review F6).
//!
//! A test process that reaches the user's real desktop (their session bus, a11y bus, voicego or
//! speech-dispatcher) is a bug that must be loud, not a convention. The test support crate
//! calls [`forbid`] with the *ambient* addresses and paths it finds in the environment; every
//! connect path of the workspace then calls [`check`] with its target. In debug builds a target
//! that contains a forbidden string panics at once. In release builds [`check`] compiles to
//! nothing, and without any registration it never fires, so the daemon is unaffected.

use std::sync::{Mutex, OnceLock, PoisonError};

fn forbidden() -> &'static Mutex<Vec<String>> {
    static FORBIDDEN: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    FORBIDDEN.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a string (a bus address, a socket path) that no connection of this process may
/// target. Empty strings are ignored.
pub fn forbid(what: impl Into<String>) {
    let what = what.into();
    if what.is_empty() {
        return;
    }
    let mut g = forbidden().lock().unwrap_or_else(PoisonError::into_inner);
    if !g.contains(&what) {
        g.push(what);
    }
}

/// The first forbidden string `target` contains, if any.
pub fn violation(target: &str) -> Option<String> {
    let g = forbidden().lock().unwrap_or_else(PoisonError::into_inner);
    g.iter().find(|f| target.contains(f.as_str())).cloned()
}

/// Panic (debug builds only) if `target` is one of the forbidden ambient addresses or paths.
#[inline]
pub fn check(target: &str) {
    #[cfg(debug_assertions)]
    if let Some(f) = violation(target) {
        // A deliberate tripwire: reaching the real desktop from a test must abort the test.
        #[allow(clippy::panic)]
        {
            panic!("test isolation tripwire: connecting to the ambient `{f}` (target `{target}`)");
        }
    }
    #[cfg(not(debug_assertions))]
    let _ = target;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_forbidden_target_is_caught_and_others_pass() {
        forbid("/run/user/424242/bus-tripwire-test");
        forbid("");
        assert!(violation("unix:path=/tmp/gorca-h.abc/run/bus").is_none());
        assert_eq!(
            violation("unix:path=/run/user/424242/bus-tripwire-test,guid=1").as_deref(),
            Some("/run/user/424242/bus-tripwire-test")
        );
        check("unix:path=/tmp/gorca-h.abc/run/bus");
    }

    #[cfg(debug_assertions)]
    #[test]
    #[should_panic(expected = "test isolation tripwire")]
    fn check_panics_on_an_ambient_address() {
        forbid("/run/user/424243/at-spi/bus-tripwire-test");
        check("unix:path=/run/user/424243/at-spi/bus-tripwire-test");
    }
}
