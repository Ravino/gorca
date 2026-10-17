//! A terminal policy session over an extension slot (the `terminal` interface, ADR-0001 T-2).
//!
//! The Reader owns one [`HostTermSession`] per tracked terminal and drives it with plain inputs.
//! Every call goes through the slot, so the slot's fault policy applies: a trap, a fuel or
//! memory overrun drops the instance and a worker builds the next one; three resets in 60 seconds
//! disable the extension for the application (one spoken notice).
//!
//! The terminal policy keeps its state inside the instance (the screen, pending keys, a password
//! prompt). A fault loses it, so the session notices a new instance (the slot's generation) or a
//! lost call and starts the next call by configuring the instance and re-baselining it with a
//! `focus` (the screen is read again). Until then the session answers no actions: the Reader's
//! keys fall through to the generic key echo, so a faulted policy degrades to the core's own
//! behaviour and never goes silent. Nothing here blocks: the slot is only `try_lock`ed.
//!
//! Debug output and logs never contain inputs (a typed key can be a password character).

use std::cell::{Cell, RefCell};
use std::fmt;
use std::sync::Arc;

use gorca_core::TermSession;
use gorca_ext_api::terminal::{
    Ms, TermAction, TermConfig, TermCounters, TermEdit, TermInput, TermSnapshot,
};

use crate::instance::HookKind;
use crate::slot::Slot;

/// Reader-side handle on one terminal of one application.
pub struct HostTermSession {
    slot: Arc<Slot>,
    cfg: TermConfig,
    /// The instance generation this session last configured and drove (0: none yet).
    seen: u64,
    /// The last call was lost (busy, faulted, rebuilding): the instance state is unknown.
    stale: bool,
    /// `step` ran since the snapshot cache was filled.
    dirty: Cell<bool>,
    cache: RefCell<Option<TermSnapshot>>,
}

impl fmt::Debug for HostTermSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostTermSession")
            .field("ext", &self.slot.name())
            .field("seen", &self.seen)
            .field("stale", &self.stale)
            .finish_non_exhaustive()
    }
}

impl HostTermSession {
    pub fn new(slot: Arc<Slot>, cfg: TermConfig) -> Self {
        Self {
            slot,
            cfg,
            seen: 0,
            stale: false,
            dirty: Cell::new(true),
            cache: RefCell::new(None),
        }
    }
}

impl TermSession for HostTermSession {
    fn alive(&self) -> bool {
        !self.stale
    }

    fn step(&mut self, now: Ms, input: TermInput, out: &mut Vec<TermAction>) {
        if matches!(input, TermInput::Blur) && self.seen == 0 {
            // A session the policy never ran for has nothing to wipe.
            return;
        }
        self.dirty.set(true);
        // Screen text comes from the application: it must never read as a message reference.
        let input = match input {
            TermInput::Screen { text, caret } => TermInput::Screen {
                text: gorca_i18n::ext::sanitize(&text).into_owned(),
                caret,
            },
            TermInput::Edit(TermEdit::Insert {
                offset,
                len,
                text: Some(t),
            }) => TermInput::Edit(TermEdit::Insert {
                offset,
                len,
                text: Some(gorca_i18n::ext::sanitize(&t).into_owned()),
            }),
            other => other,
        };
        let slot = &self.slot;
        let cfg = &self.cfg;
        let seen = self.seen;
        let stale = self.stale;
        let r = slot.call(HookKind::Terminal, false, |inst| {
            let gen_now = slot.generation();
            let fresh = gen_now != seen || stale;
            let mut acts = Vec::new();
            if gen_now != seen {
                inst.term_configure(cfg)?;
            }
            let is_focus = matches!(input, TermInput::Focus);
            let is_blur = matches!(input, TermInput::Blur);
            // A new or doubtful instance has no baseline: read the screen first. A `focus` is
            // its own baseline; a `blur` of a doubtful instance still wipes it (it may hold a
            // secret), a fresh one has nothing to wipe.
            if fresh && gen_now != seen && is_blur {
                // Nothing to wipe in an instance this session never drove.
            } else {
                if fresh && !is_focus && !is_blur {
                    acts.extend(inst.term_step(now, TermInput::Focus)?);
                }
                acts.extend(inst.term_step(now, input)?);
            }
            Ok((gen_now, acts))
        });
        match r {
            Some((gen_now, acts)) => {
                self.seen = gen_now;
                self.stale = false;
                // An extension speaks its own messages only.
                let ns = self.slot.name();
                out.extend(acts.into_iter().map(|a| match a {
                    TermAction::Say {
                        text,
                        unit,
                        urgency,
                        sensitive,
                    } => TermAction::Say {
                        text: gorca_i18n::ext::restrict_to(&text, ns).into_owned(),
                        unit,
                        urgency,
                        sensitive,
                    },
                    other => other,
                }));
            }
            None => self.stale = true,
        }
    }

    fn counters(&self) -> TermCounters {
        self.slot
            .call(HookKind::Terminal, false, |inst| inst.term_counters())
            .unwrap_or_default()
    }

    fn snapshot(&self) -> Option<TermSnapshot> {
        if !self.dirty.get() {
            return self.cache.borrow().clone();
        }
        // A session that has not (successfully) stepped on the current instance has no screen.
        let fresh = self.seen == self.slot.generation() && !self.stale;
        let snap = if fresh {
            self.slot
                .call(HookKind::Terminal, false, |inst| inst.term_snapshot())
                .flatten()
        } else {
            None
        };
        self.dirty.set(false);
        self.cache.borrow_mut().clone_from(&snap);
        snap
    }
}
