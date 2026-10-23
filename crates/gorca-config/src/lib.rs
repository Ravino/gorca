// SPDX-License-Identifier: GPL-3.0-or-later
//! gorca settings and keymaps as data (ADR-0005 section 7).
//!
//! Pure: no tokio, no I/O, no XKB, no dependency on `gorca-core`. The daemon reads the file,
//! watches it, and publishes the result; this crate turns text into validated values and back.
//!
//! - [`load`] never fails and never panics: every problem becomes a [`Diag`] and the affected
//!   key keeps its default (CV-1, 7.4).
//! - [`save`] merges runtime changes ([`Overlay`]) into the original text, keeping comments.
//! - [`ResolvedKeymap`] holds chords and command **names** per context, layered as built-in
//!   preset, then the user file (KC-1 to KC-6). Mapping names to commands is `gorca-core`'s job.

mod diag;
pub mod glob;
mod keymap;
mod load;
mod patch;
mod presets;
pub mod profiles;
mod save;
mod settings;

pub use diag::{Diag, DiagMsg, Severity, diags_hash, did_you_mean, fnv1a};
pub use keymap::{Binding, ChordMods, ChordSpec, ContextName, ResolvedKeymap};
pub use load::{Loaded, MAX_FILE_BYTES, load};
pub use patch::{KeyId, SettingsPatch, Value, get_value, is_default, set_value};
pub use presets::Presets;
pub use save::{SaveError, save};
pub use settings::{
    Apply, Backend, BoundaryMode, Caps, Confirmations, EchoSettings, EditingSettings,
    IfCoveredPolicy, InterruptPolicy, ItemClick, KeyScope, KeyboardSettings, Layout, ModeDefault,
    MouseSettings, Overlay, Preset, ProfilesSettings, Punct, SCHEMA, SETTINGS, SettingDef,
    SettingType, Settings, SpeechSettings, TextSettings, Verbosity, VerbositySettings, WordLevel,
    defaults_toml, defaults_toml_commented, resolve_language, sections,
};
