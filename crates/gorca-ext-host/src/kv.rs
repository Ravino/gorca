//! The settings KV: the only persistent state an extension has. One namespace per extension; an
//! extension can neither see nor name another's. Bounded in key count and size. Persistence is
//! the host's job and never happens on a hook's thread: [`SettingsKv::take_dirty`] tells the
//! worker to flush.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use toml_edit::{DocumentMut, Item, Table, value};

pub const MAX_KEY_BYTES: usize = 64;
pub const MAX_VALUE_BYTES: usize = 4096;
pub const MAX_KEYS: usize = 128;

#[derive(Debug, Default)]
pub struct SettingsKv {
    map: Mutex<HashMap<String, BTreeMap<String, String>>>,
    dirty: AtomicBool,
    path: Option<PathBuf>,
}

impl SettingsKv {
    /// In memory only (tests).
    pub fn memory() -> Self {
        Self::default()
    }

    /// Backed by `path`; loads what is there (a missing or broken file is an empty store).
    pub fn with_file(path: PathBuf) -> Self {
        let kv = Self {
            path: Some(path),
            ..Self::default()
        };
        kv.load();
        kv
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, BTreeMap<String, String>>> {
        // A poisoned lock still holds consistent data: every writer finishes its insert before
        // anything can panic.
        self.map
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub fn get(&self, ext: &str, key: &str) -> Option<String> {
        self.lock().get(ext)?.get(key).cloned()
    }

    /// Store `value`. Oversized keys and values and a full namespace are refused (false).
    pub fn set(&self, ext: &str, key: &str, val: &str) -> bool {
        if key.is_empty() || key.len() > MAX_KEY_BYTES || val.len() > MAX_VALUE_BYTES {
            return false;
        }
        let mut map = self.lock();
        let ns = map.entry(ext.to_owned()).or_default();
        if !ns.contains_key(key) && ns.len() >= MAX_KEYS {
            return false;
        }
        if ns.get(key).is_some_and(|v| v == val) {
            return true;
        }
        ns.insert(key.to_owned(), val.to_owned());
        drop(map);
        self.dirty.store(true, Ordering::Release);
        true
    }

    /// True once after a change: the caller flushes.
    pub fn take_dirty(&self) -> bool {
        self.dirty.swap(false, Ordering::AcqRel)
    }

    fn load(&self) {
        let Some(path) = &self.path else { return };
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(doc) = text.parse::<DocumentMut>() else {
            tracing::warn!(path = %path.display(), "extension settings file is not valid TOML; ignored");
            return;
        };
        let mut map = self.lock();
        for (ext, item) in doc.as_table() {
            let Some(t) = item.as_table() else { continue };
            let ns = map.entry(ext.to_owned()).or_default();
            for (k, v) in t {
                if let Some(s) = v.as_str()
                    && k.len() <= MAX_KEY_BYTES
                    && s.len() <= MAX_VALUE_BYTES
                    && ns.len() < MAX_KEYS
                {
                    ns.insert(k.to_owned(), s.to_owned());
                }
            }
        }
    }

    /// Write the store to its file (atomically: temp file, then rename). No-op without a file.
    pub fn flush(&self) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let mut doc = DocumentMut::new();
        {
            let map = self.lock();
            let mut names: Vec<_> = map.keys().collect();
            names.sort();
            for ext in names {
                let mut t = Table::new();
                for (k, v) in &map[ext] {
                    t.insert(k, value(v.as_str()));
                }
                doc.insert(ext, Item::Table(t));
            }
        }
        write_atomic(path, &doc.to_string())
    }
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("toml.tmp");
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn namespaces_are_separate() {
        let kv = SettingsKv::memory();
        assert!(kv.set("a", "k", "1"));
        assert!(kv.set("b", "k", "2"));
        assert_eq!(kv.get("a", "k").as_deref(), Some("1"));
        assert_eq!(kv.get("b", "k").as_deref(), Some("2"));
        assert_eq!(kv.get("c", "k"), None);
    }

    #[test]
    fn bounds_are_enforced() {
        let kv = SettingsKv::memory();
        assert!(!kv.set("a", "", "v"));
        assert!(!kv.set("a", &"k".repeat(MAX_KEY_BYTES + 1), "v"));
        assert!(!kv.set("a", "k", &"v".repeat(MAX_VALUE_BYTES + 1)));
        for i in 0..MAX_KEYS {
            assert!(kv.set("a", &format!("k{i}"), "v"));
        }
        assert!(!kv.set("a", "one-too-many", "v"));
        // Overwriting an existing key is still fine when full.
        assert!(kv.set("a", "k0", "new"));
        assert_eq!(kv.get("a", "k0").as_deref(), Some("new"));
    }

    #[test]
    fn dirty_flag_fires_once_per_change() {
        let kv = SettingsKv::memory();
        assert!(!kv.take_dirty());
        kv.set("a", "k", "1");
        assert!(kv.take_dirty());
        assert!(!kv.take_dirty());
        kv.set("a", "k", "1");
        assert!(!kv.take_dirty(), "same value is not a change");
    }

    #[test]
    fn persists_across_instances() {
        let dir = std::env::temp_dir().join(format!("gorca-kv-{}", std::process::id()));
        let path = dir.join("ext-settings.toml");
        let _ = std::fs::remove_file(&path);
        let kv = SettingsKv::with_file(path.clone());
        kv.set("ext-x", "calls", "3");
        kv.flush().unwrap();
        let again = SettingsKv::with_file(path.clone());
        assert_eq!(again.get("ext-x", "calls").as_deref(), Some("3"));
        let _ = std::fs::remove_dir_all(dir);
    }
}
