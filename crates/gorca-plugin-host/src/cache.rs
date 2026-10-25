// SPDX-License-Identifier: GPL-3.0-or-later
//! The secure `.cwasm` cache (ADR-0027 CD-W1..W3, harness-security decision of 2026-10-24).
//!
//! * The directory is mode 0700, a real directory (not a symlink) and owned by the user; anything
//!   else is refused, never repaired.
//! * Every artifact is `MAGIC ‖ HMAC-SHA256(secret, key ‖ payload) ‖ payload`. The MAC is verified
//!   in constant time **before** `Component::deserialize`; a mismatch removes the entry and the
//!   plugin is recompiled. The per-install secret lives in the same 0700 directory (mode 0600).
//! * The key is `sha256(package sha256 ‖ precompile_compatibility_hash)`, so a CPU change is a
//!   plain miss. Any failed read or deserialize is a miss, never a plugin fault (CD-W2).
//! * Sizes are checked from metadata before a byte is read.
#![allow(unsafe_code)]

use std::fs;
use std::hash::{Hash as _, Hasher};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{
    DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use wasmtime::Engine;
use wasmtime::component::Component;

use crate::mac::{ct_eq, hex, hmac_sha256, random_bytes};

const MAGIC: &[u8; 4] = b"GCW1";
const HEADER: usize = 4 + 32;
/// Largest artifact accepted from disk.
pub const MAX_ARTIFACT_BYTES: u64 = 64 << 20;
const SECRET_FILE: &str = ".secret";

/// Cache failures.
#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    /// The directory is not 0700, not owned by the user, or a symlink.
    #[error("cache directory `{0}` is not a private (0700, user-owned, non-symlink) directory")]
    BadDirectory(PathBuf),
    /// The secret file is missing, short, or has wrong permissions.
    #[error("cache secret is unusable: {0}")]
    BadSecret(&'static str),
    /// The artifact's MAC does not verify.
    #[error("cache artifact `{0}` failed MAC verification")]
    MacMismatch(String),
    /// The artifact is larger than the cap (checked before reading).
    #[error("cache artifact is {0} bytes, over the cap")]
    TooLarge(u64),
    /// Entropy failure.
    #[error("no entropy: {0}")]
    Entropy(getrandom::Error),
    /// I/O error.
    #[error("cache i/o: {0}")]
    Io(#[from] std::io::Error),
    /// wasmtime failed to compile or deserialize.
    #[error("wasmtime: {0}")]
    Wasmtime(String),
}

/// Counters, for tests and diagnostics.
#[derive(Debug, Default)]
pub struct CacheStats {
    /// Verified hits.
    pub hits: AtomicU64,
    /// Misses (compile needed).
    pub misses: AtomicU64,
    /// MAC failures.
    pub tampered: AtomicU64,
    /// `Component::deserialize` calls made (only ever after a verified MAC).
    pub deserializes: AtomicU64,
    /// Deserialize errors treated as misses.
    pub deserialize_errors: AtomicU64,
}

/// A `Hasher` that feeds a SHA-256, to hash `precompile_compatibility_hash()`.
struct Sha256Hasher(Sha256);

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }
    fn finish(&self) -> u64 {
        0
    }
}

/// The user's effective uid without `unsafe`: the owner of `/proc/self`.
fn current_uid() -> std::io::Result<u32> {
    Ok(fs::metadata("/proc/self")?.uid())
}

/// The cache.
#[derive(Debug)]
pub struct CwasmCache {
    dir: PathBuf,
    secret: [u8; 32],
    /// Counters.
    pub stats: CacheStats,
}

impl CwasmCache {
    /// Open (creating mode 0700 if absent) the cache at `dir`.
    pub fn open(dir: &Path) -> Result<Self, CacheError> {
        match fs::symlink_metadata(dir) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(dir)?;
            }
            Err(e) => return Err(e.into()),
        }
        Self::check_dir(dir)?;
        let secret = Self::load_secret(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            secret,
            stats: CacheStats::default(),
        })
    }

    fn check_dir(dir: &Path) -> Result<(), CacheError> {
        let m = fs::symlink_metadata(dir)?;
        let bad = || CacheError::BadDirectory(dir.to_path_buf());
        if !m.file_type().is_dir() {
            return Err(bad());
        }
        if m.uid() != current_uid()? {
            return Err(bad());
        }
        if m.permissions().mode() & 0o7777 != 0o700 {
            return Err(bad());
        }
        Ok(())
    }

    fn load_secret(dir: &Path) -> Result<[u8; 32], CacheError> {
        let path = dir.join(SECRET_FILE);
        match fs::symlink_metadata(&path) {
            Ok(m) => {
                if !m.file_type().is_file()
                    || m.uid() != current_uid()?
                    || m.permissions().mode() & 0o7777 != 0o600
                    || m.len() != 32
                {
                    return Err(CacheError::BadSecret("not a 32-byte 0600 user-owned file"));
                }
                let mut s = [0u8; 32];
                fs::File::open(&path)?.read_exact(&mut s)?;
                Ok(s)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let s = random_bytes::<32>().map_err(CacheError::Entropy)?;
                let mut f = fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&path)?;
                f.write_all(&s)?;
                f.sync_all()?;
                Ok(s)
            }
            Err(e) => Err(e.into()),
        }
    }

    /// The cache key of a package for `engine`.
    #[must_use]
    pub fn key(engine: &Engine, package_sha256: &[u8; 32]) -> String {
        let mut h = Sha256Hasher(Sha256::new());
        h.0.update(package_sha256);
        engine.precompile_compatibility_hash().hash(&mut h);
        hex(&h.0.finalize())
    }

    fn path_of(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.cwasm"))
    }

    fn mac(&self, key: &str, payload: &[u8]) -> [u8; 32] {
        hmac_sha256(&self.secret, &[key.as_bytes(), payload])
    }

    /// Read and verify an artifact's payload. `Ok(None)` is a plain miss.
    pub fn read_verified(&self, key: &str) -> Result<Option<Vec<u8>>, CacheError> {
        Self::check_dir(&self.dir)?;
        let path = self.path_of(key);
        let mut f = match fs::File::open(&path) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let size = f.metadata()?.len();
        if size > MAX_ARTIFACT_BYTES {
            return Err(CacheError::TooLarge(size));
        }
        let mut buf = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
        (&mut f)
            .take(MAX_ARTIFACT_BYTES + 1)
            .read_to_end(&mut buf)?;
        if buf.len() < HEADER || &buf[..4] != MAGIC {
            self.stats.tampered.fetch_add(1, Ordering::Relaxed);
            return Err(CacheError::MacMismatch(key.to_string()));
        }
        let want = self.mac(key, &buf[HEADER..]);
        if !ct_eq(&want, &buf[4..HEADER]) {
            self.stats.tampered.fetch_add(1, Ordering::Relaxed);
            return Err(CacheError::MacMismatch(key.to_string()));
        }
        buf.drain(..HEADER);
        Ok(Some(buf))
    }

    /// Store a payload atomically (temp file 0600 + rename).
    pub fn write(&self, key: &str, payload: &[u8]) -> Result<(), CacheError> {
        Self::check_dir(&self.dir)?;
        let tmp = self.dir.join(format!(
            ".tmp-{}",
            hex(&random_bytes::<8>().map_err(CacheError::Entropy)?)
        ));
        {
            let mut f = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(MAGIC)?;
            f.write_all(&self.mac(key, payload))?;
            f.write_all(payload)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, self.path_of(key))?;
        Ok(())
    }

    /// Remove an entry (best effort).
    pub fn remove(&self, key: &str) {
        let _ = fs::remove_file(self.path_of(key));
    }

    /// Load the component of `wasm` (whose sha256 is `package_sha256`): verified cache hit, or
    /// compile and store. A tampered or undeserializable entry is removed and recompiled; it is
    /// never executed and never a plugin fault.
    pub fn load(
        &self,
        engine: &Engine,
        wasm: &[u8],
        package_sha256: &[u8; 32],
    ) -> Result<Component, CacheError> {
        let key = Self::key(engine, package_sha256);
        match self.read_verified(&key) {
            Ok(Some(payload)) => {
                self.stats.deserializes.fetch_add(1, Ordering::Relaxed);
                // SAFETY: `payload` was produced by this host's `Engine::precompile_component`
                // and has just passed an HMAC-SHA256 check under the per-install secret held in
                // a 0700 user-owned directory, so it is trusted wasmtime output for this exact
                // engine configuration (the key embeds the compatibility hash). wasmtime itself
                // still validates version and ISA flags and returns an error on a mismatch.
                if let Ok(c) = unsafe { Component::deserialize(engine, &payload) } {
                    self.stats.hits.fetch_add(1, Ordering::Relaxed);
                    return Ok(c);
                }
                self.stats
                    .deserialize_errors
                    .fetch_add(1, Ordering::Relaxed);
                self.remove(&key);
            }
            Ok(None) => {}
            Err(CacheError::MacMismatch(_) | CacheError::TooLarge(_)) => self.remove(&key),
            Err(e) => return Err(e),
        }
        self.stats.misses.fetch_add(1, Ordering::Relaxed);
        let bytes = engine
            .precompile_component(wasm)
            .map_err(|e| CacheError::Wasmtime(e.to_string()))?;
        self.write(&key, &bytes)?;
        // SAFETY: `bytes` was just produced by this engine's `precompile_component` in this
        // process; nothing outside the process touched it.
        unsafe { Component::deserialize(engine, &bytes) }
            .map_err(|e| CacheError::Wasmtime(e.to_string()))
    }
}
