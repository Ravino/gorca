// SPDX-License-Identifier: MIT OR Apache-2.0
//! Extension API: the WIT world, a Rust mirror of its types and traits, and (later) generated
//! bindings.
//!
//! Responsibility (ADR-0001 sections 10 and 11): "WIT files + Rust trait mirror + bindings".
//!
//! > "Extensions depend only on `gorca-ext-api`."  (section 11, rule 5)
//! > "No raw IPC, no file system beyond a settings KV, no network. Limits: 2 ms CPU for
//! > `on-event` and `present`, 20 ms for `command`, 32 MiB per instance; three resets in 60 s
//! > disable the extension for that app."
//!
//! The Rust types mirror `wit/gorca-ext.wit` by hand until `wasmtime::component::bindgen!`
//! generates them (TBD, `wasm-engineer`, lands with `gorca-ext-host`). A test parses the WIT file
//! so it cannot rot unnoticed.
//!
//! This crate's only gorca dependency is the pure leaf `gorca-i18n`: a WASM extension links only
//! this. The terminal word model is kept in step with the editing presenter by shared test
//! vectors (`terminal::WORD_MOTION_VECTORS`), not by a dependency.
//!
//! # Writing an extension (one source, two builds)
//!
//! ```ignore
//! use gorca_ext_api::{export_extension, Hooks, Event, EventVerdict, host};
//!
//! #[derive(Default)]
//! struct MyExt;
//! impl Hooks for MyExt {
//!     fn on_event(&mut self, ev: &Event) -> EventVerdict { EventVerdict::Pass }
//! }
//! export_extension!(MyExt);
//! ```
//!
//! Build natively with `cargo build -p my-ext`, as a component with
//! `cargo build -p my-ext --target wasm32-wasip2 --release`. See `docs/design/extensions.md`.

pub mod api;
pub mod codes;
pub mod host;

pub mod grid;

#[cfg(target_arch = "wasm32")]
pub mod guest;
pub mod terminal;

pub use api::*;

/// Message references and extension catalogs (ADR-0010). An extension never writes user-facing
/// prose: it ships `i18n/<lang>.msg` files under its own name and returns a reference built with
/// [`i18n::msg`] wherever the ABI takes a text; the core resolves it in the UI language.
pub mod i18n {
    pub use gorca_i18n::Lang;
    pub use gorca_i18n::ext::{
        is_reference, register, resolve, unregister_extension as unregister,
    };

    /// A reference to message `key` of extension `ns` with named arguments.
    pub fn msg(ns: &str, key: &str, args: &[(&str, &str)]) -> String {
        gorca_i18n::ext::encode(ns, key, args)
    }
}

/// Export `$ty` (which must be [`Hooks`] and `Default`) as the extension component.
///
/// In the component build (`wasm32`) this exports the WIT `hooks` interface. In a native build it
/// only checks the bounds: the host links `$ty` and calls it through [`Hooks`] directly.
#[cfg(target_arch = "wasm32")]
#[macro_export]
macro_rules! export_extension {
    ($ty:ty) => {
        // The generated export macro takes an identifier, not a type path.
        #[doc(hidden)]
        type GorcaExtAdapter = $crate::guest::Adapter<$ty>;
        $crate::guest::wit::export!(GorcaExtAdapter with_types_in $crate::guest::wit);
    };
}

/// Native build of [`export_extension!`]: bound check only.
#[cfg(not(target_arch = "wasm32"))]
#[macro_export]
macro_rules! export_extension {
    ($ty:ty) => {
        const _: fn() = || {
            fn assert_extension<T: $crate::Hooks + Default + 'static>() {}
            assert_extension::<$ty>();
        };
    };
}

/// The WIT source of the extension world.
pub const WIT_SOURCE: &str = include_str!("../wit/gorca-ext.wit");
/// `package` line of [`WIT_SOURCE`].
///
/// Pre-1.0 versions carry no ABI promise (ADR-0019 V-1): a component built against an older
/// package does not instantiate against a newer host and is rebuilt. 0.3.0 added `term-config.word-level`,
/// `term-config.boundary` and `term-action.cue`. 0.4.0 moved the node and policy records to semantic role codes and `flags` bits, renamed `policy-facts` to `policy-view` and added the `host.read-group` accessor.
pub const WIT_PACKAGE: &str = "gorca:ext@0.4.0";

/// CPU limit for `on-event` and `present` (milliseconds).
pub const LIMIT_HOOK_MS: u32 = 2;
/// CPU limit for `command` (milliseconds).
pub const LIMIT_COMMAND_MS: u32 = 20;
/// CPU limit for the terminal `step` (milliseconds): a screen diff, run inline on the Reader.
pub const LIMIT_TERMINAL_MS: u32 = 16;
/// Memory limit per instance (bytes).
pub const LIMIT_MEMORY_BYTES: usize = 32 * 1024 * 1024;
/// Resets within [`DISABLE_WINDOW_SECS`] that disable the extension for that app.
pub const DISABLE_AFTER_RESETS: u32 = 3;
pub const DISABLE_WINDOW_SECS: u64 = 60;

#[cfg(test)]
mod tests {
    use super::*;
    use wit_parser::Resolve;

    #[test]
    fn wit_file_parses_and_declares_the_extension_world() {
        let mut resolve = Resolve::new();
        let pkg = resolve.push_str("gorca-ext.wit", WIT_SOURCE).unwrap();
        let name = &resolve.packages[pkg].name;
        assert_eq!(format!("{}:{}", name.namespace, name.name), "gorca:ext");
        assert_eq!(
            name.version.as_ref().map(ToString::to_string).as_deref(),
            Some("0.4.0")
        );
        assert!(WIT_SOURCE.contains(WIT_PACKAGE));
        let world = resolve
            .worlds
            .iter()
            .find(|(_, w)| w.name == "extension")
            .map(|(_, w)| w);
        let world = world.unwrap();
        let iface_names = |m: &wit_parser::WorldItem| match m {
            wit_parser::WorldItem::Interface { id, .. } => resolve.interfaces[*id].name.clone(),
            wit_parser::WorldItem::Function(_) | wit_parser::WorldItem::Type { .. } => None,
        };
        let imports: Vec<_> = world.imports.values().filter_map(iface_names).collect();
        let exports: Vec<_> = world.exports.values().filter_map(iface_names).collect();
        assert!(imports.contains(&"host".to_owned()), "{imports:?}");
        assert_eq!(exports, vec!["hooks".to_owned(), "terminal".to_owned()]);
    }

    #[test]
    fn the_role_codes_named_here_are_the_registry_codes() {
        let code = |name: &str| {
            codes::ROLES
                .iter()
                .find(|(n, _)| *n == name)
                .map(|(_, c)| u16::try_from(*c).unwrap())
                .unwrap()
        };
        assert_eq!(role::UNKNOWN, code("unknown"));
        assert_eq!(role::ALERT, code("alert"));
        assert_eq!(role::ALERT_DIALOG, code("alertdialog"));
        assert_eq!(role::DIALOG, code("dialog"));
        assert_eq!(role::METER, code("meter"));
        assert_eq!(role::PROGRESS_BAR, code("progressbar"));
        assert_eq!(role::SLIDER, code("slider"));
        assert_eq!(role::WINDOW, code("window"));
    }

    #[test]
    fn the_wit_declares_role_codes_the_read_group_accessor_and_policy_view() {
        let mut resolve = Resolve::new();
        resolve.push_str("gorca-ext.wit", WIT_SOURCE).unwrap();
        let named = |n: &str| {
            resolve
                .types
                .iter()
                .any(|(_, t)| t.name.as_deref() == Some(n))
        };
        assert!(named("role-code") && named("group-value") && named("policy-view"));
        assert!(
            !named("role") && !named("policy-facts"),
            "the legacy names are gone"
        );
        let (_, host) = resolve
            .interfaces
            .iter()
            .find(|(_, i)| i.name.as_deref() == Some("host"))
            .unwrap();
        assert!(host.functions.contains_key("read-group"));
    }

    #[test]
    fn the_terminal_interface_is_declared_with_step() {
        let mut resolve = Resolve::new();
        resolve.push_str("gorca-ext.wit", WIT_SOURCE).unwrap();
        let (_, iface) = resolve
            .interfaces
            .iter()
            .find(|(_, i)| i.name.as_deref() == Some("terminal"))
            .unwrap();
        assert!(iface.functions.contains_key("step"));
        assert!(iface.functions.contains_key("configure"));
    }

    #[test]
    fn limits_match_the_adr() {
        assert_eq!(LIMIT_HOOK_MS, 2);
        assert_eq!(LIMIT_COMMAND_MS, 20);
        assert_eq!(LIMIT_MEMORY_BYTES, 32 << 20);
    }
}
