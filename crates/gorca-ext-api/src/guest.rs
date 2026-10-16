//! Component build: WIT bindings for the guest side and the adapter that maps the WIT `hooks`
//! exports onto [`Hooks`]. Only compiled for `wasm32`.

use std::cell::RefCell;
use std::marker::PhantomData;

use crate::api::{
    Budget, CommandResult, Event, EventVerdict, Hooks, NavUnit, Node, PresentCtx, PresentReason,
    Prio, QueryError, Role, Segment, SegmentKind,
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
