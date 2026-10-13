//! Input boundary: key events, key sources, injection, grab sets and key classification.
//!
//! Responsibility (ADR-0001 section 11): `KeyEvent`, `KeySource`, `KeyInjector`, `GrabSet`,
//! `KeyClass`.
//!
//! > "Key handling depends on `gorca-input-api::KeyEvent`, never on a D-Bus interface name, so a
//! > Newton `KeyEvent` source can be added behind the same trait."  (section 5.7)
//!
//! Invariants captured here (section 7.3):
//! - **K-1** Grab sets are minimal: gorca's modifier, explicit command chords and, in Browse
//!   mode, quick-navigation keys. Everything else is watched, never grabbed.
//! - **K-6** `SetKeyGrabs` matches modifier masks exactly, so each chord is registered with its
//!   NumLock and CapsLock variants (x4), as libatspi does: see [`expand_lock_variants`].
//!
//! Allowed dependencies: `gorca-model`, `gorca-rt`, `bitflags`, `thiserror`. Never `zbus`: the
//! Mutter client lives in `gorca-input-mutter`, the injector in `gorca-input-inject`.

mod event;
mod grab;
mod keymap;
mod keys;
mod lane;
mod mutter_model;
mod traits;

pub use event::{CmdId, KeyClass, KeyEvent, KeyIntent, Modifiers, PressKind};
pub use grab::{Chord, GrabSet, expand_lock_variants};
pub use keymap::{
    Bound, ChordKey, CompiledKeymap, GrabView, InputContext, KeyRef, LaneLinks, LaneSettings,
    ModeKind, TableKind,
};
pub use keys::{
    KeyResolver, StaticResolver, is_control, is_modifier_keysym, is_shift, modifier_keysym,
    spoken_key_name, static_keycode, static_keysym, sym,
};
pub use lane::{HELP_IDLE, HELP_STALL, InputLane, LaneAlert, LaneOut, LaneStats, SHIFT_TAP_MAX};
pub use mutter_model::{Ev, Replica, Timing, Verdict};
pub use traits::{
    InjectError, InjectKey, KeyError, KeyInjector, KeySink, KeySource, KeySourceCtx, KeySourceKind,
    key_channel,
};
