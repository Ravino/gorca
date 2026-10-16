//! Speech accounting for the strict key gates: every utterance the reader submitted must end
//! up spoken at the server or in a named drop counter.
//!
//! Why: a key series scores a chord by "the first SPEAK between its key and the next key". Under
//! host load two different things then look like a lost key although the reader dropped nothing:
//!
//! - the answer reached the mock server **after** the next key was already emitted (the harness
//!   thread or the reader's lane thread was descheduled): a *late* answer, not a lost one;
//! - the reader's speech lane holds one interrupt slot, and a newer command's answer overwrote
//!   the previous one before the lane thread picked it up (`superseded`, E-2, latest wins). The
//!   newer answer is spoken at once, so the user hears exactly what a cancel would have given.
//!
//! The reader prints its counters when it shuts down (`core stats=...`, `speech lane stats=...`).
//! [`Account`] parses them and checks conservation: submitted = spoken + counted drops, and
//! spoken = what the mock server received. Only a violation is a *silent* loss.

use std::path::Path;

use super::apps::Reader;
use super::mock::{Kind, MockServer};

/// Counters of one reader run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Account {
    /// Utterances the core handed to the speech lane.
    pub submitted: u64,
    /// Utterances the lane sent to the server.
    pub spoken: u64,
    /// Interrupts overwritten in the slot by a newer one (latest wins).
    pub superseded: u64,
    /// Dropped because a newer event made them stale.
    pub stale_dropped: u64,
    pub inbox_dropped: u64,
    pub muted_dropped: u64,
    pub submit_failed: u64,
    /// SPEAK verbs the mock server logged over the whole run.
    pub received: u64,
}

impl Account {
    /// Utterances the reader dropped on purpose and counted.
    pub fn counted_drops(&self) -> u64 {
        self.superseded
            + self.stale_dropped
            + self.inbox_dropped
            + self.muted_dropped
            + self.submit_failed
    }

    /// Utterances that vanished without a counter: submitted but neither spoken nor counted,
    /// plus sent by the lane but never received by the server.
    pub fn silent_loss(&self) -> u64 {
        let inside = self.submitted.abs_diff(self.spoken + self.counted_drops());
        inside + self.spoken.saturating_sub(self.received)
    }
}

/// The value after `name: ` in a Debug-printed struct, within the last line after `anchor`.
fn field(text: &str, anchor: &str, name: &str) -> Option<u64> {
    let tail = &text[text.rfind(anchor)? + anchor.len()..];
    let line = tail.lines().next()?;
    let pat = format!(" {name}: ");
    let at = line.find(&pat)? + pat.len();
    line[at..]
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()
}

/// Parse the reader's shutdown log; `None` when it has no speech counters (stub reader, or a
/// reader killed before its shutdown path ran).
pub fn parse(log: &str, received: u64) -> Option<Account> {
    let lane = "speech lane stats=SpeechStats {";
    let core = "core stats=CoreStatsSnapshot {";
    Some(Account {
        submitted: field(log, core, "utterances")?,
        spoken: field(log, lane, "spoken")?,
        superseded: field(log, lane, "superseded")?,
        stale_dropped: field(log, lane, "stale_dropped")?,
        inbox_dropped: field(log, lane, "inbox_dropped")?,
        muted_dropped: field(log, lane, "muted_dropped")?,
        submit_failed: field(log, lane, "submit_failed")?,
        received,
    })
}

/// Stop the reader (its shutdown path prints the counters) and account for its speech. Call
/// after the series has been scored: the reader is gone afterwards.
pub fn retire(reader: &mut Reader, log: Option<&Path>, mock: &MockServer) -> Option<Account> {
    let log = log?;
    if !reader.stop(std::time::Duration::from_secs(8)) {
        return None;
    }
    // The server logs a SPEAK when its connection thread reads it; give the last ones a moment.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let received = mock
        .since(0)
        .iter()
        .filter(|r| matches!(r.kind, Kind::Speak { .. }))
        .count() as u64;
    parse(&std::fs::read_to_string(log).ok()?, received)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "INFO gorca_daemon: core stats=CoreStatsSnapshot { apps_announced: 1, focus_requests: 40, fetch_ok: 61, fetch_timeout: 0, fetch_failed: 0, fetch_refused: 0, stale_dropped: 29, utterances: 1231, reader_restarts: 0 }\n\
INFO gorca_daemon: speech lane stats=SpeechStats { spoken: 1230, replaced: 70, stale_dropped: 0, replace_slow: 0, superseded: 1, inbox_dropped: 0, muted_dropped: 0, submit_failed: 0 }\n";

    #[test]
    fn a_superseded_utterance_is_accounted_for() {
        let a = parse(LOG, 1230).expect("counters");
        assert_eq!((a.submitted, a.spoken, a.superseded), (1231, 1230, 1));
        assert_eq!(a.silent_loss(), 0);
    }

    #[test]
    fn an_uncounted_disappearance_is_a_silent_loss() {
        // One utterance neither spoken nor counted.
        let a = parse(&LOG.replace("superseded: 1", "superseded: 0"), 1230).expect("counters");
        assert_eq!(a.silent_loss(), 1);
        // The lane says it sent 1230 but the server saw 1229.
        let b = parse(LOG, 1229).expect("counters");
        assert_eq!(b.silent_loss(), 1);
    }

    #[test]
    fn no_counters_no_account() {
        assert!(parse("nothing here", 5).is_none());
    }
}
