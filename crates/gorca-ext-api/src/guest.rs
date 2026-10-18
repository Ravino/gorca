// SPDX-License-Identifier: MIT OR Apache-2.0
//! Component build: WIT bindings for the guest side and the adapter that maps the WIT `hooks`
//! exports onto [`Hooks`]. Only compiled for `wasm32`.

use std::cell::RefCell;
use std::marker::PhantomData;

use crate::api::{
    Budget, CommandResult, Event, EventVerdict, Hooks, NavUnit, Node, PolicyFacts, PolicyMoment,
    PolicyVerdict, PresentCtx, PresentReason, Prio, QueryError, Role, Segment, SegmentKind,
};

#[allow(clippy::all, clippy::pedantic, missing_docs, unused_unsafe)]
pub mod wit {
    wit_bindgen::generate!({
        world: "extension",
        path: "wit",
        pub_export_macro: true,
        default_bindings_module: "gorca_ext_api::guest::wit",
    });
}

use wit::exports::gorca::ext::hooks as wh;
use wit::gorca::ext::types as wt;

macro_rules! role_to_wit {
    ($($v:ident),*) => {
        fn role_to_wit(r: Role) -> wt::Role {
            match r { $(Role::$v => wt::Role::$v),* }
        }
        fn role_from_wit(r: wt::Role) -> Role {
            match r { $(wt::Role::$v => Role::$v),* }
        }
    };
}
crate::for_each_role!(role_to_wit);

pub fn node_to_wit(n: Node) -> wt::Node {
    wt::Node {
        key: n.key,
        role: role_to_wit(n.role),
        states: n.states,
        name: n.name,
        description: n.description,
        range: n.range.map(|r| wt::Range {
            current: r.current,
            min: r.min,
            max: r.max,
        }),
    }
}

pub fn node_from_wit(n: wt::Node) -> Node {
    Node {
        key: n.key,
        role: role_from_wit(n.role),
        states: n.states,
        name: n.name,
        description: n.description,
        range: n.range.map(|r| crate::Range {
            current: r.current,
            min: r.min,
            max: r.max,
        }),
    }
}

pub fn budget_to_wit(b: Budget) -> wt::Budget {
    match b {
        Budget::Interactive => wt::Budget::Interactive,
        Budget::Prefetch => wt::Budget::Prefetch,
        Budget::Background => wt::Budget::Background,
    }
}

pub fn query_error_from_wit(e: wt::QueryError) -> QueryError {
    match e {
        wt::QueryError::Timeout => QueryError::Timeout,
        wt::QueryError::Gone => QueryError::Gone,
        wt::QueryError::NotSupported => QueryError::NotSupported,
        wt::QueryError::Denied => QueryError::Denied,
    }
}

pub fn prio_to_wit(p: Prio) -> wt::Prio {
    match p {
        Prio::Interrupt => wt::Prio::Interrupt,
        Prio::Message => wt::Prio::Message,
        Prio::Text => wt::Prio::Text,
        Prio::Progress => wt::Prio::Progress,
        Prio::Notification => wt::Prio::Notification,
    }
}

pub fn segment_to_wit(s: Segment) -> wt::Segment {
    wt::Segment {
        text: s.text,
        kind: match s.kind {
            SegmentKind::Text => wt::SegmentKind::Text,
            SegmentKind::Role => wt::SegmentKind::Role,
            SegmentKind::State => wt::SegmentKind::State,
            SegmentKind::Value => wt::SegmentKind::Value,
        },
    }
}

fn event_from_wit(e: wt::Event) -> Event {
    match e {
        wt::Event::FocusChanged(k) => Event::FocusChanged(k),
        wt::Event::PropertiesChanged((k, m)) => Event::PropertiesChanged(k, m),
        wt::Event::ChildrenChanged(k) => Event::ChildrenChanged(k),
        wt::Event::TextChanged(k) => Event::TextChanged(k),
        wt::Event::CaretMoved((k, o)) => Event::CaretMoved(k, o),
        wt::Event::SelectionChanged(k) => Event::SelectionChanged(k),
        wt::Event::Announcement(s) => Event::Announcement(s),
        wt::Event::WindowActivated(k) => Event::WindowActivated(k),
        wt::Event::NodeRemoved(k) => Event::NodeRemoved(k),
    }
}

fn verdict_to_wit(v: EventVerdict) -> wt::EventVerdict {
    match v {
        EventVerdict::Pass => wt::EventVerdict::Pass,
        EventVerdict::Suppress => wt::EventVerdict::Suppress,
        EventVerdict::Rewrite(s) => {
            wt::EventVerdict::Rewrite(s.into_iter().map(segment_to_wit).collect())
        }
        EventVerdict::ReEvaluate => wt::EventVerdict::ReEvaluate,
    }
}

fn ctx_from_wit(c: wt::PresentCtx) -> PresentCtx {
    PresentCtx {
        epoch: c.epoch,
        reason: match c.reason {
            wt::PresentReason::Focus => PresentReason::Focus,
            wt::PresentReason::Navigation => PresentReason::Navigation,
            wt::PresentReason::SayAll => PresentReason::SayAll,
            wt::PresentReason::Review => PresentReason::Review,
        },
    }
}

fn policy_facts_from_wit(f: wt::PolicyFacts) -> PolicyFacts {
    PolicyFacts {
        moment: match f.moment {
            wt::PolicyMoment::Focus => PolicyMoment::Focus,
            wt::PolicyMoment::Value => PolicyMoment::Value,
            wt::PolicyMoment::Activate => PolicyMoment::Activate,
            wt::PolicyMoment::Showing => PolicyMoment::Showing,
        },
        role: role_from_wit(f.role),
        states: f.states,
        name: f.name,
        description: f.description,
        range: f.range.map(|r| crate::Range {
            current: r.current,
            min: r.min,
            max: r.max,
        }),
    }
}

fn policy_verdict_to_wit(v: PolicyVerdict) -> wt::PolicyVerdict {
    match v {
        PolicyVerdict::Defer => wt::PolicyVerdict::Defer,
        PolicyVerdict::Silence => wt::PolicyVerdict::Silence,
        PolicyVerdict::Value(s) => wt::PolicyVerdict::Value(s),
        PolicyVerdict::Say(s) => {
            wt::PolicyVerdict::Say(s.into_iter().map(segment_to_wit).collect())
        }
        PolicyVerdict::Notice => wt::PolicyVerdict::Notice,
    }
}

fn command_to_wit(r: CommandResult) -> wt::CommandResult {
    match r {
        CommandResult::Handled => wt::CommandResult::Handled,
        CommandResult::NotHandled => wt::CommandResult::NotHandled,
        CommandResult::Speak(s) => {
            wt::CommandResult::Speak(s.into_iter().map(segment_to_wit).collect())
        }
    }
}

thread_local! {
    static EXT: RefCell<Option<Box<dyn Hooks>>> = const { RefCell::new(None) };
}

/// Maps the WIT `hooks` exports onto `T`. The extension value is created on first use
/// (`T::default()`); a trap discards the whole instance, so a poisoned value never survives.
#[derive(Debug)]
pub struct Adapter<T>(PhantomData<T>);

fn with<T: Hooks + Default + 'static, R>(f: impl FnOnce(&mut dyn Hooks) -> R) -> R {
    EXT.with(|cell| {
        let mut slot = cell.borrow_mut();
        let ext = slot.get_or_insert_with(|| Box::new(T::default()));
        f(ext.as_mut())
    })
}

impl<T: Hooks + Default + 'static> wh::Guest for Adapter<T> {
    fn on_event(ev: wt::Event) -> wt::EventVerdict {
        verdict_to_wit(with::<T, _>(|e| e.on_event(&event_from_wit(ev))))
    }

    fn present(n: wt::Node, ctx: wt::PresentCtx) -> Option<Vec<wt::Segment>> {
        with::<T, _>(|e| e.present(&node_from_wit(n), ctx_from_wit(ctx)))
            .map(|v| v.into_iter().map(segment_to_wit).collect())
    }

    fn policy(f: wt::PolicyFacts) -> wt::PolicyVerdict {
        policy_verdict_to_wit(with::<T, _>(|e| e.policy(&policy_facts_from_wit(f))))
    }

    fn nav_units() -> Vec<wt::NavUnit> {
        with::<T, _>(|e| e.nav_units())
            .into_iter()
            .map(|u: NavUnit| wt::NavUnit {
                id: u.id,
                name: u.name,
            })
            .collect()
    }

    fn nav_match(unit: u32, n: wt::Node) -> bool {
        with::<T, _>(|e| e.nav_match(unit, &node_from_wit(n)))
    }

    fn command(id: u32) -> wt::CommandResult {
        command_to_wit(with::<T, _>(|e| e.command(id)))
    }
}

// ---- the `terminal` export ------------------------------------------------------------------

use wit::exports::gorca::ext::terminal as wtm;

use crate::terminal as tm;

fn term_key_from_wit(k: wtm::TermKey) -> tm::TermKey {
    match k {
        wtm::TermKey::Typed(c) => tm::TermKey::Typed(c),
        wtm::TermKey::Enter => tm::TermKey::Enter,
        wtm::TermKey::Backspace => tm::TermKey::Backspace,
        wtm::TermKey::Delete => tm::TermKey::Delete,
        wtm::TermKey::Tab => tm::TermKey::Tab,
        wtm::TermKey::CharLeft => tm::TermKey::CharLeft,
        wtm::TermKey::CharRight => tm::TermKey::CharRight,
        wtm::TermKey::WordLeft => tm::TermKey::WordLeft,
        wtm::TermKey::WordRight => tm::TermKey::WordRight,
        wtm::TermKey::LineStart => tm::TermKey::LineStart,
        wtm::TermKey::LineEnd => tm::TermKey::LineEnd,
        wtm::TermKey::LineUp => tm::TermKey::LineUp,
        wtm::TermKey::LineDown => tm::TermKey::LineDown,
        wtm::TermKey::Interrupt => tm::TermKey::Interrupt,
        wtm::TermKey::Other => tm::TermKey::Other,
    }
}

fn term_input_from_wit(i: wtm::TermInput) -> tm::TermInput {
    match i {
        wtm::TermInput::Key(k) => tm::TermInput::Key(term_key_from_wit(k)),
        wtm::TermInput::Edit(e) => tm::TermInput::Edit(match e {
            wtm::TermEdit::Insert((offset, len, text)) => {
                tm::TermEdit::Insert { offset, len, text }
            }
            wtm::TermEdit::Delete((offset, len)) => tm::TermEdit::Delete { offset, len },
            wtm::TermEdit::Replaced => tm::TermEdit::Replaced,
        }),
        wtm::TermInput::Caret(o) => tm::TermInput::Caret(o),
        wtm::TermInput::Screen((text, caret)) => tm::TermInput::Screen { text, caret },
        wtm::TermInput::ScreenFailed => tm::TermInput::ScreenFailed,
        wtm::TermInput::Tick => tm::TermInput::Tick,
        wtm::TermInput::Focus => tm::TermInput::Focus,
        wtm::TermInput::Blur => tm::TermInput::Blur,
        wtm::TermInput::Raw(k) => tm::TermInput::Raw(tm::RawKey {
            keysym: k.keysym,
            unichar: k.unichar,
            phys: k.phys,
            shift: k.shift,
            ctrl: k.ctrl,
            alt: k.alt,
            logo: k.logo,
        }),
    }
}

fn term_action_to_wit(a: tm::TermAction) -> wtm::TermAction {
    match a {
        tm::TermAction::Say {
            text,
            unit,
            urgency,
            sensitive,
        } => wtm::TermAction::Say((
            text,
            match unit {
                tm::Unit::Char => wtm::Unit::Char,
                tm::Unit::Word => wtm::Unit::Word,
                tm::Unit::Line => wtm::Unit::Line,
                tm::Unit::Text => wtm::Unit::Text,
            },
            match urgency {
                tm::Urgency::Key => wtm::Urgency::Key,
                tm::Urgency::Queued => wtm::Urgency::Queued,
                tm::Urgency::Output => wtm::Urgency::Output,
            },
            sensitive,
        )),
        tm::TermAction::FetchScreen => wtm::TermAction::FetchScreen,
        tm::TermAction::ArmTimer(t) => wtm::TermAction::ArmTimer(t),
        tm::TermAction::ClaimKey => wtm::TermAction::ClaimKey,
        tm::TermAction::Cue => wtm::TermAction::Cue,
    }
}

fn with_terminal<T: Hooks + Default + 'static, R>(
    f: impl FnOnce(&mut dyn tm::TerminalHooks) -> R,
) -> Option<R> {
    with::<T, _>(|e| e.terminal().map(f))
}

impl<T: Hooks + Default + 'static> wtm::Guest for Adapter<T> {
    fn configure(cfg: wtm::TermConfig) {
        let cfg = tm::TermConfig {
            settle_ms: cfg.settle_ms,
            max_wait_ms: cfg.max_wait_ms,
            prompt_markers: cfg.prompt_markers,
            prompt_regex: cfg.prompt_regex,
            secret_regex: cfg.secret_regex,
            burst_lines: cfg.burst_lines,
            burst_tail: cfg.burst_tail,
            max_output_chars: cfg.max_output_chars,
            word_level: cfg.word_level.map(|l| match l {
                wtm::WordLevel::Fine => tm::WordLevel::Fine,
                wtm::WordLevel::Medium => tm::WordLevel::Medium,
                wtm::WordLevel::Coarse => tm::WordLevel::Coarse,
            }),
            boundary: match cfg.boundary {
                wtm::BoundaryMode::Cue => tm::BoundaryMode::Cue,
                wtm::BoundaryMode::Repeat => tm::BoundaryMode::Repeat,
                wtm::BoundaryMode::Silent => tm::BoundaryMode::Silent,
            },
        };
        with_terminal::<T, _>(|t| t.configure(&cfg));
    }

    fn snapshot() -> Option<wtm::TermSnapshot> {
        with_terminal::<T, _>(|t| t.snapshot())
            .flatten()
            .map(|s| wtm::TermSnapshot {
                text: s.text,
                caret: s.caret,
                generation: s.generation,
            })
    }

    fn counters() -> wtm::TermCounters {
        let c = with_terminal::<T, _>(|t| t.counters()).unwrap_or_default();
        wtm::TermCounters {
            fetches: c.fetches,
            fetch_failed: c.fetch_failed,
            settled_local: c.settled_local,
            resync_mismatch: c.resync_mismatch,
            output_spoken: c.output_spoken,
            output_lines_spoken: c.output_lines_spoken,
            output_lines_skipped: c.output_lines_skipped,
            repaints: c.repaints,
            typed_echoed: c.typed_echoed,
            typed_explained: c.typed_explained,
            history_read: c.history_read,
            secret_prompts: c.secret_prompts,
        }
    }

    fn step(now: u64, input: wtm::TermInput) -> Vec<wtm::TermAction> {
        let input = term_input_from_wit(input);
        let mut out = Vec::new();
        with_terminal::<T, _>(|t| t.step(now, input, &mut out));
        out.into_iter().map(term_action_to_wit).collect()
    }
}
