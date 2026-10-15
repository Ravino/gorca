//! `ext-vte`: terminal (VTE) policy extension.
//!
//! Responsibility (ADR-0001 sections 5.6, 10 and 11, ADR-0008 tier W3): supplies the T-2 policy
//! for the terminal presenter: prompt detection, what counts as "new output" against a repaint,
//! typed-character suppression, cursor-line reading after a history recall, password prompts.
//!
//! > "T-2 Diff the screen ourselves ... `ext-vte` supplies policy."
//!
//! The policy is the pure state machine [`Session`], driven through the extension-shaped
//! interface `gorca_ext_api::terminal::TerminalHooks`: plain data in (keys, edits, screens, time),
//! plain actions out (speak, fetch the screen, arm a timer). The host owns the IPC, the clock and
//! the speech, so the same crate runs linked into the core today and as a `wasm32-wasip2`
//! component once the extension host (M5) dispatches the `terminal` interface.
//!
//! Allowed dependencies: `gorca-ext-api` only among workspace crates (section 11, rule 5).

mod prompt;
mod screen;
mod session;

use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermInput, TermSnapshot, TerminalHooks,
};
use gorca_ext_api::{
    CommandResult, Event, EventVerdict, Hooks, NavUnit, Node, PresentCtx, Segment,
};

pub use prompt::Prompt;
pub use screen::{Diff, DiffBuf, Screen, diff, diff_with};
pub use session::{Session, unit_at};

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
    fn on_event(&mut self, _ev: &Event) -> EventVerdict {
        EventVerdict::Pass
    }

    fn present(&mut self, _n: &Node, _ctx: PresentCtx) -> Option<Vec<Segment>> {
        None
    }

    fn nav_units(&self) -> Vec<NavUnit> {
        Vec::new()
    }

    fn nav_match(&self, _unit: u32, _n: &Node) -> bool {
        false
    }

    fn command(&mut self, _id: u32) -> CommandResult {
        CommandResult::NotHandled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_generic_hooks_defer_to_the_core() {
        let mut e = VteExtension::default();
        assert_eq!(
            e.on_event(&Event::Announcement("x".into())),
            EventVerdict::Pass
        );
        assert_eq!(e.command(1), CommandResult::NotHandled);
        assert_eq!(e.nav_units(), Vec::new());
    }
}
