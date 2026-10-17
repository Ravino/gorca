// SPDX-License-Identifier: GPL-3.0-or-later
//! `ext-vte`: terminal (VTE) policy extension.
//!
//! Responsibility (ADR-0001 sections 5.6, 10 and 11, ADR-0008 tier W3): supplies the T-2 policy
//! for the terminal presenter: prompt detection, what counts as "new output" against a repaint,
//! typed-character suppression, cursor-line reading after a history recall, password prompts,
//! and which key is which in a shell (readline and emacs bindings).
//!
//! > "T-2 Diff the screen ourselves ... `ext-vte` supplies policy."
//!
//! The policy is the pure state machine [`Session`], driven through the extension-shaped
//! interface `gorca_ext_api::terminal::TerminalHooks`: plain data in (keys, edits, screens, time),
//! plain actions out (speak, fetch the screen, arm a timer). The host owns the IPC, the clock and
//! the speech. One source, two builds (ADR-0008 amendment 1): a `wasm32-wasip2` component (the
//! default; the core talks to it through the `terminal` WIT interface) and a native `rlib` (the
//! exclusive fallback the daemon links in with `--terminal-policy=native`).
//!
//! Allowed dependencies: `gorca-ext-api` only among workspace crates (section 11, rule 5).

mod keys;
mod prompt;
mod screen;
mod session;

use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot, TerminalHooks,
};
use gorca_ext_api::{Hooks, export_extension};

pub use gorca_ext_api::terminal::unit_at;
pub use keys::classify as classify_key;
pub use prompt::Prompt;
pub use screen::{Diff, DiffBuf, Screen, diff, diff_with};
pub use session::Session;

/// The terminal extension: the generic hooks defer to the core, the terminal interface is the
/// [`Session`].
#[derive(Debug)]
pub struct VteExtension {
    session: Session,
}

impl VteExtension {
    pub fn new(cfg: &TermConfig) -> Self {
        Self {
            session: Session::new(cfg),
        }
    }
}

impl Default for VteExtension {
    fn default() -> Self {
        Self::new(&TermConfig::default())
    }
}

impl TerminalHooks for VteExtension {
    fn configure(&mut self, cfg: &TermConfig) {
        self.session.configure(cfg);
    }

    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        self.session.step(now, input, out);
    }

    fn counters(&self) -> TermCounters {
        self.session.counters()
    }

    fn snapshot(&self) -> Option<TermSnapshot> {
        self.session.snapshot()
    }
}

impl Hooks for VteExtension {
    fn terminal(&mut self) -> Option<&mut dyn TerminalHooks> {
        Some(self)
    }
}

export_extension!(VteExtension);

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_ext_api::{CommandResult, Event, EventVerdict};

    #[test]
    fn the_generic_hooks_defer_to_the_core() {
        let mut e = VteExtension::default();
        assert_eq!(
            e.on_event(&Event::Announcement("x".into())),
            EventVerdict::Pass
        );
        assert_eq!(e.command(1), CommandResult::NotHandled);
        assert!(e.nav_units().is_empty());
        assert!(e.terminal().is_some());
    }

    /// The core reads the screen through `gorca_ext_api::terminal::Rows`; the extension keeps it
    /// as a [`Screen`]. Both must agree on every row and every caret position.
    #[test]
    fn screen_and_rows_agree() {
        use gorca_ext_api::terminal::Rows;
        let samples = [
            "",
            "\n",
            "a",
            "a\n",
            "ab\ncd",
            "ab\ncd\n",
            "\n\n",
            "\u{43f}\u{440}\u{438}\u{432}\u{435}\u{442}\n$ ls\n",
            "x\n\ny\n\n",
        ];
        for s in samples {
            let a = Screen::new(s);
            let b = Rows::new(s);
            assert_eq!(a.line_count(), b.line_count(), "{s:?}");
            for i in 0..=a.line_count() {
                assert_eq!(a.line(i), b.line(i), "{s:?} row {i}");
            }
            for off in 0..a.char_len() + 3 {
                assert_eq!(a.locate(off), b.locate(off), "{s:?} off {off}");
            }
        }
    }
}
