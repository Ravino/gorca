//! Reference mapping from AT-SPI event signals to the closed `NodeEvent` set.
//!
//! This is the table in `docs/bakeoff/b2-ingress/SPEC.md` section 3.2. Every B-2 candidate must
//! produce exactly the same `NodeEvent`s for the same wire message; the bake-off harness checks
//! that against its own independent implementation of the table. The body is never fully
//! deserialised: only the first four fields of the `(siiva{sv})` payload are read, which is the
//! work a raw-framing drain does in its header-peek stage (MB-3).

use std::sync::Arc;

use gorca_model::{
    AppId, CaretOrigin, DocKind, LazyText, LocalId, NodeEvent, NodeKey, Politeness, PropMask,
    TextDelta, WindowKind,
};
use smallvec::SmallVec;

/// Announcement text is cut to this many bytes (at a char boundary): a hostile app must not make
/// us copy megabytes per event (SPEC 3.6).
pub const MAX_ANNOUNCEMENT_BYTES: usize = 4096;

fn clip(s: &str) -> &str {
    if s.len() <= MAX_ANNOUNCEMENT_BYTES {
        return s;
    }
    let mut end = MAX_ANNOUNCEMENT_BYTES;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// A decoded signal header plus the raw body bytes.
#[derive(Copy, Clone, Debug)]
pub struct SignalView<'a> {
    pub interface: &'a str,
    pub member: &'a str,
    pub path: &'a str,
    pub body: &'a [u8],
    pub big_endian: bool,
}

/// The body did not have the AT-SPI event shape (`siiva{sv}` prefix) or was truncated.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Malformed;

/// `LocalId` of an object path: FNV-1a 64 over the path bytes (SPEC 3.1). Total and stable.
pub fn local_id(path: &str) -> LocalId {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    LocalId(h)
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
    big: bool,
}

impl<'a> Reader<'a> {
    fn align(&mut self, n: usize) {
        self.pos = (self.pos + n - 1) & !(n - 1);
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Malformed> {
        let end = self.pos.checked_add(n).ok_or(Malformed)?;
        let s = self.buf.get(self.pos..end).ok_or(Malformed)?;
        self.pos = end;
        Ok(s)
    }

    fn u32(&mut self) -> Result<u32, Malformed> {
        self.align(4);
        let b: [u8; 4] = self.take(4)?.try_into().map_err(|_| Malformed)?;
        Ok(if self.big {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        })
    }

    #[allow(clippy::cast_possible_wrap)]
    fn i32(&mut self) -> Result<i32, Malformed> {
        Ok(self.u32()? as i32)
    }

    /// `s` / `o`: u32 length, bytes, NUL.
    fn string(&mut self) -> Result<&'a str, Malformed> {
        let len = self.u32()? as usize;
        let s = self.take(len)?;
        let nul = self.take(1)?;
        if nul != [0] {
            return Err(Malformed);
        }
        std::str::from_utf8(s).map_err(|_| Malformed)
    }

    /// `g`: u8 length, bytes, NUL.
    fn signature(&mut self) -> Result<&'a str, Malformed> {
        let len = usize::from(*self.take(1)?.first().ok_or(Malformed)?);
        let s = self.take(len)?;
        self.take(1)?;
        std::str::from_utf8(s).map_err(|_| Malformed)
    }
}

enum AnyData<'a> {
    Int,
    Str(&'a str),
    Ref(&'a str),
    Other,
}

struct Head<'a> {
    detail: &'a str,
    d1: i32,
    d2: i32,
    any: AnyData<'a>,
}

fn parse_head<'a>(v: &SignalView<'a>) -> Result<Head<'a>, Malformed> {
    let mut r = Reader {
        buf: v.body,
        pos: 0,
        big: v.big_endian,
    };
    let detail = r.string()?;
    let d1 = r.i32()?;
    let d2 = r.i32()?;
    let sig = r.signature()?;
    let any = match sig {
        "i" => {
            r.i32()?;
            AnyData::Int
        }
        "s" => AnyData::Str(r.string()?),
        "(so)" => {
            r.align(8);
            r.string()?;
            AnyData::Ref(r.string()?)
        }
        _ => AnyData::Other,
    };
    Ok(Head {
        detail,
        d1,
        d2,
        any,
    })
}

/// Longest inserted or deleted text carried with a `TextChanged` event. Typing, a paste of a
/// few words and a deleted word fit; a bulk change travels without text and the consumer
/// re-reads it (ADR-0007 TW-2).
pub const MAX_TEXT_PAYLOAD_BYTES: usize = 1024;

/// The text of a `TextChanged` event (`any_data` is a string for GTK and atk-bridge), copied
/// out of the message so it outlives the drain's buffer. `field` is 0: the bytes are the UTF-8
/// text itself.
fn text_payload(h: &Head<'_>) -> Option<LazyText> {
    match h.any {
        AnyData::Str(s) if !s.is_empty() && s.len() <= MAX_TEXT_PAYLOAD_BYTES => Some(LazyText {
            msg: Arc::from(s.as_bytes()),
            field: 0,
        }),
        AnyData::Str(_) | AnyData::Int | AnyData::Ref(_) | AnyData::Other => None,
    }
}

fn non_negative(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
}

/// Events whose node the core may have to call later, so the connector must remember the object
/// path behind the hashed [`LocalId`]: a focus change, an item that just became selected
/// (spoken for applications whose profile says so), and a caret move (the caret of a rich
/// editable lives on a paragraph the core has never fetched and must read the text of, M3b
/// CE-1; a caret event on a node whose path is already recorded costs one map lookup). Rare,
/// unlike text or property noise.
pub fn needs_path(ev: &NodeEvent) -> Option<NodeKey> {
    match ev {
        NodeEvent::FocusChanged { node } | NodeEvent::CaretMoved { node, .. } => Some(*node),
        NodeEvent::PropertiesChanged { node, mask } if mask.contains(PropMask::SELECTED_NOW) => {
            Some(*node)
        }
        NodeEvent::ActiveDescendantChanged { .. }
        | NodeEvent::PropertiesChanged { .. }
        | NodeEvent::ChildrenChanged { .. }
        | NodeEvent::TextChanged { .. }
        | NodeEvent::SelectionChanged { .. }
        | NodeEvent::Announcement { .. }
        | NodeEvent::WindowEvent { .. }
        | NodeEvent::DocumentEvent { .. }
        | NodeEvent::NodeRemoved { .. } => None,
    }
}

/// The interface of the bulk-cache signals (opt-in match rule, B-1 PW-8).
pub const CACHE_IFACE: &str = "org.a11y.atspi.Cache";

/// `(so)`: a struct (8-aligned) of a bus name and an object path.
fn read_ref<'b>(r: &mut Reader<'b>) -> Result<(&'b str, &'b str), Malformed> {
    r.align(8);
    let bus = r.string()?;
    let path = r.string()?;
    Ok((bus, path))
}

/// Map one `org.a11y.atspi.Cache` signal (ADR-0004 mapping table, B-1 PW-8):
///
/// - `RemoveAccessible (so)` becomes `NodeRemoved{hash(path)}`;
/// - `AddAccessible` becomes `ChildrenChanged{hash(parent path)}`; only the first three refs of
///   the body are read (object, application, parent).
///
/// A foreign bus name in a ref that is read (Remove's ref; Add's object or parent ref) is
/// `Malformed`: no event, and the drain counts it. `sender` is the unique name of the emitting
/// connection, which every honest ref carries.
pub fn map_cache_signal(
    app: AppId,
    sender: &str,
    v: &SignalView<'_>,
    out: &mut SmallVec<[NodeEvent; 2]>,
) -> Result<(), Malformed> {
    let mut r = Reader {
        buf: v.body,
        pos: 0,
        big: v.big_endian,
    };
    match v.member {
        "RemoveAccessible" => {
            let (bus, path) = read_ref(&mut r)?;
            if bus != sender {
                return Err(Malformed);
            }
            out.push(NodeEvent::NodeRemoved {
                node: NodeKey {
                    app,
                    local: local_id(path),
                },
            });
        }
        "AddAccessible" => {
            // The signal body is one struct: align to 8, then the refs follow back to back.
            r.align(8);
            let (obj_bus, _obj) = read_ref(&mut r)?;
            let _app_ref = read_ref(&mut r)?;
            let (parent_bus, parent) = read_ref(&mut r)?;
            if obj_bus != sender || parent_bus != sender {
                return Err(Malformed);
            }
            out.push(NodeEvent::ChildrenChanged {
                node: NodeKey {
                    app,
                    local: local_id(parent),
                },
            });
        }
        _ => {}
    }
    Ok(())
}

/// Map one signal. Appends zero, one or two events to `out`; `Ok` with nothing appended means
/// "valid but not interesting". The caller has already resolved `app` from the sender.
pub fn map_signal(
    app: AppId,
    v: &SignalView<'_>,
    out: &mut SmallVec<[NodeEvent; 2]>,
) -> Result<(), Malformed> {
    let Some(class) = v.interface.strip_prefix("org.a11y.atspi.Event.") else {
        return Ok(());
    };
    let node = NodeKey {
        app,
        local: local_id(v.path),
    };
    match (class, v.member) {
        ("Focus", "Focus") => out.push(NodeEvent::FocusChanged { node }),
        ("Object", "StateChanged") => {
            let h = parse_head(v)?;
            match (h.detail, h.d1) {
                ("focused", 1) => out.push(NodeEvent::FocusChanged { node }),
                ("defunct", 1) => out.push(NodeEvent::NodeRemoved { node }),
                // GNOME Shell's Alt+Tab switcher moves a `selected` pseudo-class between items
                // and emits nothing else (St has no Selection interface).
                ("selected", 1) => out.push(NodeEvent::PropertiesChanged {
                    node,
                    mask: PropMask::STATES | PropMask::SELECTED_NOW,
                }),
                _ => out.push(NodeEvent::PropertiesChanged {
                    node,
                    mask: PropMask::STATES,
                }),
            }
        }
        ("Object", "PropertyChange") => {
            let h = parse_head(v)?;
            let mask = match h.detail {
                "accessible-name" => PropMask::NAME,
                "accessible-description" => PropMask::DESCRIPTION,
                "accessible-role" => PropMask::ROLE,
                "accessible-value" => PropMask::VALUE,
                _ => PropMask::ATTRS,
            };
            out.push(NodeEvent::PropertiesChanged { node, mask });
        }
        ("Object", "ChildrenChanged") => {
            let h = parse_head(v)?;
            out.push(NodeEvent::ChildrenChanged { node });
            if h.detail.starts_with("remove")
                && let AnyData::Ref(child) = h.any
            {
                out.push(NodeEvent::NodeRemoved {
                    node: NodeKey {
                        app,
                        local: local_id(child),
                    },
                });
            }
        }
        ("Object", "TextChanged") => {
            let h = parse_head(v)?;
            let (offset, len) = (non_negative(h.d1), non_negative(h.d2));
            if h.detail.starts_with("insert") {
                out.push(NodeEvent::TextChanged {
                    node,
                    delta: TextDelta::Insert {
                        offset,
                        len,
                        text: text_payload(&h),
                    },
                });
            } else if h.detail.starts_with("delete") {
                out.push(NodeEvent::TextChanged {
                    node,
                    delta: TextDelta::Delete {
                        offset,
                        len,
                        text: text_payload(&h),
                    },
                });
            }
        }
        ("Object", "TextCaretMoved") => {
            let h = parse_head(v)?;
            out.push(NodeEvent::CaretMoved {
                node,
                offset: non_negative(h.d1),
                origin: CaretOrigin::Unknown,
            });
        }
        ("Object", "SelectionChanged" | "TextSelectionChanged") => {
            out.push(NodeEvent::SelectionChanged { node });
        }
        ("Object", "ActiveDescendantChanged") => {
            let h = parse_head(v)?;
            let child = match h.any {
                AnyData::Ref(p) => Some(NodeKey {
                    app,
                    local: local_id(p),
                }),
                AnyData::Int | AnyData::Str(_) | AnyData::Other => None,
            };
            out.push(NodeEvent::ActiveDescendantChanged {
                container: node,
                child,
            });
        }
        ("Object", "Announcement") => {
            let h = parse_head(v)?;
            let text: Arc<str> = match h.any {
                AnyData::Str(s) => Arc::from(clip(s)),
                AnyData::Int | AnyData::Ref(_) | AnyData::Other => Arc::from(""),
            };
            out.push(NodeEvent::Announcement {
                node: Some(node),
                text,
                politeness: if h.d1 == 2 {
                    Politeness::Assertive
                } else {
                    Politeness::Polite
                },
            });
        }
        ("Window", m @ ("Activate" | "Deactivate" | "Create" | "Destroy")) => {
            let kind = match m {
                "Activate" => WindowKind::Activate,
                "Deactivate" => WindowKind::Deactivate,
                "Create" => WindowKind::Create,
                _ => WindowKind::Destroy,
            };
            out.push(NodeEvent::WindowEvent { node, kind });
        }
        ("Document", m @ ("LoadComplete" | "Reload" | "LoadStopped")) => {
            let kind = match m {
                "LoadComplete" => DocKind::LoadComplete,
                "Reload" => DocKind::Reload,
                _ => DocKind::LoadStopped,
            };
            out.push(NodeEvent::DocumentEvent { node, kind });
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
mod tests {
    use super::*;
    use gorca_model::ConnectorId;

    fn app() -> AppId {
        AppId {
            connector: ConnectorId(0),
            serial: 1,
        }
    }

    fn body(detail: &str, d1: i32, d2: i32, any_sig: &str, any: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend_from_slice(&(detail.len() as u32).to_le_bytes());
        b.extend_from_slice(detail.as_bytes());
        b.push(0);
        while b.len() % 4 != 0 {
            b.push(0);
        }
        b.extend_from_slice(&d1.to_le_bytes());
        b.extend_from_slice(&d2.to_le_bytes());
        b.push(any_sig.len() as u8);
        b.extend_from_slice(any_sig.as_bytes());
        b.push(0);
        while b.len() % 4 != 0 {
            b.push(0);
        }
        b.extend_from_slice(any);
        b
    }

    fn run(iface: &str, member: &str, path: &str, body: &[u8]) -> SmallVec<[NodeEvent; 2]> {
        let mut out = SmallVec::new();
        let v = SignalView {
            interface: iface,
            member,
            path,
            body,
            big_endian: false,
        };
        map_signal(app(), &v, &mut out).unwrap();
        out
    }

    #[test]
    fn focus_signal_and_focused_state_agree() {
        let b = body("focused", 1, 0, "i", &0i32.to_le_bytes());
        let a = run("org.a11y.atspi.Event.Object", "StateChanged", "/x/1", &b);
        let c = run("org.a11y.atspi.Event.Focus", "Focus", "/x/1", &b);
        assert!(matches!(a[0], NodeEvent::FocusChanged { .. }));
        assert!(matches!(c[0], NodeEvent::FocusChanged { .. }));
    }

    #[test]
    fn selected_state_becomes_a_selection_hint_only_when_set() {
        let on = body("selected", 1, 0, "i", &0i32.to_le_bytes());
        let off = body("selected", 0, 0, "i", &0i32.to_le_bytes());
        let a = run("org.a11y.atspi.Event.Object", "StateChanged", "/x/1", &on);
        let b = run("org.a11y.atspi.Event.Object", "StateChanged", "/x/1", &off);
        assert!(matches!(
            a[0],
            NodeEvent::PropertiesChanged { mask, .. } if mask.contains(PropMask::SELECTED_NOW)
        ));
        assert!(matches!(
            b[0],
            NodeEvent::PropertiesChanged { mask, .. } if mask == PropMask::STATES
        ));
    }

    #[test]
    fn only_focus_caret_and_new_selection_need_a_remembered_path() {
        let on = body("selected", 1, 0, "i", &0i32.to_le_bytes());
        let off = body("selected", 0, 0, "i", &0i32.to_le_bytes());
        let f = body("focused", 1, 0, "i", &0i32.to_le_bytes());
        let ev = |b: &[u8]| run("org.a11y.atspi.Event.Object", "StateChanged", "/x/1", b);
        assert!(needs_path(&ev(&on)[0]).is_some());
        assert!(needs_path(&ev(&f)[0]).is_some());
        assert!(needs_path(&ev(&off)[0]).is_none());
    }

    #[test]
    fn negative_offsets_clamp_to_zero() {
        let b = body("insert", -5, i32::MIN, "s", b"\x01\0\0\0x\0");
        let e = run("org.a11y.atspi.Event.Object", "TextChanged", "/x/1", &b);
        assert!(matches!(
            e[0],
            NodeEvent::TextChanged {
                delta: TextDelta::Insert {
                    offset: 0,
                    len: 0,
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn text_changed_carries_its_text() {
        let b = body("insert", 3, 1, "s", b"\x01\0\0\0x\0");
        let e = run("org.a11y.atspi.Event.Object", "TextChanged", "/x/1", &b);
        match &e[0] {
            NodeEvent::TextChanged {
                delta:
                    TextDelta::Insert {
                        offset: 3,
                        len: 1,
                        text: Some(t),
                    },
                ..
            } => assert_eq!(&*t.msg, b"x"),
            other => panic!("{other:?}"),
        }
        // An int payload (some bridges) carries no text.
        let b = body("delete", 3, 1, "i", &0i32.to_le_bytes());
        let e = run("org.a11y.atspi.Event.Object", "TextChanged", "/x/1", &b);
        assert!(matches!(
            e[0],
            NodeEvent::TextChanged {
                delta: TextDelta::Delete { text: None, .. },
                ..
            }
        ));
    }

    #[test]
    fn truncated_body_is_malformed() {
        let mut out = SmallVec::new();
        let v = SignalView {
            interface: "org.a11y.atspi.Event.Object",
            member: "TextCaretMoved",
            path: "/x",
            body: &[1, 0, 0],
            big_endian: false,
        };
        assert_eq!(map_signal(app(), &v, &mut out), Err(Malformed));
    }

    #[test]
    fn announcement_text_is_clipped_on_a_char_boundary() {
        let big = "é".repeat(10_000);
        let mut any = Vec::new();
        any.extend_from_slice(&(big.len() as u32).to_le_bytes());
        any.extend_from_slice(big.as_bytes());
        any.push(0);
        let b = body("", 1, 0, "s", &any);
        let e = run("org.a11y.atspi.Event.Object", "Announcement", "/x", &b);
        let NodeEvent::Announcement { text, .. } = &e[0] else {
            panic!("not an announcement");
        };
        assert!(text.len() <= MAX_ANNOUNCEMENT_BYTES && text.len() > MAX_ANNOUNCEMENT_BYTES - 4);
    }

    fn put_ref(b: &mut Vec<u8>, bus: &str, path: &str) {
        while !b.len().is_multiple_of(8) {
            b.push(0);
        }
        for s in [bus, path] {
            b.extend_from_slice(&(s.len() as u32).to_le_bytes());
            b.extend_from_slice(s.as_bytes());
            b.push(0);
            while !b.len().is_multiple_of(4) {
                b.push(0);
            }
        }
    }

    fn cache(
        member: &str,
        body: &[u8],
        sender: &str,
    ) -> Result<SmallVec<[NodeEvent; 2]>, Malformed> {
        let mut out = SmallVec::new();
        let v = SignalView {
            interface: CACHE_IFACE,
            member,
            path: "/org/a11y/atspi/cache",
            body,
            big_endian: false,
        };
        map_cache_signal(app(), sender, &v, &mut out).map(|()| out)
    }

    #[test]
    fn cache_remove_is_a_node_removal() {
        let mut b = Vec::new();
        put_ref(&mut b, ":1.7", "/x/9");
        let out = cache("RemoveAccessible", &b, ":1.7").unwrap();
        assert!(matches!(
            out[0],
            NodeEvent::NodeRemoved { node } if node.local == local_id("/x/9")
        ));
    }

    #[test]
    fn cache_add_dirties_the_parents_children_and_reads_three_refs_only() {
        let mut b = Vec::new();
        put_ref(&mut b, ":1.7", "/x/new");
        put_ref(&mut b, ":1.7", "/org/a11y/atspi/accessible/root");
        put_ref(&mut b, ":1.7", "/x/parent");
        // Whatever follows (indices, name, ...) is never read: garbage is fine.
        b.extend_from_slice(&[0xff; 7]);
        let out = cache("AddAccessible", &b, ":1.7").unwrap();
        assert!(matches!(
            out[0],
            NodeEvent::ChildrenChanged { node } if node.local == local_id("/x/parent")
        ));
    }

    #[test]
    fn foreign_bus_names_in_cache_signals_are_malformed() {
        let mut b = Vec::new();
        put_ref(&mut b, ":1.99", "/x/9");
        assert!(cache("RemoveAccessible", &b, ":1.7").is_err());
        let mut b = Vec::new();
        put_ref(&mut b, ":1.7", "/x/new");
        put_ref(&mut b, ":1.7", "/root");
        put_ref(&mut b, ":1.99", "/x/parent");
        assert!(cache("AddAccessible", &b, ":1.7").is_err());
        let mut b = Vec::new();
        put_ref(&mut b, ":1.99", "/x/new");
        put_ref(&mut b, ":1.7", "/root");
        put_ref(&mut b, ":1.7", "/x/parent");
        assert!(cache("AddAccessible", &b, ":1.7").is_err());
    }

    #[test]
    fn truncated_cache_bodies_are_malformed_and_unknown_members_ignored() {
        assert!(cache("RemoveAccessible", &[1, 0], ":1.7").is_err());
        assert!(cache("AddAccessible", &[], ":1.7").is_err());
        assert!(cache("Unknown", &[], ":1.7").unwrap().is_empty());
    }

    #[test]
    fn unknown_interface_is_ignored() {
        let out = run("org.example.Other", "Foo", "/x", &[]);
        assert!(out.is_empty());
    }
}
