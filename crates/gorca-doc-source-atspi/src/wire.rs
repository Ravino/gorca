//! Hand-written readers for the few AT-SPI reply shapes the block fetch needs.
//!
//! Why not zvariant: a `GetTree` reply for a 100k-node page is ~15 MB of `a((so)a{sv})`.
//! Deserialising it into `Value`s costs several allocations per property; this reader walks the
//! bytes once and allocates only the strings that are kept. All data is untrusted (SL-3): every
//! read is bounds-checked and a malformed reply is an error, never a panic.
//!
//! Only little-endian replies are accepted (libdbus and zbus peers on this platform).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use gorca_doc_source_api::web_attrs;
use gorca_model::AttrKey;
use smallvec::SmallVec;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum WireError {
    #[error("reply truncated at offset {0}")]
    Truncated(usize),
    #[error("string is not valid UTF-8 at offset {0}")]
    BadUtf8(usize),
    #[error("unsupported or malformed signature")]
    BadSignature,
    #[error("big-endian reply")]
    BigEndian,
    #[error("unexpected reply shape: {0}")]
    Shape(&'static str),
}

pub type WireResult<T> = Result<T, WireError>;

/// Bounds-checked cursor over a D-Bus little-endian message body.
#[derive(Debug)]
pub struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, p: 0 }
    }

    pub fn pos(&self) -> usize {
        self.p
    }

    pub fn align(&mut self, n: usize) -> WireResult<()> {
        let np = (self.p + n - 1) & !(n - 1);
        if np > self.b.len() {
            return Err(WireError::Truncated(self.p));
        }
        self.p = np;
        Ok(())
    }

    fn take(&mut self, n: usize) -> WireResult<&'a [u8]> {
        let end = self.p.checked_add(n).ok_or(WireError::Truncated(self.p))?;
        let s = self
            .b
            .get(self.p..end)
            .ok_or(WireError::Truncated(self.p))?;
        self.p = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> WireResult<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn u32(&mut self) -> WireResult<u32> {
        self.align(4)?;
        let s = self.take(4)?;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }

    pub fn i32(&mut self) -> WireResult<i32> {
        Ok(self.u32()?.cast_signed())
    }

    /// `s` or `o`: u32 length, bytes, NUL.
    pub fn str(&mut self) -> WireResult<&'a str> {
        let len = self.u32()? as usize;
        let at = self.p;
        let s = self.take(len)?;
        self.u8()?;
        std::str::from_utf8(s).map_err(|_| WireError::BadUtf8(at))
    }

    /// `g`: u8 length, bytes, NUL.
    pub fn sig(&mut self) -> WireResult<&'a [u8]> {
        let len = self.u8()? as usize;
        let s = self.take(len)?;
        self.u8()?;
        Ok(s)
    }

    /// Start of an array: returns the byte offset where its elements end. `elem_align` is the
    /// alignment of the element type (padding after the length applies even for empty arrays).
    pub fn array_end(&mut self, elem_align: usize) -> WireResult<usize> {
        let len = self.u32()? as usize;
        self.align(elem_align)?;
        let end = self
            .p
            .checked_add(len)
            .ok_or(WireError::Truncated(self.p))?;
        if end > self.b.len() {
            return Err(WireError::Truncated(self.p));
        }
        Ok(end)
    }

    pub fn at_or_past(&self, end: usize) -> bool {
        self.p >= end
    }

    /// Skip one complete value of the single complete type starting at `sig[0]`. Returns the
    /// number of signature bytes consumed.
    pub fn skip(&mut self, sig: &[u8]) -> WireResult<usize> {
        let c = *sig.first().ok_or(WireError::BadSignature)?;
        match c {
            b'y' => {
                self.take(1)?;
                Ok(1)
            }
            b'b' | b'i' | b'u' | b'n' | b'q' => {
                let sz = if matches!(c, b'n' | b'q') { 2 } else { 4 };
                self.align(sz)?;
                self.take(sz)?;
                Ok(1)
            }
            b'x' | b't' | b'd' => {
                self.align(8)?;
                self.take(8)?;
                Ok(1)
            }
            b's' | b'o' => {
                self.str()?;
                Ok(1)
            }
            b'g' => {
                self.sig()?;
                Ok(1)
            }
            b'v' => {
                let s = self.sig()?;
                self.skip(s)?;
                Ok(1)
            }
            b'a' => {
                let elem = &sig[1..];
                let n = single_type_len(elem)?;
                let el = &elem[..n];
                let end = self.array_end(align_of_sig(el[0]))?;
                while self.p < end {
                    self.skip(el)?;
                }
                Ok(1 + n)
            }
            b'(' | b'{' => {
                self.align(8)?;
                let close = if c == b'(' { b')' } else { b'}' };
                let mut i = 1;
                while *sig.get(i).ok_or(WireError::BadSignature)? != close {
                    i += self.skip(&sig[i..])?;
                }
                Ok(i + 1)
            }
            _ => Err(WireError::BadSignature),
        }
    }
}

fn align_of_sig(c: u8) -> usize {
    match c {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'b' | b'i' | b'u' | b's' | b'o' | b'a' => 4,
        _ => 8,
    }
}

/// Length in bytes of the single complete type at the start of `sig`.
fn single_type_len(sig: &[u8]) -> WireResult<usize> {
    let c = *sig.first().ok_or(WireError::BadSignature)?;
    match c {
        b'a' => Ok(1 + single_type_len(&sig[1..])?),
        b'(' | b'{' => {
            let close = if c == b'(' { b')' } else { b'}' };
            let mut i = 1;
            while *sig.get(i).ok_or(WireError::BadSignature)? != close {
                i += single_type_len(&sig[i..])?;
            }
            Ok(i + 1)
        }
        _ => Ok(1),
    }
}

/// Interns attribute keys that are not in the well-known table, and short attribute values.
#[derive(Debug, Default)]
pub struct AttrInterner {
    keys: Mutex<HashMap<Box<str>, AttrKey>>,
}

impl AttrInterner {
    pub fn key(&self, name: &str) -> AttrKey {
        if let Some(k) = web_attrs::well_known(name) {
            return k;
        }
        let Ok(mut m) = self.keys.lock() else {
            return AttrKey(u16::MAX);
        };
        if let Some(k) = m.get(name) {
            return *k;
        }
        let next = web_attrs::FIRST_DYNAMIC.saturating_add(u16::try_from(m.len()).unwrap_or(0));
        let k = AttrKey(next);
        m.insert(name.into(), k);
        k
    }

    /// Reverse lookup for reports and tests.
    pub fn name_of(&self, key: AttrKey) -> Option<String> {
        let m = self.keys.lock().ok()?;
        m.iter()
            .find(|(_, v)| **v == key)
            .map(|(k, _)| k.to_string())
    }
}

/// Identity of an accessible inside one application: the numeric tail of its object path.
/// `0` is `.../root`; a numeric tail `n` is `n + 1`; anything else is rare and kept by the
/// source in a side table (high bit set).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct PathKey(pub u64);

pub const OTHER_PATH_BIT: u64 = 1 << 63;
const PREFIX: &str = "/org/a11y/atspi/accessible/";

impl PathKey {
    pub const ROOT: PathKey = PathKey(0);

    /// `None` when the path is not a numeric tail or `root` (the caller interns it).
    pub fn parse(path: &str) -> Option<PathKey> {
        let tail = path.strip_prefix(PREFIX)?;
        if tail == "root" {
            return Some(PathKey::ROOT);
        }
        if tail.is_empty() || tail.len() > 18 || !tail.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        tail.parse::<u64>().ok().map(|n| PathKey(n + 1))
    }

    /// The object path for a parsed key, `None` for side-table keys.
    pub fn to_path(self) -> Option<String> {
        if self.0 & OTHER_PATH_BIT != 0 {
            None
        } else if self.0 == 0 {
            Some(format!("{PREFIX}root"))
        } else {
            Some(format!("{PREFIX}{}", self.0 - 1))
        }
    }
}

/// One accessible from a `GetTree` reply.
#[derive(Clone, Debug)]
pub struct RawNode {
    pub path: PathKey,
    /// Index into [`TreeReply::buses`]: which unique name owns the node.
    pub bus: u16,
    pub name: Option<Arc<str>>,
    pub child_count: i32,
    pub attrs: SmallVec<[(AttrKey, Arc<str>); 4]>,
    /// Set when the path is not numeric; the real string.
    pub other_path: Option<Box<str>>,
}

#[derive(Clone, Debug, Default)]
pub struct TreeReply {
    pub nodes: Vec<RawNode>,
    pub buses: Vec<Box<str>>,
}

/// Parse a `GetTree` reply body (`a((so)a{sv})`).
pub fn parse_get_tree(body: &[u8], interner: &AttrInterner) -> WireResult<TreeReply> {
    let mut r = Rd::new(body);
    let end = r.array_end(8)?;
    let mut out = TreeReply {
        nodes: Vec::with_capacity((end - r.pos()) / 96),
        buses: Vec::new(),
    };
    // Per-call string pool: tags, roles and class lists repeat thousands of times.
    let mut pool: HashMap<&str, Arc<str>> = HashMap::new();
    let mut other_seq: u64 = 0;
    while r.pos() < end {
        r.align(8)?;
        let bus = r.str()?;
        let path = r.str()?;
        let bus_idx = if let Some(i) = out.buses.iter().position(|b| &**b == bus) {
            i
        } else {
            out.buses.push(bus.into());
            out.buses.len() - 1
        };
        let (key, other_path) = if let Some(k) = PathKey::parse(path) {
            (k, None)
        } else {
            other_seq += 1;
            (
                PathKey(OTHER_PATH_BIT | other_seq),
                Some(Box::<str>::from(path)),
            )
        };
        let mut node = RawNode {
            path: key,
            bus: u16::try_from(bus_idx).unwrap_or(u16::MAX),
            name: None,
            child_count: 0,
            attrs: SmallVec::new(),
            other_path,
        };
        let pend = r.array_end(8)?;
        while r.pos() < pend {
            r.align(8)?;
            let k = r.str()?;
            let sig = r.sig()?;
            match (k, sig) {
                ("Name", b"s") => {
                    let s = r.str()?;
                    if !s.is_empty() {
                        node.name = Some(Arc::from(s));
                    }
                }
                ("ChildCount", b"i") => node.child_count = r.i32()?,
                ("Attributes", b"a{ss}") => {
                    let aend = r.array_end(8)?;
                    while r.pos() < aend {
                        r.align(8)?;
                        let ak = r.str()?;
                        let av = r.str()?;
                        let key = interner.key(ak);
                        let val = if av.len() <= 32 {
                            if let Some(a) = pool.get(av) {
                                Arc::clone(a)
                            } else {
                                let a: Arc<str> = Arc::from(av);
                                pool.insert(av, Arc::clone(&a));
                                a
                            }
                        } else {
                            Arc::from(av)
                        };
                        node.attrs.push((key, val));
                    }
                }
                _ => {
                    r.skip(sig)?;
                }
            }
        }
        out.nodes.push(node);
    }
    Ok(out)
}

/// `GetRole` reply body: one `u`.
pub fn parse_u32(body: &[u8]) -> WireResult<u32> {
    Rd::new(body).u32()
}

/// `GetState` reply body: `au`, folded into a 64-bit set (AT-SPI state numbers 0..=63).
pub fn parse_state(body: &[u8]) -> WireResult<u64> {
    let mut r = Rd::new(body);
    let end = r.array_end(4)?;
    let mut words = [0u32; 2];
    let mut i = 0;
    while r.pos() < end {
        let w = r.u32()?;
        if let Some(slot) = words.get_mut(i) {
            *slot = w;
        }
        i += 1;
    }
    Ok(u64::from(words[0]) | (u64::from(words[1]) << 32))
}

/// `GetChildren` reply body: `a(so)`; returns `(bus, path)` references.
pub fn parse_children(body: &[u8]) -> WireResult<Vec<(String, String)>> {
    let mut r = Rd::new(body);
    let end = r.array_end(8)?;
    let mut v = Vec::with_capacity((end - r.pos()) / 48);
    while r.pos() < end {
        r.align(8)?;
        let bus = r.str()?;
        let path = r.str()?;
        v.push((bus.to_owned(), path.to_owned()));
    }
    Ok(v)
}

/// A reply that is a single `s`.
pub fn parse_string(body: &[u8]) -> WireResult<&str> {
    Rd::new(body).str()
}

/// A reply that is a single `a{ss}` (`GetAttributes`).
pub fn parse_string_map(body: &[u8]) -> WireResult<Vec<(String, String)>> {
    let mut r = Rd::new(body);
    let end = r.array_end(8)?;
    let mut v = Vec::new();
    while r.pos() < end {
        r.align(8)?;
        v.push((r.str()?.to_owned(), r.str()?.to_owned()));
    }
    Ok(v)
}

/// A reply that is a single variant holding `s` (`Properties.Get` of a string property).
pub fn parse_variant_string(body: &[u8]) -> WireResult<&str> {
    let mut r = Rd::new(body);
    let sig = r.sig()?;
    if sig != b"s" {
        return Err(WireError::Shape("variant is not a string"));
    }
    r.str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap as Map;
    use zbus::zvariant::serialized::Context;
    use zbus::zvariant::{ObjectPath, Value, to_bytes};

    type Entry<'a> = ((String, ObjectPath<'a>), Map<String, Value<'a>>);

    fn encode<T: serde::Serialize + zbus::zvariant::Type>(v: &T) -> Vec<u8> {
        let ctx = Context::new_dbus(zbus::zvariant::LE, 0);
        to_bytes(ctx, v).map(|d| d.to_vec()).unwrap_or_default()
    }

    #[test]
    fn get_tree_roundtrip() {
        let mut attrs = Map::new();
        attrs.insert("tag".to_owned(), "div".to_owned());
        attrs.insert("id".to_owned(), "main".to_owned());
        attrs.insert("custom-thing".to_owned(), "x".to_owned());
        let mut p1: Map<String, Value<'_>> = Map::new();
        p1.insert("Name".into(), Value::from("Hello"));
        p1.insert("ChildCount".into(), Value::from(1i32));
        p1.insert("Attributes".into(), Value::from(attrs));
        let mut p2: Map<String, Value<'_>> = Map::new();
        p2.insert("Name".into(), Value::from(""));
        p2.insert("ChildCount".into(), Value::from(0i32));
        p2.insert("Unknown".into(), Value::from(7u64));
        let v: Vec<Entry<'_>> = vec![
            (
                (
                    ":1.7".into(),
                    ObjectPath::try_from("/org/a11y/atspi/accessible/41").unwrap(),
                ),
                p1,
            ),
            (
                (
                    ":1.7".into(),
                    ObjectPath::try_from("/org/a11y/atspi/accessible/weird").unwrap(),
                ),
                p2,
            ),
        ];
        let bytes = encode(&v);
        assert!(!bytes.is_empty());
        let it = AttrInterner::default();
        let t = parse_get_tree(&bytes, &it).unwrap();
        assert_eq!(t.nodes.len(), 2);
        assert_eq!(t.buses.len(), 1);
        assert_eq!(t.nodes[0].path, PathKey(42));
        assert_eq!(t.nodes[0].name.as_deref(), Some("Hello"));
        assert_eq!(t.nodes[0].child_count, 1);
        assert_eq!(t.nodes[0].attrs.len(), 3);
        assert!(
            t.nodes[0]
                .attrs
                .iter()
                .any(|(k, v)| *k == web_attrs::TAG && &**v == "div")
        );
        assert!(t.nodes[1].name.is_none());
        assert_eq!(
            t.nodes[1].other_path.as_deref(),
            Some("/org/a11y/atspi/accessible/weird")
        );
        assert!(t.nodes[1].path.0 & OTHER_PATH_BIT != 0);
        assert!(it.name_of(AttrKey(256)).is_some());
    }

    #[test]
    fn truncated_and_hostile_input_is_an_error_not_a_panic() {
        let it = AttrInterner::default();
        for len in 0..40usize {
            let b = vec![0xffu8; len];
            let _ = parse_get_tree(&b, &it);
            let _ = parse_state(&b);
            let _ = parse_children(&b);
        }
        // Array length pointing far past the end.
        let mut b = Vec::new();
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        b.extend_from_slice(&[0; 4]);
        assert!(parse_get_tree(&b, &it).is_err());
    }

    #[test]
    fn state_and_role_parse() {
        let v: Vec<u32> = vec![1 << 25, 1];
        let b = encode(&v);
        let s = parse_state(&b).unwrap();
        assert_eq!(s & (1 << 25), 1 << 25);
        assert_eq!(s >> 32, 1);
        assert_eq!(parse_u32(&encode(&95u32)).unwrap(), 95);
    }

    #[test]
    fn path_keys() {
        assert_eq!(
            PathKey::parse("/org/a11y/atspi/accessible/root"),
            Some(PathKey::ROOT)
        );
        assert_eq!(
            PathKey::parse("/org/a11y/atspi/accessible/0"),
            Some(PathKey(1))
        );
        assert_eq!(PathKey::parse("/org/a11y/atspi/accessible/12x"), None);
        assert_eq!(
            PathKey(1).to_path().as_deref(),
            Some("/org/a11y/atspi/accessible/0")
        );
        assert_eq!(PathKey::parse("/other"), None);
    }

    #[test]
    fn skip_handles_nested_types() {
        let mut m: Map<String, Value<'_>> = Map::new();
        m.insert("a".into(), Value::from(vec![1u32, 2, 3]));
        let b = encode(&m);
        let mut r = Rd::new(&b);
        let n = r.skip(b"a{sv}").unwrap();
        assert_eq!(n, 5);
        assert_eq!(r.pos(), b.len());
    }
}
