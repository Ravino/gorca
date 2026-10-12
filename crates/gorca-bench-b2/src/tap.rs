//! The truth tap: a second, simple blocking reader on the same bus with its own decoder.
//! It sees exactly the broadcasts the drain under test is owed, timestamps them on arrival and
//! folds them per sender, so a run with traffic the harness did not generate itself (the
//! Python fake apps) still has a ground truth.

use std::collections::HashMap;
use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

use anyhow::Result;
use smallvec::SmallVec;

use crate::clock::now_ns;
use crate::model::{Ev, Fold};
use crate::wire::{BusConn, Msg, Parse, SIGNAL, parse};
use crate::workload::fnv1a;

/// Independent implementation of the SPEC 3.2 mapping table over a parsed wire message.
pub fn tap_map(m: &Msg<'_>) -> SmallVec<[Ev; 2]> {
    let mut out = SmallVec::new();
    let (Some(iface), Some(member), Some(path)) = (m.interface, m.member, m.path) else {
        return out;
    };
    let Some(class) = iface.strip_prefix("org.a11y.atspi.Event.") else {
        return out;
    };
    let node = fnv1a(path.as_bytes());
    let head = Head::read(m.body, m.big);
    match (class, member) {
        ("Focus", "Focus") => out.push(Ev::Focus(node)),
        ("Object", "StateChanged") => {
            if let Some(h) = head {
                out.push(match (h.detail, h.d1) {
                    ("focused", 1) => Ev::Focus(node),
                    ("defunct", 1) => Ev::Removed(node),
                    _ => Ev::Props(node, 1 << 4),
                });
            }
        }
        ("Object", "PropertyChange") => {
            if let Some(h) = head {
                let bit = match h.detail {
                    "accessible-name" => 0,
                    "accessible-description" => 1,
                    "accessible-value" => 2,
                    "accessible-role" => 3,
                    _ => 6,
                };
                out.push(Ev::Props(node, 1 << bit));
            }
        }
        ("Object", "ChildrenChanged") => {
            if let Some(h) = head {
                out.push(Ev::Children(node));
                if h.detail.starts_with("remove")
                    && let Any::Ref(p) = h.any
                {
                    out.push(Ev::Removed(fnv1a(p.as_bytes())));
                }
            }
        }
        ("Object", "TextChanged") => {
            if let Some(h) = head {
                let (o, l) = (h.d1.max(0) as u32, h.d2.max(0) as u32);
                if h.detail.starts_with("insert") {
                    out.push(Ev::TextIns(node, o, l));
                } else if h.detail.starts_with("delete") {
                    out.push(Ev::TextDel(node, o, l));
                }
            }
        }
        ("Object", "TextCaretMoved") => {
            if let Some(h) = head {
                out.push(Ev::Caret(node, h.d1.max(0) as u32));
            }
        }
        ("Object", "SelectionChanged" | "TextSelectionChanged") => out.push(Ev::Selection(node)),
        ("Object", "ActiveDescendantChanged") => {
            if let Some(h) = head {
                out.push(Ev::Active(
                    node,
                    match h.any {
                        Any::Ref(p) => Some(fnv1a(p.as_bytes())),
                        _ => None,
                    },
                ));
            }
        }
        ("Object", "Announcement") => {
            if let Some(h) = head {
                let text = match h.any {
                    Any::Str(s) => s.to_owned(),
                    _ => String::new(),
                };
                out.push(Ev::Announce(Some(node), text, h.d1 == 2));
            }
        }
        ("Window", w @ ("Activate" | "Deactivate" | "Create" | "Destroy")) => {
            out.push(Ev::Window(
                node,
                ["Activate", "Deactivate", "Create", "Destroy"]
                    .iter()
                    .position(|x| *x == w)
                    .unwrap_or(0) as u8,
            ));
        }
        ("Document", d @ ("LoadComplete" | "Reload" | "LoadStopped")) => {
            out.push(Ev::Doc(
                node,
                ["LoadComplete", "Reload", "LoadStopped"]
                    .iter()
                    .position(|x| *x == d)
                    .unwrap_or(0) as u8,
            ));
        }
        _ => {}
    }
    out
}

enum Any<'a> {
    Int,
    Str(&'a str),
    Ref(&'a str),
    Other,
}

struct Head<'a> {
    detail: &'a str,
    d1: i32,
    d2: i32,
    any: Any<'a>,
}

impl<'a> Head<'a> {
    fn read(b: &'a [u8], big: bool) -> Option<Self> {
        let u32_at = |pos: usize| -> Option<u32> {
            let x: [u8; 4] = b.get(pos..pos + 4)?.try_into().ok()?;
            Some(if big {
                u32::from_be_bytes(x)
            } else {
                u32::from_le_bytes(x)
            })
        };
        let str_at = |pos: usize| -> Option<(&'a str, usize)> {
            let n = u32_at(pos)? as usize;
            let s = std::str::from_utf8(b.get(pos + 4..pos + 4 + n)?).ok()?;
            (*b.get(pos + 4 + n)? == 0).then_some((s, pos + 5 + n))
        };
        let up = |x: usize, a: usize| (x + a - 1) / a * a;
        let (detail, mut pos) = str_at(0)?;
        pos = up(pos, 4);
        let d1 = u32_at(pos)? as i32;
        let d2 = u32_at(pos + 4)? as i32;
        pos += 8;
        let slen = usize::from(*b.get(pos)?);
        let sig = std::str::from_utf8(b.get(pos + 1..pos + 1 + slen)?).ok()?;
        pos += 2 + slen;
        let any = match sig {
            "i" => Any::Int,
            "s" => Any::Str(str_at(up(pos, 4))?.0),
            "(so)" => {
                let (_, after) = str_at(up(pos, 8))?;
                Any::Ref(str_at(up(after, 4))?.0)
            }
            _ => Any::Other,
        };
        Some(Self {
            detail,
            d1,
            d2,
            any,
        })
    }
}

#[derive(Debug, Default)]
pub struct TapResult {
    /// AT-SPI event signals seen (including ones that map to nothing).
    pub messages: u64,
    pub folds: HashMap<String, Fold>,
    /// `(arrival ns, sender, node)` of every focus event.
    pub focus: Vec<(u64, String, u64)>,
}

#[derive(Debug)]
pub struct Tap {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<TapResult>>,
    pub seen: Arc<std::sync::atomic::AtomicU64>,
}

impl Tap {
    pub fn start(address: &str, rules: &[String]) -> Result<Self> {
        let mut c = BusConn::connect(address, Duration::from_secs(5))?;
        for r in rules {
            c.add_match(r, Duration::from_secs(5))?;
        }
        c.stream.set_read_timeout(Some(Duration::from_millis(50)))?;
        let stop = Arc::new(AtomicBool::new(false));
        let seen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let thread = {
            let (stop, seen) = (Arc::clone(&stop), Arc::clone(&seen));
            std::thread::Builder::new()
                .name("hx-tap".to_owned())
                .spawn(move || {
                    let mut res = TapResult::default();
                    let mut buf = std::mem::take(&mut c.leftover);
                    let mut chunk = vec![0u8; 256 * 1024];
                    loop {
                        let mut off = 0;
                        loop {
                            match parse(&buf[off..]) {
                                Parse::Msg(m, used) => {
                                    if m.mtype == SIGNAL
                                        && m.interface
                                            .is_some_and(|i| i.starts_with("org.a11y.atspi.Event."))
                                    {
                                        let t = now_ns();
                                        res.messages += 1;
                                        seen.fetch_add(1, Ordering::Relaxed);
                                        let sender = m.sender.unwrap_or("").to_owned();
                                        for ev in tap_map(&m) {
                                            if let Ev::Focus(n) = &ev {
                                                res.focus.push((t, sender.clone(), *n));
                                            }
                                            let f = res.folds.entry(sender.clone()).or_default();
                                            f.pushed += 1;
                                            f.apply(&ev);
                                        }
                                    }
                                    off += used;
                                }
                                Parse::NeedMore => break,
                                Parse::Bad => return res,
                            }
                        }
                        buf.drain(..off);
                        if stop.load(Ordering::Acquire) && buf.is_empty() {
                            return res;
                        }
                        match c.stream.read(&mut chunk) {
                            Ok(0) => return res,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                            Err(e)
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) =>
                            {
                                if stop.load(Ordering::Acquire) {
                                    return res;
                                }
                            }
                            Err(_) => return res,
                        }
                    }
                })?
        };
        Ok(Self {
            stop,
            thread: Some(thread),
            seen,
        })
    }

    pub fn stop(mut self) -> TapResult {
        self.stop.store(true, Ordering::Release);
        self.thread
            .take()
            .and_then(|t| t.join().ok())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::{Gen, Mix, Scratch};
    use gorca_model::{AppId, ConnectorId};

    /// The tap's decoder, the generator's expectation and (in `workload.rs`) the reference
    /// mapping in `gorca-dbus` are three implementations of the SPEC 3.2 table; they must agree.
    #[test]
    fn tap_decoder_agrees_with_the_expected_mapping() {
        let app = AppId {
            connector: ConnectorId(0),
            serial: 1,
        };
        let mut sc = Scratch::default();
        for mix in [Mix::Fakeapp, Mix::Terminal, Mix::Realistic, Mix::Lossless] {
            let mut g = Gen::new(21, mix, 64);
            for i in 0..4000u32 {
                let ev = g.next_event();
                let mut out = Vec::new();
                ev.encode(&mut out, &mut sc, i + 1, Some(":1.7"));
                let Parse::Msg(m, _) = parse(&out) else {
                    panic!("parse failed for {ev:?}");
                };
                let got: Vec<Ev> = tap_map(&m).into_iter().collect();
                let want: Vec<Ev> = ev.expected(app).into_iter().collect();
                assert_eq!(got, want, "mismatch for {ev:?}");
            }
        }
    }
}
