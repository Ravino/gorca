// SPDX-License-Identifier: GPL-3.0-or-later
//! Package loading: manifest, size caps, first-party pin list, tier classification
//! (ADR-0026 §2.5.3, §2.7).
//!
//! A package directory holds `manifest.toml` and `plugin.wasm`. Sizes are checked from file
//! metadata before a byte is read.

use std::io::Read as _;
use std::path::Path;

use sha2::{Digest, Sha256};
use toml_edit::DocumentMut;

use crate::trust::{PermError, Perms, TrustTier, effective_grants, parse_perm};
use crate::urlmatch::UrlPattern;

/// Component file cap (§2.5.3).
pub const MAX_COMPONENT_BYTES: u64 = 8 << 20;
/// Whole package cap (§2.5.3).
pub const MAX_PACKAGE_BYTES: u64 = 16 << 20;
/// Manifest cap (this slice's choice; the ADR gives no number, see the M18a-1 report).
pub const MAX_MANIFEST_BYTES: u64 = 64 << 10;
/// Default instance memory (B-7).
pub const DEFAULT_MEMORY_MIB: u32 = 16;
/// Maximum a first-party manifest may ask for (B-7).
pub const MAX_MEMORY_MIB: u32 = 64;
/// Reserved id prefix.
pub const RESERVED_PREFIX: &str = "gorca.";
/// Prefix a first-party plugin under development loads under.
pub const DEV_PREFIX: &str = "dev.";

/// One entry of the compiled-in first-party list.
#[derive(Debug, Clone, Copy)]
pub struct Pin {
    /// Plugin id (reserved).
    pub id: &'static str,
    /// sha256 of `plugin.wasm`.
    pub sha256: [u8; 32],
}

/// The first-party pin list compiled into the daemon. Empty until the first-party packages
/// exist (P-0 `system-scopes` lands in M18a-3); the pin file is generated at release build.
pub const FIRST_PARTY_PINS: &[Pin] = &[];

/// Where a package came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `/usr/lib/gorca/addons/`: eligible for T2 if pinned.
    Installed,
    /// A sideloaded `.gorca-addon`.
    Sideloaded,
    /// `--ext-dev DIR`.
    DevDir,
}

/// The plugin world a component targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum World {
    /// `app-module`.
    AppModule,
    /// `site`.
    Site,
}

/// What the writer may do about a plugin on a focus change (manifest `[plugin] class`, or the
/// older name `priority`). A label is a prior and a hard constraint of the scheduler, never a
/// promise (ADR-0026 section 2.4.7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Priority {
    /// No label: the scheduler starts from the middle prior and learns the rest.
    #[default]
    Unlabelled,
    /// The writer may wait for it, up to the 0.3 ms bound: for work that is cheap enough to be
    /// in the first utterance (the row names of a chat list).
    Interactive,
    /// The writer never waits for it and it never gates a turn: its facts always arrive as
    /// refinements. For anything heavier.
    Refinement,
}

/// A validated manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Plugin id.
    pub id: String,
    /// Version string (informational).
    pub version: String,
    /// World.
    pub world: World,
    /// Declared permissions (not yet granted).
    pub declared: Perms,
    /// Requested instance memory in MiB.
    pub memory_mib: u32,
    /// `[plugin] priority`: whether the writer may wait for this plugin's facts.
    pub priority: Priority,
    /// `[match] apps`: application identities (case-insensitive); empty = any application.
    pub apps: Vec<String>,
    /// `[match] urls`: document URL patterns (structured, see `urlmatch`); empty = no URL
    /// condition.
    pub urls: Vec<UrlPattern>,
    /// `[[watch]]` selectors: the nodes the plugin's `derive` is called for.
    pub watches: Vec<Watch>,
}

/// Longest list in `[match]` and number of `[[watch]]` tables (a manifest is untrusted input).
pub const MAX_MATCH_ENTRIES: usize = 32;
/// Longest single matcher string.
pub const MAX_MATCH_LEN: usize = 256;

/// A `[[watch]]` selector: every condition present must hold. Evaluated natively against the
/// observed (S0) attributes of a node; guest code runs only for nodes a selector matched (PM-2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Watch {
    /// Selector id handed to `derive` in `dirty.watch` (non-zero).
    pub id: u16,
    /// HTML tag (neutral attribute `tag`).
    pub tag: Option<String>,
    /// One class token (neutral attribute `class`, whitespace separated).
    pub class: Option<String>,
    /// Authored ARIA role (neutral attribute `aria-role`).
    pub aria_role: Option<String>,
}

impl Watch {
    /// True when the node's attributes satisfy every condition.
    #[must_use]
    pub fn matches(&self, tag: Option<&str>, class: Option<&str>, aria_role: Option<&str>) -> bool {
        self.tag.as_deref().is_none_or(|t| tag == Some(t))
            && self
                .class
                .as_deref()
                .is_none_or(|c| class.is_some_and(|cl| cl.split_ascii_whitespace().any(|x| x == c)))
            && self
                .aria_role
                .as_deref()
                .is_none_or(|r| aria_role == Some(r))
    }
}

impl Manifest {
    /// True when `app` (and, when URL globs are declared, `url`) satisfy `[match]`.
    #[must_use]
    pub fn matches_scope(&self, app: &str, url: Option<&str>) -> bool {
        let app_ok = self.apps.is_empty() || self.apps.iter().any(|a| a == app);
        let url_ok =
            self.urls.is_empty() || url.is_some_and(|u| self.urls.iter().any(|g| g.matches(u)));
        app_ok && url_ok
    }
}

/// Why a package was refused.
#[derive(Debug, thiserror::Error)]
pub enum PackageError {
    /// A file is over its cap (checked from metadata, before reading).
    #[error("`{what}` is {size} bytes, over the {cap} byte cap")]
    TooLarge {
        /// Which file.
        what: &'static str,
        /// Actual size.
        size: u64,
        /// Cap.
        cap: u64,
    },
    /// I/O error.
    #[error("package i/o: {0}")]
    Io(#[from] std::io::Error),
    /// Manifest is not valid.
    #[error("manifest: {0}")]
    Manifest(String),
    /// A permission is unknown or not allowed.
    #[error(transparent)]
    Perm(#[from] PermError),
    /// The id is reserved and the package is not the pinned first-party one.
    #[error("id `{0}` is reserved")]
    ReservedId(String),
    /// Installed under the first-party directory but the hash differs from the pin.
    #[error("sha256 of `{0}` does not match its first-party pin")]
    PinMismatch(String),
    /// Installed under the first-party directory but not on the pin list.
    #[error("`{0}` is not on the first-party pin list")]
    NotPinned(String),
    /// A dev package must load as `dev.<id>`.
    #[error("dev package id `{0}` must start with `dev.`")]
    DevIdPrefix(String),
}

/// Parse and validate manifest text.
pub fn parse_manifest(text: &str) -> Result<Manifest, PackageError> {
    let doc: DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| PackageError::Manifest(e.to_string()))?;
    let plugin = doc
        .get("plugin")
        .and_then(|i| i.as_table())
        .ok_or_else(|| PackageError::Manifest("missing [plugin]".into()))?;
    let get = |k: &str| plugin.get(k).and_then(|v| v.as_str());
    let id = get("id").ok_or_else(|| PackageError::Manifest("missing plugin.id".into()))?;
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
    {
        return Err(PackageError::Manifest(format!("bad plugin.id `{id}`")));
    }
    let world = match get("world") {
        None | Some("app-module") => World::AppModule,
        Some("site") => World::Site,
        Some(w) => return Err(PackageError::Manifest(format!("unknown world `{w}`"))),
    };
    let mut declared = Perms::empty();
    if let Some(arr) = doc
        .get("permissions")
        .and_then(|t| t.get("grants"))
        .and_then(|v| v.as_array())
    {
        for v in arr {
            let name = v
                .as_str()
                .ok_or_else(|| PackageError::Manifest("permission is not a string".into()))?;
            declared |= parse_perm(name)?;
        }
    }
    let priority = match get("class").or_else(|| get("priority")) {
        None => Priority::Unlabelled,
        Some("interactive") => Priority::Interactive,
        Some("refinement") => Priority::Refinement,
        Some(p) => return Err(PackageError::Manifest(format!("unknown class `{p}`"))),
    };
    let memory_mib = match plugin.get("memory-mib") {
        None => DEFAULT_MEMORY_MIB,
        Some(v) => v
            .as_integer()
            .and_then(|n| u32::try_from(n).ok())
            .filter(|n| (1..=MAX_MEMORY_MIB).contains(n))
            .ok_or_else(|| PackageError::Manifest("memory-mib out of range".into()))?,
    };
    let apps = string_list(&doc, "apps")?;
    let urls = string_list(&doc, "urls")?
        .iter()
        .map(|u| UrlPattern::parse(u).map_err(|e| PackageError::Manifest(e.to_string())))
        .collect::<Result<Vec<_>, _>>()?;
    let watches = parse_watches(&doc)?;
    Ok(Manifest {
        id: id.to_string(),
        version: get("version").unwrap_or("0").to_string(),
        world,
        declared,
        memory_mib,
        priority,
        apps,
        urls,
        watches,
    })
}

fn string_list(doc: &DocumentMut, key: &str) -> Result<Vec<String>, PackageError> {
    let Some(arr) = doc
        .get("match")
        .and_then(|t| t.get(key))
        .and_then(|v| v.as_array())
    else {
        return Ok(Vec::new());
    };
    if arr.len() > MAX_MATCH_ENTRIES {
        return Err(PackageError::Manifest(format!(
            "match.{key} has too many entries"
        )));
    }
    arr.iter()
        .map(|v| {
            v.as_str()
                .filter(|s| !s.is_empty() && s.len() <= MAX_MATCH_LEN)
                .map(str::to_owned)
                .ok_or_else(|| PackageError::Manifest(format!("bad entry in match.{key}")))
        })
        .collect()
}

fn parse_watches(doc: &DocumentMut) -> Result<Vec<Watch>, PackageError> {
    let Some(tables) = doc.get("watch").and_then(|w| w.as_array_of_tables()) else {
        return Ok(Vec::new());
    };
    if tables.len() > MAX_MATCH_ENTRIES {
        return Err(PackageError::Manifest("too many [[watch]] tables".into()));
    }
    let mut out: Vec<Watch> = Vec::new();
    for t in tables {
        let id = t
            .get("id")
            .and_then(toml_edit::Item::as_integer)
            .and_then(|n| u16::try_from(n).ok())
            .filter(|n| *n != 0)
            .ok_or_else(|| PackageError::Manifest("watch.id must be 1..65535".into()))?;
        if out.iter().any(|w| w.id == id) {
            return Err(PackageError::Manifest(format!("duplicate watch.id {id}")));
        }
        let opt = |k: &str| -> Result<Option<String>, PackageError> {
            match t.get(k) {
                None => Ok(None),
                Some(v) => v
                    .as_str()
                    .filter(|s| !s.is_empty() && s.len() <= MAX_MATCH_LEN)
                    .map(|s| Some(s.to_owned()))
                    .ok_or_else(|| PackageError::Manifest(format!("bad watch.{k}"))),
            }
        };
        let w = Watch {
            id,
            tag: opt("tag")?,
            class: opt("class")?,
            aria_role: opt("aria-role")?,
        };
        if w.tag.is_none() && w.class.is_none() && w.aria_role.is_none() {
            return Err(PackageError::Manifest(
                "a [[watch]] needs tag, class or aria-role".into(),
            ));
        }
        out.push(w);
    }
    Ok(out)
}

/// Classify a package into a trust tier. First-party status needs the installed location,
/// the reserved id and the exact pinned hash; anything else claiming a reserved id is refused.
pub fn classify(
    id: &str,
    wasm_sha256: &[u8; 32],
    source: Source,
    user_trusted: bool,
    pins: &[Pin],
) -> Result<TrustTier, PackageError> {
    let pin = pins.iter().find(|p| p.id == id);
    let reserved = pin.is_some() || id.starts_with(RESERVED_PREFIX);
    match source {
        Source::Installed => match pin {
            Some(p) if p.sha256 == *wasm_sha256 => Ok(TrustTier::FirstParty),
            Some(_) => Err(PackageError::PinMismatch(id.to_string())),
            None => Err(PackageError::NotPinned(id.to_string())),
        },
        Source::Sideloaded => {
            if reserved {
                Err(PackageError::ReservedId(id.to_string()))
            } else if user_trusted {
                Ok(TrustTier::TrustedThirdParty)
            } else {
                Ok(TrustTier::Untrusted)
            }
        }
        Source::DevDir => {
            if reserved {
                Err(PackageError::ReservedId(id.to_string()))
            } else if !id.starts_with(DEV_PREFIX) {
                Err(PackageError::DevIdPrefix(id.to_string()))
            } else {
                Ok(TrustTier::Dev)
            }
        }
    }
}

/// A loaded, classified package. The granted permissions are the effective set; default none.
#[derive(Debug)]
pub struct Package {
    /// Validated manifest.
    pub manifest: Manifest,
    /// The component bytes.
    pub wasm: Vec<u8>,
    /// sha256 of `wasm`.
    pub sha256: [u8; 32],
    /// Trust tier.
    pub tier: TrustTier,
    /// Effective permissions.
    pub grants: Perms,
}

fn read_capped(path: &Path, what: &'static str, cap: u64) -> Result<Vec<u8>, PackageError> {
    let f = crate::fsx::open_regular(path)?;
    let size = f.metadata()?.len();
    if size > cap {
        return Err(PackageError::TooLarge { what, size, cap });
    }
    let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    // `take` keeps a file that grows after the metadata check within the cap.
    f.take(cap + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > cap {
        return Err(PackageError::TooLarge {
            what,
            size: buf.len() as u64,
            cap,
        });
    }
    Ok(buf)
}

/// Load `dir/manifest.toml` and `dir/plugin.wasm`.
pub fn load_dir(
    dir: &Path,
    source: Source,
    user_trusted: bool,
    consented: Perms,
    pins: &[Pin],
) -> Result<Package, PackageError> {
    let mtext = read_capped(&dir.join("manifest.toml"), "manifest", MAX_MANIFEST_BYTES)?;
    let wasm = read_capped(&dir.join("plugin.wasm"), "component", MAX_COMPONENT_BYTES)?;
    if (mtext.len() + wasm.len()) as u64 > MAX_PACKAGE_BYTES {
        return Err(PackageError::TooLarge {
            what: "package",
            size: (mtext.len() + wasm.len()) as u64,
            cap: MAX_PACKAGE_BYTES,
        });
    }
    let text = String::from_utf8(mtext)
        .map_err(|_| PackageError::Manifest("manifest is not UTF-8".into()))?;
    let manifest = parse_manifest(&text)?;
    let sha256: [u8; 32] = Sha256::digest(&wasm).into();
    let tier = classify(&manifest.id, &sha256, source, user_trusted, pins)?;
    if manifest.memory_mib > DEFAULT_MEMORY_MIB && tier != TrustTier::FirstParty {
        return Err(PackageError::Manifest(
            "memory-mib above 16 is for first-party plugins only".into(),
        ));
    }
    let grants = effective_grants(manifest.declared, tier, consented);
    Ok(Package {
        manifest,
        wasm,
        sha256,
        tier,
        grants,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const M: &str = "[plugin]\nid = \"example.hello\"\nversion = \"1\"\n";

    #[test]
    fn class_defaults_to_unlabelled_and_rejects_unknown_names() {
        assert_eq!(parse_manifest(M).expect("m").priority, Priority::Unlabelled);
        let i = format!("{M}class = \"interactive\"\n");
        assert_eq!(
            parse_manifest(&i).expect("m").priority,
            Priority::Interactive
        );
        let r = format!("{M}priority = \"refinement\"\n");
        assert_eq!(
            parse_manifest(&r).expect("m").priority,
            Priority::Refinement,
            "the older key name still works"
        );
        let r = format!("{M}class = \"refinement\"\n");
        assert_eq!(
            parse_manifest(&r).expect("m").priority,
            Priority::Refinement
        );
        let bad = format!("{M}priority = \"urgent\"\n");
        assert!(parse_manifest(&bad).is_err());
    }

    #[test]
    fn match_and_watch_tables_parse() {
        let t = format!(
            "{M}[match]\napps = [\"firefox\"]\nurls = [\"https://web.telegram.org/a/*\"]\n\
             [[watch]]\nid = 1\nclass = \"Message\"\n[[watch]]\nid = 2\ntag = \"a\"\nclass = \"chat\"\n"
        );
        let m = parse_manifest(&t).expect("manifest");
        assert!(m.matches_scope("firefox", Some("https://web.telegram.org/a/#1")));
        assert!(!m.matches_scope("firefox", Some("https://web.telegram.org/k/")));
        assert!(!m.matches_scope("firefox", None));
        assert!(!m.matches_scope("gedit", Some("https://web.telegram.org/a/")));
        assert!(!m.matches_scope("firefox", Some("https://web.telegram.org.evil.com/a/")));
        assert!(
            parse_manifest(&format!(
                "{M}[match]\nurls = [\"https://web.telegram.org*\"]\n"
            ))
            .is_err()
        );
        assert_eq!(m.watches.len(), 2);
        assert!(m.watches[0].matches(Some("div"), Some("Message x"), None));
        assert!(!m.watches[0].matches(Some("div"), Some("Messages"), None));
        assert!(m.watches[1].matches(Some("a"), Some("foo chat"), None));
        assert!(!m.watches[1].matches(Some("div"), Some("chat"), None));
        for bad in [
            "[[watch]]\nid = 0\nclass = \"x\"\n",
            "[[watch]]\nid = 1\n",
            "[match]\napps = [1]\n",
        ] {
            assert!(parse_manifest(&format!("{M}{bad}")).is_err(), "{bad}");
        }
    }

    #[test]
    fn default_grants_are_empty() {
        let m = parse_manifest(M).expect("manifest");
        assert_eq!(m.declared, Perms::empty());
        assert_eq!(m.world, World::AppModule);
    }

    #[test]
    fn declared_permissions_parse_and_bad_ones_fail() {
        let ok = format!("{M}[permissions]\ngrants = [\"read-names\", \"speak\"]\n");
        assert_eq!(
            parse_manifest(&ok).expect("ok").declared,
            Perms::READ_NAMES | Perms::SPEAK
        );
        let bad = format!("{M}[permissions]\ngrants = [\"net:any\"]\n");
        assert!(matches!(parse_manifest(&bad), Err(PackageError::Perm(_))));
        assert!(parse_manifest("[plugin]\nid = \"Bad Id\"\n").is_err());
    }

    #[test]
    fn pin_rules() {
        let h = [7u8; 32];
        let pins = [Pin {
            id: "gorca.system-scopes",
            sha256: h,
        }];
        assert_eq!(
            classify("gorca.system-scopes", &h, Source::Installed, false, &pins).expect("t2"),
            TrustTier::FirstParty
        );
        assert!(matches!(
            classify(
                "gorca.system-scopes",
                &[8; 32],
                Source::Installed,
                false,
                &pins
            ),
            Err(PackageError::PinMismatch(_))
        ));
        assert!(matches!(
            classify("other", &h, Source::Installed, false, &pins),
            Err(PackageError::NotPinned(_))
        ));
        // Reserved ids are refused from every other source, even with the right hash.
        for s in [Source::Sideloaded, Source::DevDir] {
            assert!(matches!(
                classify("gorca.system-scopes", &h, s, true, &pins),
                Err(PackageError::ReservedId(_))
            ));
            assert!(matches!(
                classify("gorca.anything", &h, s, true, &pins),
                Err(PackageError::ReservedId(_))
            ));
        }
        assert_eq!(
            classify("x.y", &h, Source::Sideloaded, false, &pins).expect("t3"),
            TrustTier::Untrusted
        );
        assert_eq!(
            classify("x.y", &h, Source::Sideloaded, true, &pins).expect("t3t"),
            TrustTier::TrustedThirdParty
        );
        assert_eq!(
            classify("dev.y", &h, Source::DevDir, false, &pins).expect("dev"),
            TrustTier::Dev
        );
        assert!(matches!(
            classify("y", &h, Source::DevDir, false, &pins),
            Err(PackageError::DevIdPrefix(_))
        ));
    }

    #[test]
    fn oversize_component_is_refused_before_reading() {
        let d = tempfile::tempdir().expect("tmp");
        fs::write(d.path().join("manifest.toml"), M).expect("w");
        let f = fs::File::create(d.path().join("plugin.wasm")).expect("c");
        f.set_len(MAX_COMPONENT_BYTES + 1).expect("sparse");
        let e = load_dir(d.path(), Source::Sideloaded, false, Perms::empty(), &[]);
        assert!(matches!(
            e,
            Err(PackageError::TooLarge {
                what: "component",
                ..
            })
        ));
    }

    #[test]
    fn sideloaded_package_gets_no_grants_by_default() {
        let d = tempfile::tempdir().expect("tmp");
        fs::write(
            d.path().join("manifest.toml"),
            format!("{M}[permissions]\ngrants = [\"speak\", \"pointer\"]\n"),
        )
        .expect("w");
        fs::write(d.path().join("plugin.wasm"), b"\0asm").expect("w");
        let p = load_dir(d.path(), Source::Sideloaded, false, Perms::empty(), &[]).expect("load");
        assert_eq!(p.tier, TrustTier::Untrusted);
        // `pointer` is dropped for third parties, `speak` kept as declared.
        assert_eq!(p.grants, Perms::SPEAK);
    }

    #[test]
    fn fifo_and_symlink_are_refused_without_blocking() {
        let d = tempfile::tempdir().expect("tmp");
        fs_write(&d, "manifest.toml", M.as_bytes());
        let st = std::process::Command::new("mkfifo")
            .arg(d.path().join("plugin.wasm"))
            .status()
            .expect("mkfifo");
        assert!(st.success());
        let (tx, rx) = std::sync::mpsc::channel();
        let p = d.path().to_path_buf();
        std::thread::spawn(move || {
            let r = load_dir(&p, Source::Sideloaded, false, Perms::empty(), &[]);
            let _ = tx.send(r.is_err());
        });
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(true),
            "a FIFO must be refused, not blocked on"
        );
        // A symlink (even to a good file) is refused too.
        std::fs::remove_file(d.path().join("plugin.wasm")).expect("rm");
        fs_write(&d, "real.wasm", b"\0asm");
        std::os::unix::fs::symlink(d.path().join("real.wasm"), d.path().join("plugin.wasm"))
            .expect("ln");
        assert!(load_dir(d.path(), Source::Sideloaded, false, Perms::empty(), &[]).is_err());
    }

    fn fs_write(d: &tempfile::TempDir, name: &str, b: &[u8]) {
        std::fs::write(d.path().join(name), b).expect("w");
    }
}
