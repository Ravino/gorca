//! Reference mapping from AT-SPI event signals to the closed `NodeEvent` set.
//!
//! This is the table in `docs/bakeoff/b2-ingress/SPEC.md` section 3.2. Every B-2 candidate must
//! produce exactly the same `NodeEvent`s for the same wire message; the bake-off harness checks
//! that against its own independent implementation of the table. The body is never fully
//! deserialised: only the first four fields of the `(siiva{sv})` payload are read, which is the
//! work a raw-framing drain does in its header-peek stage (MB-3).

use std::sync::Arc;

use gorca_model::{
    AppId, CaretOrigin, DocKind, LocalId, NodeEvent, NodeKey, Politeness, PropMask, TextDelta,
    WindowKind,
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

fn non_negative(v: i32) -> u32 {
    u32::try_from(v).unwrap_or(0)
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
                        text: None,
                    },
                });
            } else if h.detail.starts_with("delete") {
                out.push(NodeEvent::TextChanged {
                    node,
                    delta: TextDelta::Delete {
                        offset,
                        len,
                        text: None,
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

    #[test]
    fn unknown_interface_is_ignored() {
        let out = run("org.example.Other", "Foo", "/x", &[]);
        assert!(out.is_empty());
    }
}
