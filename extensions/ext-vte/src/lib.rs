//! `ext-vte`: terminal (VTE) policy extension skeleton.
//!
//! Responsibility (ADR-0001 sections 5.6, 10 and 11): supplies the T-2 policy for the terminal
//! presenter: prompt detection, what counts as "new output", and review integration. The line-hash
//! screen diff itself runs in the app actor on `cpu`, not here.
//!
//! > "T-2 Diff the screen ourselves ... `ext-vte` supplies policy."
//!
//! Matches terminals by toolkit/app name through its manifest (declared by the host once the
//! manifest format exists). Phase 0: it passes every event and presents nothing, so the core
//! falls back to its default presentation.
//!
//! Allowed dependencies: **only** `gorca-ext-api` (section 11, rule 5). It is compiled for
//! `wasm32-wasip2` as a component once the toolchain pipeline exists (the target is installed).

use gorca_ext_api::{
    CommandResult, Event, EventVerdict, Hooks, NavUnit, Node, PresentCtx, Segment,
};

/// Terminal policy. Stateless for now; will hold the previous-screen line hashes' metadata.
#[derive(Debug, Default)]
pub struct VteExtension;

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
    fn skeleton_defers_to_the_core() {
        let mut e = VteExtension;
        assert_eq!(
            e.on_event(&Event::Announcement("x".into())),
            EventVerdict::Pass
        );
        assert_eq!(e.command(1), CommandResult::NotHandled);
        assert_eq!(e.nav_units(), Vec::new());
    }
}
