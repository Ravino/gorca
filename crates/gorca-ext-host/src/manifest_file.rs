//! The manifest file (`ext.toml`): parse and validate. Never panics; every problem is an error
//! the loader reports and survives (a broken extension never affects the core).
//!
//! ```toml
//! name = "ext-vte"                 # [a-z0-9_-], at most 48 chars; keymap names derive from it
//! backend = "wasm"                 # or "native" (in-tree only, no hang protection)
//! module = "ext_vte.wasm"          # relative to this file (wasm only)
//! hooks = ["on-event", "present", "policy", "nav-units", "command"]
//! capabilities = ["speak", "settings", "fetch:prefetch", "fetch:background"]
//!
//! [matcher]                        # every key is a glob list; omitted = any
//! app_name = ["gnome-terminal*"]
//! toolkit = ["gtk*"]
//! connector = ["atspi"]
//! document_role = []
//! verified_shell = false           # true: only the connector-verified GNOME Shell (never a name)
//!
//! [[keymap]]
//! command = "say_prompt"           # registry name: ext.ext_vte.say_prompt
//! id = 1                           # what the `command` hook receives
//! help = "say the shell prompt"
//! contexts = ["focus"]             # focus | global | browse | review
//! chord = "gorca+shift+p"          # optional default binding, in the first context
//! max_press = 1
//! interrupts = true
//! ```

use gorca_config::ContextName;
use toml_edit::{DocumentMut, Item, Table};

use crate::manifest::{Capabilities, ExtBackend, HookSet, KeymapEntry, Manifest, Matcher};

/// Upper bound on a manifest file, so a hostile one cannot exhaust memory.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
const MAX_KEYMAP: usize = 64;
const MAX_GLOBS: usize = 32;
const MAX_NAME: usize = 48;
const MAX_HELP: usize = 200;

#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum ManifestError {
    #[error("manifest is larger than {MAX_MANIFEST_BYTES} bytes")]
    TooLarge,
    #[error("manifest is not valid TOML: {0}")]
    Toml(String),
    #[error("`{0}` is missing")]
    Missing(&'static str),
    #[error("`{key}`: {why}")]
    Bad { key: String, why: String },
}

fn bad(key: &str, why: &str) -> ManifestError {
    ManifestError::Bad {
        key: key.to_owned(),
        why: why.to_owned(),
    }
}

fn str_list(t: &Table, key: &str, max: usize) -> Result<Vec<String>, ManifestError> {
    let Some(item) = t.get(key) else {
        return Ok(Vec::new());
    };
    let Some(arr) = item.as_array() else {
        return Err(bad(key, "expected an array of strings"));
    };
    if arr.len() > max {
        return Err(bad(key, "too many entries"));
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .filter(|s| s.len() <= 128)
                .ok_or_else(|| bad(key, "expected short strings"))
        })
        .collect()
}

fn context(name: &str) -> Option<ContextName> {
    match name {
        "global" => Some(ContextName::Global),
        "focus" => Some(ContextName::Focus),
        "browse" => Some(ContextName::Browse),
        "review" => Some(ContextName::Review),
        _ => None,
    }
}

fn valid_ident(s: &str, max: usize) -> bool {
    !s.is_empty()
        && s.len() <= max
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
}

impl Manifest {
    /// Parse and validate `text`.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        if text.len() > MAX_MANIFEST_BYTES {
            return Err(ManifestError::TooLarge);
        }
        let doc: DocumentMut = text
            .parse()
            .map_err(|e: toml_edit::TomlError| ManifestError::Toml(e.to_string()))?;
        let root = doc.as_table();

        let name = root
            .get("name")
            .and_then(Item::as_str)
            .ok_or(ManifestError::Missing("name"))?
            .to_owned();
        if !valid_ident(&name, MAX_NAME) {
            return Err(bad("name", "use [a-z0-9_-], at most 48 characters"));
        }
        let backend = match root.get("backend").and_then(Item::as_str) {
            None | Some("wasm") => ExtBackend::Wasm,
            Some("native") => ExtBackend::Native,
            Some(_) => return Err(bad("backend", "expected \"wasm\" or \"native\"")),
        };
        let module = root.get("module").and_then(Item::as_str).map(str::to_owned);
        match (backend, &module) {
            (ExtBackend::Wasm, None) => return Err(ManifestError::Missing("module")),
            (ExtBackend::Wasm, Some(m)) if m.contains("..") || m.starts_with('/') => {
                return Err(bad("module", "must be a relative path without `..`"));
            }
            (ExtBackend::Wasm | ExtBackend::Native, _) => {}
        }

        let mut hooks = HookSet::empty();
        for h in str_list(root, "hooks", 8)? {
            hooks |= match h.as_str() {
                "on-event" => HookSet::ON_EVENT,
                "present" => HookSet::PRESENT,
                "nav-units" => HookSet::NAV_UNITS,
                "command" => HookSet::COMMAND,
                "policy" => HookSet::POLICY,
                _ => return Err(bad("hooks", &format!("unknown hook `{h}`"))),
            };
        }
        let mut capabilities = Capabilities::empty();
        for c in str_list(root, "capabilities", 8)? {
            capabilities |= match c.as_str() {
                "speak" => Capabilities::SPEAK,
                "fetch:prefetch" => Capabilities::FETCH_PREFETCH,
                "fetch:background" => Capabilities::FETCH_BACKGROUND,
                "settings" => Capabilities::SETTINGS,
                _ => return Err(bad("capabilities", &format!("unknown capability `{c}`"))),
            };
        }

        let mut matcher = Matcher::default();
        if let Some(m) = root.get("matcher") {
            let Some(m) = m.as_table() else {
                return Err(bad("matcher", "expected a table"));
            };
            matcher.app_name = str_list(m, "app_name", MAX_GLOBS)?;
            matcher.toolkit = str_list(m, "toolkit", MAX_GLOBS)?;
            matcher.connector = str_list(m, "connector", MAX_GLOBS)?;
            matcher.document_role = str_list(m, "document_role", MAX_GLOBS)?;
            matcher.verified_shell = match m.get("verified_shell") {
                None => false,
                Some(v) => v
                    .as_bool()
                    .ok_or_else(|| bad("matcher.verified_shell", "expected true or false"))?,
            };
        }

        let mut keymap = Vec::new();
        if let Some(k) = root.get("keymap") {
            let Some(arr) = k.as_array_of_tables() else {
                return Err(bad("keymap", "expected [[keymap]] tables"));
            };
            if arr.len() > MAX_KEYMAP {
                return Err(bad("keymap", "too many commands"));
            }
            for t in arr {
                keymap.push(keymap_entry(t)?);
            }
        }
        if !keymap.is_empty() && !hooks.contains(HookSet::COMMAND) {
            return Err(bad("keymap", "commands need the `command` hook"));
        }
        for (i, a) in keymap.iter().enumerate() {
            if keymap[..i]
                .iter()
                .any(|b| b.command == a.command || b.local_id == a.local_id)
            {
                return Err(bad("keymap", "duplicate command name or id"));
            }
        }

        Ok(Manifest {
            name,
            matcher,
            hooks,
            keymap,
            capabilities,
            backend,
            module,
        })
    }
}

fn keymap_entry(t: &Table) -> Result<KeymapEntry, ManifestError> {
    let command = t
        .get("command")
        .and_then(Item::as_str)
        .ok_or(ManifestError::Missing("keymap.command"))?
        .to_owned();
    if !valid_ident(&command.replace('-', "_"), MAX_NAME) || command.contains('.') {
        return Err(bad("keymap.command", "use [a-z0-9_]"));
    }
    let local_id = t
        .get("id")
        .and_then(Item::as_integer)
        .and_then(|i| u32::try_from(i).ok())
        .ok_or(ManifestError::Missing("keymap.id"))?;
    let help = t
        .get("help")
        .and_then(Item::as_str)
        .unwrap_or("extension command")
        .to_owned();
    if help.len() > MAX_HELP {
        return Err(bad("keymap.help", "too long"));
    }
    let mut contexts = Vec::new();
    for c in str_list(t, "contexts", 4)? {
        contexts.push(context(&c).ok_or_else(|| bad("keymap.contexts", "unknown context"))?);
    }
    if contexts.is_empty() {
        contexts.push(ContextName::Focus);
    }
    let chord = t.get("chord").and_then(Item::as_str).map(str::to_owned);
    if chord.as_ref().is_some_and(|c| c.len() > 64) {
        return Err(bad("keymap.chord", "too long"));
    }
    let max_press = match t.get("max_press").and_then(Item::as_integer) {
        None => 1,
        Some(n @ 1..=3) => u8::try_from(n).unwrap_or(1),
        Some(_) => return Err(bad("keymap.max_press", "expected 1 to 3")),
    };
    let interrupts = t.get("interrupts").and_then(Item::as_bool).unwrap_or(true);
    Ok(KeymapEntry {
        command,
        local_id,
        help,
        contexts,
        chord,
        max_press,
        interrupts,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"
name = "ext-x"
module = "x.wasm"
hooks = ["on-event", "command"]
capabilities = ["speak"]
[matcher]
app_name = ["gnome-terminal*"]
[[keymap]]
command = "hello"
id = 1
help = "hi"
contexts = ["focus", "global"]
chord = "gorca+shift+f9"
"#;

    #[test]
    fn parses_a_good_manifest() {
        let m = Manifest::parse(GOOD).unwrap();
        assert_eq!(m.name, "ext-x");
        assert_eq!(m.backend, ExtBackend::Wasm);
        assert_eq!(m.module.as_deref(), Some("x.wasm"));
        assert!(m.hooks.contains(HookSet::ON_EVENT | HookSet::COMMAND));
        assert_eq!(m.capabilities, Capabilities::SPEAK);
        assert_eq!(m.matcher.app_name, ["gnome-terminal*"]);
        assert_eq!(m.keymap.len(), 1);
        assert_eq!(m.keymap[0].contexts.len(), 2);
        assert_eq!(m.keymap[0].chord.as_deref(), Some("gorca+shift+f9"));
        assert_eq!(m.keymap[0].max_press, 1);
    }

    #[test]
    fn rejects_bad_manifests_without_panicking() {
        for (text, why) in [
            ("", "missing name"),
            ("name = \"A B\"\nmodule=\"x.wasm\"", "bad name"),
            ("name = \"x\"", "missing module"),
            ("name = \"x\"\nmodule = \"../x.wasm\"", "path escape"),
            ("name = \"x\"\nmodule = \"/etc/x\"", "absolute path"),
            ("name = \"x\"\nmodule=\"x\"\nbackend=\"jit\"", "bad backend"),
            ("name = \"x\"\nmodule=\"x\"\nhooks=[\"nope\"]", "bad hook"),
            (
                "name = \"x\"\nmodule=\"x\"\ncapabilities=[\"root\"]",
                "bad cap",
            ),
            ("name = [", "toml"),
            (
                "name=\"x\"\nmodule=\"x\"\n[[keymap]]\ncommand=\"a\"\nid=1",
                "keymap needs the command hook",
            ),
            (
                "name=\"x\"\nmodule=\"x\"\nhooks=[\"command\"]\n[[keymap]]\ncommand=\"a\"\nid=1\n[[keymap]]\ncommand=\"b\"\nid=1",
                "duplicate id",
            ),
            (
                "name=\"x\"\nmodule=\"x\"\nhooks=[\"command\"]\n[[keymap]]\ncommand=\"a.b\"\nid=1",
                "dotted command",
            ),
        ] {
            assert!(Manifest::parse(text).is_err(), "{why}");
        }
        assert_eq!(
            Manifest::parse(&"x".repeat(MAX_MANIFEST_BYTES + 1)),
            Err(ManifestError::TooLarge)
        );
    }

    #[test]
    fn verified_shell_is_a_matcher_key() {
        let m = Manifest::parse(
            "name = \"s\"\nmodule = \"s.wasm\"\n[matcher]\nverified_shell = true\n",
        )
        .unwrap();
        assert!(m.matcher.verified_shell);
        assert!(
            Manifest::parse("name = \"s\"\nmodule = \"s.wasm\"\n[matcher]\nverified_shell = 1\n")
                .is_err()
        );
    }

    #[test]
    fn hostile_sizes_are_refused() {
        let entries = (0..=MAX_KEYMAP)
            .map(|i| format!("[[keymap]]\ncommand=\"c{i}\"\nid={i}\n"))
            .collect::<Vec<_>>()
            .concat();
        let many = format!("name=\"x\"\nmodule=\"x\"\nhooks=[\"command\"]\n{entries}");
        assert!(Manifest::parse(&many).is_err(), "too many commands");
        let long_chord = format!(
            "name=\"x\"\nmodule=\"x\"\nhooks=[\"command\"]\n[[keymap]]\ncommand=\"c\"\nid=1\nchord=\"{}\"\n",
            "gorca+".repeat(50)
        );
        assert!(Manifest::parse(&long_chord).is_err(), "chord too long");
        let help = format!(
            "name=\"x\"\nmodule=\"x\"\nhooks=[\"command\"]\n[[keymap]]\ncommand=\"c\"\nid=1\nhelp=\"{}\"\n",
            "h".repeat(MAX_HELP + 1)
        );
        assert!(Manifest::parse(&help).is_err(), "help too long");
        let globs = format!(
            "name=\"x\"\nmodule=\"x\"\n[matcher]\napp_name=[{}]\n",
            vec!["\"a\""; MAX_GLOBS + 1].join(",")
        );
        assert!(Manifest::parse(&globs).is_err(), "too many globs");
    }

    #[test]
    fn native_needs_no_module() {
        let m = Manifest::parse("name = \"n\"\nbackend = \"native\"").unwrap();
        assert_eq!(m.backend, ExtBackend::Native);
        assert!(m.module.is_none());
    }

    #[test]
    fn the_shipped_test_manifest_parses() {
        let text = include_str!("../../../extensions/ext-test/ext.toml");
        let m = Manifest::parse(text).unwrap();
        assert_eq!(m.name, "ext-test");
        assert_eq!(m.keymap.len(), 4);
    }
}
