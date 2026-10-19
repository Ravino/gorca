// SPDX-License-Identifier: GPL-3.0-or-later
//! Profile file names from untrusted names (ADR-0016 PE-6).
//!
//! Application names come from the application and hosts from the page, so a name is never used
//! as a path: the file id is a slug of at most 48 characters `[a-z0-9-]` plus `-` and eight hex
//! digits of the FNV-1a hash of the whole name. The name goes into the trigger as an escaped
//! literal (`glob::escape_literal`), so an application called `*` matches only itself.

use crate::diag::fnv1a;

/// Longest display name, in characters.
pub const MAX_DISPLAY_CHARS: usize = 128;
/// Longest slug before the hash suffix.
pub const MAX_SLUG: usize = 48;

/// A display name: control characters and bidirectional or invisible formatting marks dropped,
/// at most [`MAX_DISPLAY_CHARS`] characters, whitespace trimmed and collapsed.
pub fn display_name(raw: &str) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for c in raw.chars() {
        let drop = c.is_control()
            || matches!(
                c,
                '\u{200b}'..='\u{200f}'
                    | '\u{202a}'..='\u{202e}'
                    | '\u{2060}'..='\u{2064}'
                    | '\u{2066}'..='\u{2069}'
                    | '\u{feff}'
            );
        if drop {
            continue;
        }
        if c.is_whitespace() {
            if !last_space {
                out.push(' ');
            }
            last_space = true;
        } else {
            out.push(c);
            last_space = false;
        }
        if out.chars().count() >= MAX_DISPLAY_CHARS {
            break;
        }
    }
    out.trim_end().to_owned()
}

/// The file id (stem) for `name`: `<slug>-<8 hex of FNV-1a(name)>`. Always matches
/// `[a-z0-9-]{1,48}-[0-9a-f]{8}` or is the bare hash when nothing of the name is usable.
pub fn file_id(name: &str) -> String {
    let hash = fnv1a(name.as_bytes());
    let mut slug = String::new();
    let mut dash = true;
    for c in name.chars() {
        if slug.len() >= MAX_SLUG {
            break;
        }
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
            dash = false;
        } else if !dash {
            slug.push('-');
            dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    let h = format!("{:08x}", hash & 0xffff_ffff);
    if slug.is_empty() {
        h
    } else {
        format!("{slug}-{h}")
    }
}

/// `file_id` + `.toml`.
pub fn file_name(name: &str) -> String {
    format!("{}.toml", file_id(name))
}

/// A profile file id as the loader accepts it from the directory: `[a-z0-9._-]`, 1 to 100
/// characters, no leading dot.
pub fn is_valid_file_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 100
        && !id.starts_with('.')
        && id.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn safe(id: &str) -> bool {
        is_valid_file_id(id)
            && !id.contains('/')
            && !id.contains('\0')
            && !id.contains("..")
            && !id.starts_with('.')
    }

    #[test]
    fn hostile_names_become_safe_ids() {
        let long = "x".repeat(4096);
        let names = [
            "..",
            "/",
            "../../etc/passwd",
            "a\0b",
            "*",
            "code*",
            long.as_str(),
            "\u{202e}gnp.exe",
            "",
            "   ",
            "\u{5e94}\u{7528}",
            "Firefox",
            "a/b",
            ".hidden",
            "con\r\nsole",
        ];
        for n in names {
            let id = file_id(n);
            assert!(safe(&id), "{n:?} -> {id:?}");
            assert!(id.len() <= MAX_SLUG + 9, "{id}");
            assert!(
                std::path::Path::new(&file_name(n))
                    .extension()
                    .is_some_and(|e| e == "toml")
            );
        }
    }

    #[test]
    fn ids_are_stable_and_distinct_for_distinct_names() {
        assert_eq!(file_id("Firefox"), file_id("Firefox"));
        assert_ne!(file_id("Firefox"), file_id("firefox"));
        // Names that slug to the same text still differ by the hash.
        assert_ne!(file_id("a*b"), file_id("a?b"));
        assert!(file_id("Firefox").starts_with("firefox-"));
        // The empty slug leaves the bare hash.
        assert_eq!(file_id("***").len(), 8);
    }

    #[test]
    fn display_names_lose_controls_and_marks() {
        assert_eq!(display_name("Fire\u{0}fox\n"), "Firefox");
        assert_eq!(display_name("\u{202e}evil\u{202c}"), "evil");
        assert_eq!(display_name("  a   b  "), "a b");
        assert_eq!(
            display_name(&"y".repeat(4096)).chars().count(),
            MAX_DISPLAY_CHARS
        );
        assert_eq!(display_name("\u{200f}\u{200e}"), "");
    }

    #[test]
    fn loader_ids() {
        assert!(is_valid_file_id("firefox-0a1b2c3d"));
        assert!(!is_valid_file_id(".x"));
        assert!(!is_valid_file_id("A"));
        assert!(!is_valid_file_id("a/b"));
        assert!(!is_valid_file_id(""));
    }
}
