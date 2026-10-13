//! `Cache.GetItems` wire format: an encoder for test dumps, the adapter trait for the decoder
//! under test (PW-9, `fetch/items.rs`), and a bounded reference decoder (MA-7).
//!
//! Signature `a((so)(so)(so)iiassusau)`: ref, app, parent, index, child count, interfaces, name,
//! role, description, states. D-Bus little-endian marshalling, alignment relative to the start of
//! the message body.

/// Caps of SPEC section 7 that apply to a decoded dump.
#[derive(Copy, Clone, Debug)]
pub struct DecodeLimits {
    pub max_items: usize,
    pub max_bytes: usize,
    pub max_text: usize,
    pub max_path: usize,
    pub max_ifaces: usize,
    pub max_state_words: usize,
    pub max_child_count: i32,
}

impl Default for DecodeLimits {
    fn default() -> Self {
        Self {
            max_items: 65_536,
            max_bytes: 16 * 1024 * 1024,
            max_text: 2_048,
            max_path: 256,
            max_ifaces: 32,
            max_state_words: 4,
            max_child_count: 65_536,
        }
    }
}

/// One item as the encoder takes it and the decoder returns it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DItem {
    pub bus: String,
    pub path: String,
    pub parent_bus: String,
    pub parent_path: String,
    pub index: i32,
    pub child_count: i32,
    pub ifaces: Vec<String>,
    pub name: String,
    pub role: u32,
    pub desc: String,
    pub states: Vec<u32>,
}

#[derive(Clone, Debug, Default)]
pub struct Decoded {
    pub items: Vec<DItem>,
    /// Items dropped because their own ref lives in another application.
    pub foreign_dropped: u32,
    /// Parent refs dropped because they name another application.
    pub foreign_parents_dropped: u32,
    pub truncated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecodeError {
    /// The reply as a whole is refused (too large, malformed framing).
    Protocol(String),
}

/// The decoder under test. `own_bus` is the unique name of the app that answered.
pub trait ItemsDecoder {
    fn decode(&self, own_bus: &str, bytes: &[u8]) -> Result<Decoded, DecodeError>;
}

// ---- encoder -------------------------------------------------------------------------------

struct W(Vec<u8>);

impl W {
    fn align(&mut self, n: usize) {
        while self.0.len() % n != 0 {
            self.0.push(0);
        }
    }
    fn u32(&mut self, v: u32) {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.align(4);
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    /// `s` and `o`: u32 length, bytes, NUL.
    fn string(&mut self, s: &str) {
        self.u32(u32::try_from(s.len()).unwrap_or(u32::MAX));
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
    }
}

/// Marshal a dump body.
pub fn encode_items(items: &[DItem]) -> Vec<u8> {
    let mut w = W(Vec::new());
    w.u32(0); // patched
    w.align(8);
    let start = w.0.len();
    for it in items {
        w.align(8);
        for (b, p) in [
            (&it.bus, &it.path),
            (&it.bus, &"/org/a11y/atspi/accessible/root".to_owned()),
            (&it.parent_bus, &it.parent_path),
        ] {
            w.align(8);
            w.string(b);
            w.string(p);
        }
        w.i32(it.index);
        w.i32(it.child_count);
        // as
        w.u32(0);
        let a0 = w.0.len();
        for i in &it.ifaces {
            w.string(i);
        }
        let len = u32::try_from(w.0.len() - a0).unwrap_or(0);
        w.0[a0 - 4..a0].copy_from_slice(&len.to_le_bytes());
        w.string(&it.name);
        w.u32(it.role);
        w.string(&it.desc);
        // au
        w.u32(u32::try_from(it.states.len() * 4).unwrap_or(0));
        for s in &it.states {
            w.u32(*s);
        }
    }
    let len = u32::try_from(w.0.len() - start).unwrap_or(0);
    w.0[0..4].copy_from_slice(&len.to_le_bytes());
    w.0
}

// ---- reference decoder ---------------------------------------------------------------------

struct R<'a> {
    b: &'a [u8],
    p: usize,
}

impl R<'_> {
    fn align(&mut self, n: usize) -> Result<(), DecodeError> {
        self.p = self.p.next_multiple_of(n);
        if self.p > self.b.len() {
            return Err(proto("alignment past the end"));
        }
        Ok(())
    }
    fn u32(&mut self) -> Result<u32, DecodeError> {
        self.align(4)?;
        let s = self
            .b
            .get(self.p..self.p + 4)
            .ok_or_else(|| proto("short u32"))?;
        self.p += 4;
        Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.u32()?.to_le_bytes()))
    }
    fn string(&mut self, cap: usize) -> Result<String, DecodeError> {
        let n = self.u32()? as usize;
        let end = self
            .p
            .checked_add(n)
            .ok_or_else(|| proto("length overflow"))?;
        let s = self
            .b
            .get(self.p..end)
            .ok_or_else(|| proto("string past the end"))?;
        if self.b.get(end) != Some(&0) {
            return Err(proto("missing NUL"));
        }
        self.p = end + 1;
        Ok(clip(String::from_utf8_lossy(s).into_owned(), cap))
    }
}

/// Truncate to at most `cap` bytes on a char boundary.
fn clip(mut s: String, cap: usize) -> String {
    if s.len() > cap {
        let mut end = cap;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

fn proto(m: &str) -> DecodeError {
    DecodeError::Protocol(m.to_owned())
}

/// A decoder that implements section 7 literally: bounded, streaming, no panics.
#[derive(Copy, Clone, Debug, Default)]
pub struct RefDecoder {
    pub limits: DecodeLimits,
}

impl ItemsDecoder for RefDecoder {
    fn decode(&self, own_bus: &str, bytes: &[u8]) -> Result<Decoded, DecodeError> {
        let l = &self.limits;
        if bytes.len() > l.max_bytes {
            return Err(proto("reply larger than the cap"));
        }
        let mut r = R { b: bytes, p: 0 };
        let total = r.u32()? as usize;
        r.align(8).or_else(|_| {
            if total == 0 {
                Ok(())
            } else {
                Err(proto("no data"))
            }
        })?;
        let end =
            r.p.checked_add(total)
                .ok_or_else(|| proto("length overflow"))?;
        if end > bytes.len() {
            return Err(proto("array longer than the body"));
        }
        let mut out = Decoded::default();
        while r.p < end {
            if out.items.len() + out.foreign_dropped as usize >= l.max_items {
                out.truncated = true;
                break;
            }
            r.align(8)?;
            let ref1 = (r.string(l.max_path)?, r.string(l.max_path)?);
            r.align(8)?;
            let _app = (r.string(l.max_path)?, r.string(l.max_path)?);
            r.align(8)?;
            let parent = (r.string(l.max_path)?, r.string(l.max_path)?);
            let index = r.i32()?;
            let child_count = r.i32()?.clamp(0, l.max_child_count);
            let alen = r.u32()? as usize;
            let aend =
                r.p.checked_add(alen)
                    .ok_or_else(|| proto("length overflow"))?;
            if aend > end {
                return Err(proto("interface array past the dump"));
            }
            let mut ifaces = Vec::new();
            while r.p < aend {
                let s = r.string(l.max_path)?;
                if ifaces.len() < l.max_ifaces {
                    ifaces.push(s);
                }
            }
            let name = r.string(l.max_text)?;
            let role = r.u32()?;
            let desc = r.string(l.max_text)?;
            let slen = r.u32()? as usize;
            if slen % 4 != 0 || r.p.checked_add(slen).is_none_or(|e| e > end) {
                return Err(proto("bad state array"));
            }
            let mut states = Vec::new();
            for _ in 0..slen / 4 {
                let w = r.u32()?;
                if states.len() < l.max_state_words {
                    states.push(w);
                }
            }
            if ref1.0 != own_bus {
                out.foreign_dropped += 1;
                continue;
            }
            let (parent_bus, parent_path) = if parent.0 == own_bus {
                parent
            } else {
                out.foreign_parents_dropped += 1;
                (String::new(), String::new())
            };
            out.items.push(DItem {
                bus: ref1.0,
                path: ref1.1,
                parent_bus,
                parent_path,
                index,
                child_count,
                ifaces,
                name,
                role,
                desc,
                states,
            });
        }
        Ok(out)
    }
}

/// A deliberately careless decoder: trusts every length. Used to show that the fuzz and the caps
/// tests catch what they are meant to catch.
#[derive(Copy, Clone, Debug, Default)]
pub struct CarelessDecoder;

impl ItemsDecoder for CarelessDecoder {
    #[allow(clippy::indexing_slicing, clippy::cast_possible_truncation)]
    fn decode(&self, own_bus: &str, bytes: &[u8]) -> Result<Decoded, DecodeError> {
        let total = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as usize;
        let mut out = Decoded::default();
        let mut p = 8;
        while p < 8 + total {
            // Unchecked reads: panics on a truncated or inflated dump.
            let n =
                u32::from_le_bytes([bytes[p], bytes[p + 1], bytes[p + 2], bytes[p + 3]]) as usize;
            let s = String::from_utf8_lossy(&bytes[p + 4..p + 4 + n]).into_owned();
            out.items.push(DItem {
                bus: own_bus.to_owned(),
                path: s,
                ..DItem::default()
            });
            p += 4 + n + 1;
            p = p.next_multiple_of(8);
            if out.items.len() > 1 << 24 {
                break;
            }
        }
        Ok(out)
    }
}

/// A plausible dump of `n` items for `bus`, with a chain-and-fan-out shape.
pub fn sample_dump(bus: &str, n: usize) -> Vec<DItem> {
    (0..n)
        .map(|i| DItem {
            bus: bus.to_owned(),
            path: format!("/org/a11y/atspi/accessible/{i}"),
            parent_bus: bus.to_owned(),
            parent_path: format!("/org/a11y/atspi/accessible/{}", i / 4),
            index: i32::try_from(i % 4).unwrap_or(0),
            child_count: i32::try_from(i % 5).unwrap_or(0),
            ifaces: vec![
                "org.a11y.atspi.Accessible".into(),
                "org.a11y.atspi.Component".into(),
            ],
            name: format!("item {i}"),
            role: 43,
            desc: String::new(),
            states: vec![0x0100_0100, 0x40],
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let items = sample_dump(":1.5", 50);
        let bytes = encode_items(&items);
        let d = RefDecoder::default().decode(":1.5", &bytes).unwrap();
        assert_eq!(d.items, items);
        assert_eq!(d.foreign_dropped, 0);
    }

    #[test]
    fn empty_dump_roundtrips() {
        let bytes = encode_items(&[]);
        let d = RefDecoder::default().decode(":1.5", &bytes).unwrap();
        assert!(d.items.is_empty());
    }

    #[test]
    fn foreign_items_and_parents_are_dropped_and_counted() {
        let mut items = sample_dump(":1.5", 10);
        items[3].bus = ":1.9".into();
        items[4].parent_bus = ":1.9".into();
        let d = RefDecoder::default()
            .decode(":1.5", &encode_items(&items))
            .unwrap();
        assert_eq!(d.items.len(), 9);
        assert_eq!(d.foreign_dropped, 1);
        assert_eq!(d.foreign_parents_dropped, 1);
        assert!(d.items.iter().all(|i| i.bus == ":1.5"));
    }

    #[test]
    fn caps_clip_text_counts_and_items() {
        let mut items = sample_dump(":1.5", 30);
        items[0].name = "x".repeat(10_000);
        items[1].child_count = i32::MAX;
        items[2].child_count = -5;
        let dec = RefDecoder {
            limits: DecodeLimits {
                max_items: 20,
                ..DecodeLimits::default()
            },
        };
        let d = dec.decode(":1.5", &encode_items(&items)).unwrap();
        assert_eq!(d.items.len(), 20);
        assert!(d.truncated);
        assert_eq!(d.items[0].name.len(), 2_048);
        assert_eq!(d.items[1].child_count, 65_536);
        assert_eq!(d.items[2].child_count, 0);
    }

    #[test]
    fn framing_lies_are_protocol_errors() {
        let mut b = encode_items(&sample_dump(":1.5", 3));
        b[0..4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(RefDecoder::default().decode(":1.5", &b).is_err());
        assert!(RefDecoder::default().decode(":1.5", &[]).is_err());
    }
}
