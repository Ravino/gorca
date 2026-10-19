// SPDX-License-Identifier: GPL-3.0-or-later
//! The pure availability machine of ADR-0025 section 2.5. No I/O: events in, effects out, so every
//! row of the table is a unit test.

use std::sync::Arc;
use std::time::Duration;

use gorca_output_api::PointerAvail;

/// The helper's pointer API starts at this version.
pub const MIN_VERSION: u32 = 2;
/// Deadline misses (or `expired` outcomes) in a row that make the helper `Unresponsive`.
pub const MISS_LIMIT: u8 = 3;
/// Re-probe schedule while `Unresponsive`: 2 s, 4 s, ... capped.
pub const REPROBE_MIN: Duration = Duration::from_secs(2);
pub const REPROBE_MAX: Duration = Duration::from_secs(30);
/// While `Denied`, `NoCap` or `TooOld` the owner may have just edited the allow-list.
pub const POLL_EVERY: Duration = Duration::from_secs(10);

/// A unique bus name.
pub type Owner = Arc<str>;

/// Result of one probe (`Ping` then `Capabilities`).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ProbeResult {
    Ok {
        version: u32,
        pointer: bool,
    },
    /// `AccessDenied`: gorca is not allow-listed.
    Denied,
    /// A call exceeded its deadline.
    Timeout,
    /// Any other failure (the helper vanished mid-probe, bad reply).
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AvailEvent {
    /// The owners of `org.gorca.ShellHelper` and `org.gnome.Shell` as they are now.
    Owners {
        helper: Option<Owner>,
        shell: Option<Owner>,
    },
    Probed(ProbeResult),
    /// A deadline miss, or an `expired` outcome (the Shell ran the call late).
    CallMissed,
    /// The helper answered a call (any answer that is not a miss).
    CallAnswered,
    /// The call we gave up on (deadline miss) answered late: the helper is alive, but the miss
    /// already counted and stays counted.
    LateAnswer,
    /// `AccessDenied` on a call: the allow-list may have changed.
    CallDenied,
    /// The 10 s poll for `Denied`, `NoCap` and `TooOld`.
    PollTick,
    /// A scheduled re-probe is due.
    ReprobeDue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AvailEffect {
    /// Check owners, `Ping`, `Capabilities` (150 ms each), then feed [`AvailEvent::Probed`].
    Probe,
    /// Feed [`AvailEvent::ReprobeDue`] after this long (replaces an earlier schedule).
    ReprobeAfter(Duration),
}

/// State: the published availability plus the miss counter and the re-probe backoff.
#[derive(Clone, Debug)]
pub struct AvailMachine {
    avail: PointerAvail,
    misses: u8,
    backoff: Duration,
    /// The helper name is owned by the Shell's owner (the last `Owners` event said so).
    owned: bool,
}

impl Default for AvailMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl AvailMachine {
    pub const fn new() -> Self {
        Self {
            avail: PointerAvail::Absent,
            misses: 0,
            backoff: REPROBE_MIN,
            owned: false,
        }
    }

    pub const fn avail(&self) -> PointerAvail {
        self.avail
    }

    fn next_backoff(&mut self) -> Duration {
        let b = self.backoff;
        self.backoff = (self.backoff * 2).min(REPROBE_MAX);
        b
    }

    /// Apply one event; returns the effects to run. The caller publishes [`Self::avail`] when it
    /// changed.
    pub fn step(&mut self, ev: AvailEvent) -> Vec<AvailEffect> {
        match ev {
            AvailEvent::Owners { helper, shell } => match (helper, shell) {
                (None, _) => {
                    self.avail = PointerAvail::Absent;
                    self.misses = 0;
                    self.owned = false;
                    Vec::new()
                }
                (Some(h), Some(s)) if h == s => {
                    self.owned = true;
                    vec![AvailEffect::Probe]
                }
                // The helper name is owned by something that is not the Shell (MI-7), or the Shell
                // has no owner at all: no call is ever made.
                (Some(_), Some(_) | None) => {
                    self.owned = false;
                    self.avail = PointerAvail::Squatted;
                    self.misses = 0;
                    Vec::new()
                }
            },
            AvailEvent::Probed(r) => self.probed(r),
            AvailEvent::CallMissed => match self.avail {
                PointerAvail::Ready { .. } => {
                    self.misses = self.misses.saturating_add(1);
                    if self.misses >= MISS_LIMIT {
                        self.avail = PointerAvail::Unresponsive;
                        self.backoff = REPROBE_MIN;
                        vec![AvailEffect::ReprobeAfter(self.next_backoff())]
                    } else {
                        Vec::new()
                    }
                }
                PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Denied
                | PointerAvail::TooOld { .. }
                | PointerAvail::NoCap
                | PointerAvail::Unresponsive => Vec::new(),
            },
            AvailEvent::CallAnswered => match self.avail {
                PointerAvail::Ready { .. } => {
                    self.misses = 0;
                    Vec::new()
                }
                // A late answer from a helper we gave up on: look again now.
                PointerAvail::Unresponsive => vec![AvailEffect::Probe],
                PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Denied
                | PointerAvail::TooOld { .. }
                | PointerAvail::NoCap => Vec::new(),
            },
            AvailEvent::LateAnswer => match self.avail {
                PointerAvail::Unresponsive => vec![AvailEffect::Probe],
                PointerAvail::Ready { .. }
                | PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Denied
                | PointerAvail::TooOld { .. }
                | PointerAvail::NoCap => Vec::new(),
            },
            AvailEvent::CallDenied => match self.avail {
                PointerAvail::Ready { .. } | PointerAvail::Unresponsive => {
                    vec![AvailEffect::Probe]
                }
                PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Denied
                | PointerAvail::TooOld { .. }
                | PointerAvail::NoCap => Vec::new(),
            },
            AvailEvent::PollTick => match self.avail {
                PointerAvail::Denied | PointerAvail::NoCap | PointerAvail::TooOld { .. } => {
                    vec![AvailEffect::Probe]
                }
                PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Unresponsive
                | PointerAvail::Ready { .. } => Vec::new(),
            },
            // A scheduled re-probe: only while the helper is owned by the Shell's owner.
            AvailEvent::ReprobeDue => {
                if self.owned {
                    vec![AvailEffect::Probe]
                } else {
                    Vec::new()
                }
            }
        }
    }

    fn probed(&mut self, r: ProbeResult) -> Vec<AvailEffect> {
        match r {
            ProbeResult::Ok { version, pointer } => {
                self.avail = if version < MIN_VERSION {
                    PointerAvail::TooOld { version }
                } else if !pointer {
                    PointerAvail::NoCap
                } else {
                    PointerAvail::Ready { version }
                };
                self.misses = 0;
                self.backoff = REPROBE_MIN;
                Vec::new()
            }
            ProbeResult::Denied => {
                self.avail = PointerAvail::Denied;
                self.misses = 0;
                Vec::new()
            }
            ProbeResult::Timeout | ProbeResult::Failed => match self.avail {
                // A helper we were using stopped answering: keep the gate (C-3), look again later.
                PointerAvail::Ready { .. } | PointerAvail::Unresponsive => {
                    self.avail = PointerAvail::Unresponsive;
                    vec![AvailEffect::ReprobeAfter(self.next_backoff())]
                }
                // Never reached Ready: stay where we are; the poll and owner events retry.
                PointerAvail::Absent
                | PointerAvail::Squatted
                | PointerAvail::Denied
                | PointerAvail::TooOld { .. }
                | PointerAvail::NoCap => vec![AvailEffect::ReprobeAfter(self.next_backoff())],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[allow(clippy::unnecessary_wraps)]
    fn o(s: &str) -> Option<Owner> {
        Some(Arc::from(s))
    }

    fn ready() -> AvailMachine {
        let mut m = AvailMachine::new();
        assert_eq!(
            m.step(AvailEvent::Owners {
                helper: o(":1.5"),
                shell: o(":1.5")
            }),
            vec![AvailEffect::Probe]
        );
        m.step(AvailEvent::Probed(ProbeResult::Ok {
            version: 2,
            pointer: true,
        }));
        m
    }

    #[test]
    fn starts_absent_and_owner_equality_triggers_a_probe() {
        let mut m = AvailMachine::new();
        assert_eq!(m.avail(), PointerAvail::Absent);
        let fx = m.step(AvailEvent::Owners {
            helper: o(":1.5"),
            shell: o(":1.5"),
        });
        assert_eq!(fx, vec![AvailEffect::Probe]);
        assert_eq!(
            m.avail(),
            PointerAvail::Absent,
            "published only after the probe"
        );
    }

    #[test]
    fn a_squatter_is_never_called() {
        let mut m = AvailMachine::new();
        let fx = m.step(AvailEvent::Owners {
            helper: o(":1.9"),
            shell: o(":1.5"),
        });
        assert!(fx.is_empty());
        assert_eq!(m.avail(), PointerAvail::Squatted);
        // Shell without an owner counts as a squatter too.
        let mut m = AvailMachine::new();
        m.step(AvailEvent::Owners {
            helper: o(":1.9"),
            shell: None,
        });
        assert_eq!(m.avail(), PointerAvail::Squatted);
    }

    #[test]
    fn probe_results_map_to_states() {
        let mut m = AvailMachine::new();
        m.step(AvailEvent::Probed(ProbeResult::Ok {
            version: 1,
            pointer: true,
        }));
        assert_eq!(m.avail(), PointerAvail::TooOld { version: 1 });
        m.step(AvailEvent::Probed(ProbeResult::Ok {
            version: 2,
            pointer: false,
        }));
        assert_eq!(m.avail(), PointerAvail::NoCap);
        m.step(AvailEvent::Probed(ProbeResult::Denied));
        assert_eq!(m.avail(), PointerAvail::Denied);
        m.step(AvailEvent::Probed(ProbeResult::Ok {
            version: 2,
            pointer: true,
        }));
        assert_eq!(m.avail(), PointerAvail::Ready { version: 2 });
    }

    #[test]
    fn helper_owner_gone_is_absent_from_any_state() {
        let mut m = ready();
        m.step(AvailEvent::Owners {
            helper: None,
            shell: o(":1.5"),
        });
        assert_eq!(m.avail(), PointerAvail::Absent);
    }

    #[test]
    fn three_misses_make_it_unresponsive_and_the_gate_stays_open() {
        let mut m = ready();
        assert!(m.step(AvailEvent::CallMissed).is_empty());
        assert!(m.step(AvailEvent::CallMissed).is_empty());
        let fx = m.step(AvailEvent::CallMissed);
        assert_eq!(fx, vec![AvailEffect::ReprobeAfter(REPROBE_MIN)]);
        assert_eq!(m.avail(), PointerAvail::Unresponsive);
        assert!(m.avail().gate_open());
    }

    #[test]
    fn an_answer_resets_the_miss_counter() {
        let mut m = ready();
        m.step(AvailEvent::CallMissed);
        m.step(AvailEvent::CallMissed);
        m.step(AvailEvent::CallAnswered);
        m.step(AvailEvent::CallMissed);
        m.step(AvailEvent::CallMissed);
        assert_eq!(m.avail(), PointerAvail::Ready { version: 2 });
    }

    #[test]
    fn unresponsive_backs_off_2_4_8_16_30_and_recovers_on_a_good_probe() {
        let mut m = ready();
        for _ in 0..3 {
            m.step(AvailEvent::CallMissed);
        }
        let mut seen = Vec::new();
        for _ in 0..5 {
            assert_eq!(m.step(AvailEvent::ReprobeDue), vec![AvailEffect::Probe]);
            let fx = m.step(AvailEvent::Probed(ProbeResult::Timeout));
            let [AvailEffect::ReprobeAfter(d)] = fx[..] else {
                panic!("{fx:?}")
            };
            seen.push(d.as_secs());
        }
        assert_eq!(seen, vec![4, 8, 16, 30, 30]);
        m.step(AvailEvent::Probed(ProbeResult::Ok {
            version: 2,
            pointer: true,
        }));
        assert_eq!(m.avail(), PointerAvail::Ready { version: 2 });
    }

    #[test]
    fn a_late_answer_from_an_unresponsive_helper_triggers_a_probe() {
        let mut m = ready();
        for _ in 0..3 {
            m.step(AvailEvent::CallMissed);
        }
        assert_eq!(m.step(AvailEvent::LateAnswer), vec![AvailEffect::Probe]);
    }

    #[test]
    fn a_late_answer_does_not_reset_the_miss_counter() {
        let mut m = ready();
        m.step(AvailEvent::CallMissed);
        m.step(AvailEvent::CallMissed);
        m.step(AvailEvent::LateAnswer);
        m.step(AvailEvent::CallMissed);
        assert_eq!(m.avail(), PointerAvail::Unresponsive);
    }

    #[test]
    fn denied_nocap_tooold_are_polled_and_others_are_not() {
        for a in [
            (ProbeResult::Denied),
            ProbeResult::Ok {
                version: 2,
                pointer: false,
            },
            ProbeResult::Ok {
                version: 1,
                pointer: true,
            },
        ] {
            let mut m = AvailMachine::new();
            m.step(AvailEvent::Probed(a));
            assert_eq!(m.step(AvailEvent::PollTick), vec![AvailEffect::Probe]);
        }
        let mut m = ready();
        assert!(m.step(AvailEvent::PollTick).is_empty());
    }

    #[test]
    fn access_denied_on_a_call_reprobes_without_changing_state_first() {
        let mut m = ready();
        assert_eq!(m.step(AvailEvent::CallDenied), vec![AvailEffect::Probe]);
        assert_eq!(m.avail(), PointerAvail::Ready { version: 2 });
        m.step(AvailEvent::Probed(ProbeResult::Denied));
        assert_eq!(m.avail(), PointerAvail::Denied);
    }

    #[test]
    fn a_failed_first_probe_keeps_the_state_and_schedules_a_retry() {
        let mut m = AvailMachine::new();
        let fx = m.step(AvailEvent::Probed(ProbeResult::Failed));
        assert_eq!(fx, vec![AvailEffect::ReprobeAfter(REPROBE_MIN)]);
        assert_eq!(m.avail(), PointerAvail::Absent);
    }
}
