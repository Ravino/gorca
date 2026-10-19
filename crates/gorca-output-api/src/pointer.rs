// SPDX-License-Identifier: GPL-3.0-or-later
//! Pointer output: the neutral types and the [`PointerSink`] trait of ADR-0025 sections 2.4 and 2.9.
//!
//! The core never sees stage coordinates or a transport: a connector resolves a node to a
//! [`PointTarget`], the sink (today the GNOME Shell helper client) maps it to the compositor and
//! injects. Every call carries a [`Deadline`]; the result comes back as a value, never by blocking
//! the key path (ADR-0001 R3, ADR-0025 MI-2).
//!
//! > "`PointerSink` trait (output side, like speech), `PointTarget`, `ToplevelHint`, `Button`,
//! > `ButtonAct`, `PointerOutcome`, `PointerError`. Neutral, no zbus."  (ADR-0025 2.9)

use std::sync::Arc;

use gorca_rt::{BoxFuture, Deadline, watch};

/// A mouse button as the user means it (logical: the helper swaps for a left-handed user).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum Button {
    Left,
    Middle,
    Right,
}

impl Button {
    /// The wire number (1 left, 2 middle, 3 right).
    pub const fn wire(self) -> u32 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        }
    }

    /// Bit of this button in the helper's `held` mask (left 1, middle 2, right 4).
    pub const fn held_bit(self) -> u32 {
        match self {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 4,
        }
    }
}

/// What to do with a button at the current pointer position.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ButtonAct {
    Click,
    DoubleClick,
    Press,
    Release,
}

/// How a client reports coordinates of children below the toplevel (ADR-0025 2.4).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ChildSpace {
    /// Same space as the toplevel (logical px).
    Toplevel,
    /// Device pixels: divide by the monitor scale (Chromium, Electron).
    DevicePixels,
}

/// What a connector knows about the toplevel a point belongs to; the sink matches it to a window.
#[derive(Clone, Debug, PartialEq)]
pub struct ToplevelHint {
    pub pid: u32,
    pub title: Arc<str>,
    pub atspi_origin: (i32, i32),
    pub atspi_size: (i32, i32),
    pub child_space: ChildSpace,
    /// The connector VERIFIED that `pid` is not the application's own (a Flatpak or bus-proxy application
    /// reports the proxy's pid). Only then may the sink match without the pid, and then only on an exact
    /// title and exact size with a single candidate. `false` (the default) never matches other processes'
    /// windows: a hostile application must not receive clicks meant for another one.
    pub pid_is_proxy: bool,
}

/// Where to put the pointer.
#[derive(Clone, Debug, PartialEq)]
pub enum PointTarget {
    /// A point in the toplevel's own AT-SPI coordinate space.
    Window { top: ToplevelHint, x: f64, y: f64 },
    /// A stage point (St / Shell UI extents are stage coordinates).
    Stage { x: f64, y: f64 },
}

/// Click policy knobs (ADR-0025 2.2 opts).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ClickOpts {
    /// Click even if something covers the point (`mouse.if_covered = click`).
    pub if_covered: IfCovered,
    /// `Click` or `DoubleClick` for [`PointerSink::click_at`].
    pub count: ClickCount,
}

impl Default for ClickOpts {
    fn default() -> Self {
        Self {
            if_covered: IfCovered::Refuse,
            count: ClickCount::One,
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IfCovered {
    Refuse,
    Proceed,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum ClickCount {
    One,
    Two,
}

/// The result kind of an injecting call that reached the compositor.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum OutcomeKind {
    Done,
    /// The Shell ran the call after its `not-after`: nothing was injected.
    Expired,
    /// Another window, a banner, the Overview or an attached dialog covers the point.
    RefusedOccluded,
    /// The point is outside the window (or inside a window for a stage target).
    RefusedOutside,
}

/// Reply of an injecting helper call.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerOutcome {
    pub kind: OutcomeKind,
    pub x: f64,
    pub y: f64,
    /// Topmost window at the point (0 = Shell UI or none).
    pub window_at: u64,
    pub occluded: bool,
    pub clamped: bool,
    /// Bitmask of the buttons this caller holds ([`Button::held_bit`]).
    pub held: u32,
}

impl PointerOutcome {
    pub fn holds(&self, b: Button) -> bool {
        self.held & b.held_bit() != 0
    }
}

/// `GetPointer`.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerPos {
    pub x: f64,
    pub y: f64,
    pub window: u64,
    pub wx: f64,
    pub wy: f64,
    pub monitor: i32,
}

/// Availability of pointer emulation (ADR-0025 2.5), owned by the pointer actor and mirrored to
/// the Reader. The gate that grabs the keypad chords is [`PointerAvail::gate_open`].
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum PointerAvail {
    /// No owner of `org.gorca.ShellHelper`.
    Absent,
    /// The name is owned by something that is not the owner of `org.gnome.Shell` (MI-7).
    Squatted,
    /// The helper refuses gorca (not allow-listed).
    Denied,
    /// Helper older than version 2.
    TooOld {
        version: u32,
    },
    /// Allowed, but the allow-list lacks the `pointer` cap.
    NoCap,
    /// Three consecutive deadline misses; keys stay grabbed (C-3).
    Unresponsive,
    Ready {
        version: u32,
    },
}

impl PointerAvail {
    /// The gate of ADR-0025 C-3: open while `Ready` or `Unresponsive`; the Reader also requires
    /// `mouse.enabled`. A Shell stall stalls key delivery too, so closing on it buys nothing.
    pub const fn gate_open(self) -> bool {
        match self {
            PointerAvail::Ready { .. } | PointerAvail::Unresponsive => true,
            PointerAvail::Absent
            | PointerAvail::Squatted
            | PointerAvail::Denied
            | PointerAvail::TooOld { .. }
            | PointerAvail::NoCap => false,
        }
    }

    /// Calls may be sent (only `Ready`).
    pub const fn can_call(self) -> bool {
        match self {
            PointerAvail::Ready { .. } => true,
            PointerAvail::Absent
            | PointerAvail::Squatted
            | PointerAvail::Denied
            | PointerAvail::TooOld { .. }
            | PointerAvail::NoCap
            | PointerAvail::Unresponsive => false,
        }
    }
}

/// Why a pointer request failed. Each maps to one spoken message (ADR-0025 section 4).
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PointerError {
    #[error("pointer emulation is not available: {0:?}")]
    Unavailable(PointerAvail),
    #[error("the session is locked")]
    Locked,
    /// Another caller holds a button, or the request queue is full.
    #[error("the pointer is busy")]
    Busy,
    #[error("rate limited by the helper")]
    RateLimited,
    #[error("no such window")]
    NoWindow,
    #[error("bad request: {0}")]
    BadArgs(String),
    /// The target has no usable location (matching failed, off-screen, no extents).
    #[error("no location")]
    NoLocation,
    /// The helper did not answer within the call deadline.
    #[error("the helper did not answer in time")]
    Timeout,
    /// An earlier call is overdue or the helper is unresponsive: nothing is dispatched.
    #[error("the helper is not responding")]
    NotResponding,
    #[error("the helper refused the caller")]
    Denied,
    #[error("helper error: {0}")]
    Failed(String),
}

/// The pointer output backend (GNOME Shell helper today; Newton later). Implementations never
/// block the caller beyond the deadline and never run on the key to speech path.
pub trait PointerSink: Send + Sync {
    fn click_at(
        &self,
        target: PointTarget,
        button: Button,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;

    fn move_to(
        &self,
        target: PointTarget,
        opts: ClickOpts,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;

    /// Act on `button` at the current pointer position.
    fn button(
        &self,
        button: Button,
        act: ButtonAct,
        deadline: Deadline,
    ) -> BoxFuture<'_, Result<PointerOutcome, PointerError>>;

    /// Release every button this caller holds; returns how many.
    fn release_all(&self, deadline: Deadline) -> BoxFuture<'_, Result<u32, PointerError>>;

    fn position(&self, deadline: Deadline) -> BoxFuture<'_, Result<PointerPos, PointerError>>;

    /// The availability watch; the latest value is always current, changes wake the Reader.
    fn availability(&self) -> watch::Receiver<PointerAvail>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gate_is_open_only_for_ready_and_unresponsive() {
        assert!(PointerAvail::Ready { version: 2 }.gate_open());
        assert!(PointerAvail::Unresponsive.gate_open());
        for a in [
            PointerAvail::Absent,
            PointerAvail::Squatted,
            PointerAvail::Denied,
            PointerAvail::TooOld { version: 1 },
            PointerAvail::NoCap,
        ] {
            assert!(!a.gate_open(), "{a:?}");
        }
    }

    #[test]
    fn only_ready_can_call() {
        assert!(PointerAvail::Ready { version: 2 }.can_call());
        assert!(!PointerAvail::Unresponsive.can_call());
    }

    #[test]
    fn button_wire_and_bits() {
        assert_eq!(Button::Left.wire(), 1);
        assert_eq!(Button::Right.held_bit(), 4);
        let o = PointerOutcome {
            kind: OutcomeKind::Done,
            x: 0.0,
            y: 0.0,
            window_at: 0,
            occluded: false,
            clamped: false,
            held: 5,
        };
        assert!(o.holds(Button::Left) && o.holds(Button::Right) && !o.holds(Button::Middle));
    }
}
