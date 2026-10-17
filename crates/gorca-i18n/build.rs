//! Generates `MsgId` (one variant per key of the English catalog) and the embedded catalog
//! sources, so a message id is a compile-time name and a typo cannot reach a user.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn fail(msg: &str) -> ! {
    eprintln!("gorca-i18n build: {msg}");
    std::process::exit(1)
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "msg"))
            .collect(),
        Err(e) => fail(&format!("cannot read {}: {e}", dir.display())),
    };
    v.sort();
    v
}

fn camel(key: &str) -> String {
    let mut out = String::new();
    for part in key.split('-') {
        let mut c = part.chars();
        if let Some(f) = c.next() {
            out.extend(f.to_uppercase());
            out.push_str(c.as_str());
        }
    }
    out
}

fn main() {
    let root = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| fail("no manifest dir")),
    );
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap_or_else(|_| fail("no out dir")));
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=catalog");

    let mut keys: Vec<String> = Vec::new();
    let mut gen_src = String::new();
    for lang in ["en", "ru"] {
        let dir = root.join("catalog").join(lang);
        let _ = writeln!(
            gen_src,
            "pub(crate) const SOURCES_{}: &[&str] = &[",
            lang.to_uppercase()
        );
        for f in files(&dir) {
            println!("cargo:rerun-if-changed={}", f.display());
            let _ = writeln!(gen_src, "    include_str!({:?}),", f.display().to_string());
            if lang != "en" {
                continue;
            }
            let text = std::fs::read_to_string(&f)
                .unwrap_or_else(|e| fail(&format!("{}: {e}", f.display())));
            for (n, line) in text.lines().enumerate() {
                let l = line.trim();
                if l.is_empty() || l.starts_with('#') {
                    continue;
                }
                let Some((k, _)) = l.split_once('=') else {
                    fail(&format!(
                        "{}:{}: expected `key = template`",
                        f.display(),
                        n + 1
                    ));
                };
                let k = k.trim();
                let ok = !k.is_empty()
                    && k.split('-').all(|p| {
                        !p.is_empty()
                            && p.chars()
                                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
                    })
                    && !k.starts_with(|c: char| c.is_ascii_digit());
                if !ok {
                    fail(&format!(
                        "{}:{}: bad key `{k}` (lower-case kebab)",
                        f.display(),
                        n + 1
                    ));
                }
                keys.push(k.to_owned());
            }
        }
        gen_src.push_str("];\n");
    }
    let mut seen = std::collections::BTreeMap::new();
    for k in &keys {
        if let Some(prev) = seen.insert(camel(k), k.clone()) {
            fail(&format!("keys `{prev}` and `{k}` collide as one variant"));
        }
    }
    let mut dup = keys.clone();
    dup.sort();
    if let Some(w) = dup.windows(2).find(|w| w[0] == w[1]) {
        fail(&format!("duplicate key `{}` in the English catalog", w[0]));
    }

    gen_src.push_str(
        "\n/// A message of the catalog. One variant per key of the English catalog; the variant name\n\
/// is the key in CamelCase (`role-list-item` is `RoleListItem`).\n\
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]\n\
#[repr(u16)]\n\
pub enum MsgId {\n",
    );
    for k in &keys {
        let _ = writeln!(gen_src, "    {},", camel(k));
    }
    gen_src.push_str(
        "}\n\nimpl MsgId {\n    /// Every message, in catalog order.\n    pub const ALL: &'static [MsgId] = &[\n",
    );
    for k in &keys {
        let _ = writeln!(gen_src, "        MsgId::{},", camel(k));
    }
    gen_src.push_str(
        "    ];\n\n    /// The catalog key.\n    pub const fn key(self) -> &'static str {\n        match self {\n",
    );
    for k in &keys {
        let _ = writeln!(gen_src, "            MsgId::{} => {k:?},", camel(k));
    }
    gen_src.push_str(
        "        }\n    }\n\n    /// The message of a catalog key.\n    pub fn from_key(key: &str) -> Option<MsgId> {\n        Some(match key {\n",
    );
    for k in &keys {
        let _ = writeln!(gen_src, "            {k:?} => MsgId::{},", camel(k));
    }
    gen_src.push_str("            _ => return None,\n        })\n    }\n}\n");
    if let Err(e) = std::fs::write(out.join("msg_id.rs"), gen_src) {
        fail(&format!("cannot write msg_id.rs: {e}"));
    }
}
