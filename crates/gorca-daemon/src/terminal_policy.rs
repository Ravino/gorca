//! The native build of `ext-vte` behind the core's [`TermProvider`] seam (ADR-0008 amendment 1:
//! the component is the default, the same source built natively is the exclusive fallback).
//!
//! The extension runs on the Reader, so a panic must not reach it: every call is caught. After a
//! panic the policy state is unknown, so the session starts over (a fresh extension, re-baselined
//! by a `focus`), and three panics switch the native policy off for the rest of the run (the
//! Reader then tracks no terminal screen and the generic key echo speaks). A native call cannot
//! be pre-empted, unlike the component's fuel limit; that is the price of the fallback.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ext_vte::VteExtension;
use gorca_core::{TermProvider, TermSession};
use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot, TerminalHooks,
};

/// Panics after which the native policy is off for the rest of the run.
const DISABLE_AFTER: u32 = 3;

#[derive(Debug, Default)]
pub struct NativeTerminal {
    panics: Arc<AtomicU32>,
}

impl TermProvider for NativeTerminal {
    fn open(&self, cfg: &TermConfig) -> Box<dyn TermSession> {
        Box::new(Session {
            ext: VteExtension::new(cfg),
            cfg: cfg.clone(),
            lost: false,
            panics: Arc::clone(&self.panics),
        })
    }
}

#[derive(Debug)]
struct Session {
    ext: VteExtension,
    cfg: TermConfig,
    /// A panic reset the extension: the next input starts with a `focus`.
    lost: bool,
    panics: Arc<AtomicU32>,
}

impl Session {
    fn off(&self) -> bool {
        self.panics.load(Ordering::Relaxed) >= DISABLE_AFTER
    }

    fn guarded(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) -> bool {
        let ext = &mut self.ext;
        let mut local = Vec::new();
        let r = catch_unwind(AssertUnwindSafe(|| ext.step(now, input, &mut local)));
        if r.is_ok() {
            out.append(&mut local);
            return true;
        }
        let n = self.panics.fetch_add(1, Ordering::Relaxed) + 1;
        tracing::error!(
            panics = n,
            "the native terminal policy panicked; it starts over{}",
            if n >= DISABLE_AFTER {
                " and is now off"
            } else {
                ""
            }
        );
        self.ext = VteExtension::new(&self.cfg);
        self.lost = true;
        false
    }
}

impl TermSession for Session {
    fn alive(&self) -> bool {
        !self.lost && !self.off()
    }

    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        if self.off() {
            return;
        }
        if self.lost {
            self.lost = false;
            if matches!(input, TermInput::Blur) {
                return;
            }
            if !matches!(input, TermInput::Focus) && !self.guarded(now, TermInput::Focus, out) {
                return;
            }
        }
        self.guarded(now, input, out);
    }

    fn counters(&self) -> TermCounters {
        self.ext.counters()
    }

    fn snapshot(&self) -> Option<TermSnapshot> {
        if self.off() || self.lost {
            return None;
        }
        self.ext.snapshot()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_ext_api::terminal::RawKey;

    fn key(c: char) -> TermInput {
        TermInput::Raw(RawKey {
            keysym: c as u32,
            unichar: c as u32,
            phys: 0,
            shift: false,
            ctrl: false,
            alt: false,
            logo: false,
        })
    }

    #[test]
    fn a_session_reads_the_screen_on_focus_and_claims_a_typed_key() {
        let p = NativeTerminal::default();
        let mut s = p.open(&TermConfig::default());
        let mut out = Vec::new();
        s.step(0, TermInput::Focus, &mut out);
        assert!(out.contains(&TermAction::FetchScreen), "{out:?}");
        out.clear();
        s.step(
            1,
            TermInput::Screen {
                text: "me@box:~$ ".to_owned(),
                caret: Some(10),
            },
            &mut out,
        );
        out.clear();
        s.step(5, key('a'), &mut out);
        assert!(out.contains(&TermAction::ClaimKey), "{out:?}");
        assert!(s.snapshot().is_some());
    }
}
