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
//! This crate has **no** gorca dependencies on purpose: a WASM extension links only this.

pub mod api;
pub mod terminal;

pub use api::*;

/// The WIT source of the extension world.
pub const WIT_SOURCE: &str = include_str!("../wit/gorca-ext.wit");
/// `package` line of [`WIT_SOURCE`].
pub const WIT_PACKAGE: &str = "gorca:ext@0.1.0";

/// CPU limit for `on-event` and `present` (milliseconds).
pub const LIMIT_HOOK_MS: u32 = 2;
/// CPU limit for `command` (milliseconds).
pub const LIMIT_COMMAND_MS: u32 = 20;
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
            Some("0.1.0")
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
        assert_eq!(exports, vec!["hooks".to_owned()]);
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
