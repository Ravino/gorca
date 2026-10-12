//! Minimal D-Bus wire codec: exactly what the harness needs, written independently of zbus and of
//! the drain implementations so it can act as ground truth.
//!
//! * encode signals (AT-SPI event shape `siiva{sv}`) and method calls/returns;
//! * parse a message from a byte buffer (either endianness);
//! * client handshake (`AUTH EXTERNAL`, `BEGIN`, `Hello`, `AddMatch`) over a unix socket.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub const METHOD_CALL: u8 = 1;
pub const METHOD_RETURN: u8 = 2;
pub const ERROR: u8 = 3;
pub const SIGNAL: u8 = 4;

/// Largest message the harness will frame (the broker allows 1 GB; the harness never sends more).
pub const MAX_MESSAGE: usize = 128 * 1024 * 1024;

// ---- encoding --------------------------------------------------------------------------------

/// Little-endian encoder over a `Vec<u8>`. Offsets are relative to the start of the vector, so
/// the vector must start at a message boundary (or an 8-aligned body).
#[derive(Debug, Default)]
pub struct Enc {
    pub buf: Vec<u8>,
    /// Offset of the message start inside `buf`; alignment is relative to it.
    pub base: usize,
}

impl Enc {
    pub fn align(&mut self, n: usize) {
        while !(self.buf.len() - self.base).is_multiple_of(n) {
            self.buf.push(0);
        }
    }

    pub fn u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    pub fn u32(&mut self, v: u32) {
        self.align(4);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    pub fn i32(&mut self, v: i32) {
        self.u32(v as u32);
    }

    pub fn u64(&mut self, v: u64) {
        self.align(8);
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    /// `s` or `o`.
    pub fn string(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    /// `g`.
    pub fn sig(&mut self, s: &str) {
        self.buf.push(s.len() as u8);
        self.buf.extend_from_slice(s.as_bytes());
        self.buf.push(0);
    }

    fn patch_u32(&mut self, at: usize, v: u32) {
        self.buf[at..at + 4].copy_from_slice(&v.to_le_bytes());
    }
}

/// Value of the `any_data` variant of an AT-SPI event.
#[derive(Copy, Clone, Debug)]
pub enum Any<'a> {
    Int(i32),
    Str(&'a str),
    /// `(so)`: bus name and object path.
    Ref(&'a str, &'a str),
    /// A variant the drain does not interpret (`d`, 8 bytes).
    Double(f64),
}

/// Encode a `(siiva{sv})` body. `seq`, when present, goes into the properties as `"seq": t`.
pub fn encode_event_body(
    e: &mut Enc,
    detail: &str,
    d1: i32,
    d2: i32,
    any: Any<'_>,
    seq: Option<u64>,
) {
    e.string(detail);
    e.i32(d1);
    e.i32(d2);
    match any {
        Any::Int(v) => {
            e.sig("i");
            e.i32(v);
        }
        Any::Str(s) => {
            e.sig("s");
            e.string(s);
        }
        Any::Ref(bus, path) => {
            e.sig("(so)");
            e.align(8);
            e.string(bus);
            e.string(path);
        }
        Any::Double(d) => {
            e.sig("d");
            e.u64(d.to_bits());
        }
    }
    // a{sv}
    e.u32(0);
    let len_at = e.buf.len() - 4;
    e.align(8);
    let start = e.buf.len();
    if let Some(s) = seq {
        e.align(8);
        e.string("seq");
        e.sig("t");
        e.u64(s);
    }
    let len = (e.buf.len() - start) as u32;
    e.patch_u32(len_at, len);
}

/// Header fields of a message to encode.
#[derive(Copy, Clone, Debug, Default)]
pub struct Fields<'a> {
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
    pub sender: Option<&'a str>,
    pub destination: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub reply_serial: Option<u32>,
    pub error_name: Option<&'a str>,
}

/// Append a complete little-endian message to `out`. `out` must be empty or end on a message
/// boundary.
pub fn encode_message(out: &mut Vec<u8>, mtype: u8, serial: u32, f: &Fields<'_>, body: &[u8]) {
    let base = out.len();
    let mut e = Enc {
        buf: std::mem::take(out),
        base,
    };
    e.u8(b'l');
    e.u8(mtype);
    e.u8(0);
    e.u8(1);
    e.u32(body.len() as u32);
    e.u32(serial);
    e.u32(0);
    let len_at = e.buf.len() - 4;
    e.align(8);
    let start = e.buf.len();
    let field_str = |e: &mut Enc, code: u8, sig: &str, v: &str| {
        e.align(8);
        e.u8(code);
        e.sig(sig);
        e.string(v);
    };
    if let Some(v) = f.path {
        field_str(&mut e, 1, "o", v);
    }
    if let Some(v) = f.interface {
        field_str(&mut e, 2, "s", v);
    }
    if let Some(v) = f.member {
        field_str(&mut e, 3, "s", v);
    }
    if let Some(v) = f.error_name {
        field_str(&mut e, 4, "s", v);
    }
    if let Some(v) = f.reply_serial {
        e.align(8);
        e.u8(5);
        e.sig("u");
        e.u32(v);
    }
    if let Some(v) = f.destination {
        field_str(&mut e, 6, "s", v);
    }
    if let Some(v) = f.sender {
        field_str(&mut e, 7, "s", v);
    }
    if let Some(v) = f.signature {
        e.align(8);
        e.u8(8);
        e.sig("g");
        e.sig(v);
    }
    let flen = (e.buf.len() - start) as u32;
    e.patch_u32(len_at, flen);
    e.align(8);
    e.buf.extend_from_slice(body);
    *out = e.buf;
}

// ---- parsing ---------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct Msg<'a> {
    pub mtype: u8,
    pub flags: u8,
    pub big: bool,
    pub serial: u32,
    pub path: Option<&'a str>,
    pub interface: Option<&'a str>,
    pub member: Option<&'a str>,
    pub sender: Option<&'a str>,
    pub destination: Option<&'a str>,
    pub signature: Option<&'a str>,
    pub reply_serial: Option<u32>,
    pub body: &'a [u8],
}

#[derive(Debug)]
pub enum Parse<'a> {
    /// A message and the number of bytes it occupied.
    Msg(Msg<'a>, usize),
    NeedMore,
    Bad,
}

fn rd_u32(b: &[u8], at: usize, big: bool) -> Option<u32> {
    let x: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(x)
    } else {
        u32::from_le_bytes(x)
    })
}

fn align_up(x: usize, n: usize) -> usize {
    (x + n - 1) & !(n - 1)
}

/// Parse the first message of `buf`.
pub fn parse(buf: &[u8]) -> Parse<'_> {
    if buf.len() < 16 {
        return Parse::NeedMore;
    }
    let big = match buf[0] {
        b'l' => false,
        b'B' => true,
        _ => return Parse::Bad,
    };
    if buf[3] != 1 {
        return Parse::Bad;
    }
    let (Some(body_len), Some(serial), Some(flen)) = (
        rd_u32(buf, 4, big),
        rd_u32(buf, 8, big),
        rd_u32(buf, 12, big),
    ) else {
        return Parse::Bad;
    };
    let (body_len, flen) = (body_len as usize, flen as usize);
    let body_at = align_up(16 + flen, 8);
    let total = body_at.saturating_add(body_len);
    if total > MAX_MESSAGE {
        return Parse::Bad;
    }
    if buf.len() < total {
        return Parse::NeedMore;
    }
    let mut m = Msg {
        mtype: buf[1],
        flags: buf[2],
        big,
        serial,
        path: None,
        interface: None,
        member: None,
        sender: None,
        destination: None,
        signature: None,
        reply_serial: None,
        body: &buf[body_at..total],
    };
    let end = 16 + flen;
    let mut pos = 16;
    while pos < end {
        pos = align_up(pos, 8);
        if pos >= end {
            break;
        }
        let code = buf[pos];
        pos += 1;
        let Some(&slen) = buf.get(pos) else {
            return Parse::Bad;
        };
        let sig = match buf.get(pos + 1..pos + 1 + usize::from(slen)) {
            Some(s) => s,
            None => return Parse::Bad,
        };
        pos += 2 + usize::from(slen);
        let str_at = |pos: &mut usize, wide: bool| -> Option<&str> {
            if wide {
                *pos = align_up(*pos, 4);
                let n = rd_u32(buf, *pos, big)? as usize;
                let s = buf.get(*pos + 4..*pos + 4 + n)?;
                *pos += 5 + n;
                std::str::from_utf8(s).ok()
            } else {
                let n = usize::from(*buf.get(*pos)?);
                let s = buf.get(*pos + 1..*pos + 1 + n)?;
                *pos += 2 + n;
                std::str::from_utf8(s).ok()
            }
        };
        match (code, sig) {
            (1, b"o") => m.path = str_at(&mut pos, true),
            (2, b"s") => m.interface = str_at(&mut pos, true),
            (3, b"s") => m.member = str_at(&mut pos, true),
            (4, b"s") => {
                let _ = str_at(&mut pos, true);
            }
            (6, b"s") => m.destination = str_at(&mut pos, true),
            (7, b"s") => m.sender = str_at(&mut pos, true),
            (8, b"g") => m.signature = str_at(&mut pos, false),
            (5, b"u") => {
                pos = align_up(pos, 4);
                m.reply_serial = rd_u32(buf, pos, big);
                pos += 4;
            }
            _ => return Parse::Bad,
        }
    }
    Parse::Msg(m, total)
}

/// Total length of the message at the start of `buf` if the 16-byte primary header is there.
pub fn peek_len(buf: &[u8]) -> Option<usize> {
    if buf.len() < 16 {
        return None;
    }
    let big = buf[0] == b'B';
    let body = rd_u32(buf, 4, big)? as usize;
    let flen = rd_u32(buf, 12, big)? as usize;
    Some(align_up(16 + flen, 8) + body)
}

// ---- client handshake ------------------------------------------------------------------------

/// `unix:path=...` to a path.
pub fn unix_path(address: &str) -> Option<&str> {
    let rest = address.strip_prefix("unix:")?;
    rest.split(',').find_map(|kv| kv.strip_prefix("path="))
}

/// Numeric uid of this process, from `/proc/self`.
pub fn my_uid() -> u32 {
    std::fs::metadata("/proc/self").map_or(0, |m| m.uid())
}

fn hex(s: &str) -> String {
    use std::fmt::Write as _;
    s.bytes().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

/// An authenticated bus connection that has said `Hello`.
#[derive(Debug)]
pub struct BusConn {
    pub stream: UnixStream,
    pub unique: String,
    /// Bytes read after the last reply and not yet consumed by the caller.
    pub leftover: Vec<u8>,
    next_serial: u32,
}

impl BusConn {
    /// Connect, authenticate with EXTERNAL, `Hello`. Blocking, bounded by `timeout`.
    pub fn connect(address: &str, timeout: Duration) -> Result<Self> {
        let path = unix_path(address).with_context(|| format!("unsupported address {address}"))?;
        let mut s = UnixStream::connect(path).with_context(|| format!("connect {path}"))?;
        s.set_read_timeout(Some(timeout))?;
        s.set_write_timeout(Some(timeout))?;
        let uid = my_uid().to_string();
        s.write_all(format!("\0AUTH EXTERNAL {}\r\n", hex(&uid)).as_bytes())?;
        let mut line = Vec::new();
        let mut b = [0u8; 1];
        while !line.ends_with(b"\r\n") {
            if s.read(&mut b)? == 0 {
                bail!("EOF during authentication");
            }
            line.push(b[0]);
            if line.len() > 512 {
                bail!("authentication reply too long");
            }
        }
        if !line.starts_with(b"OK ") {
            bail!(
                "authentication rejected: {}",
                String::from_utf8_lossy(&line)
            );
        }
        s.write_all(b"BEGIN\r\n")?;
        let mut c = Self {
            stream: s,
            unique: String::new(),
            leftover: Vec::new(),
            next_serial: 1,
        };
        let reply = c.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "Hello",
            None,
            &[],
            timeout,
        )?;
        // Body is a single `s`: u32 length, bytes.
        let n = rd_u32(&reply, 0, false).unwrap_or(0) as usize;
        c.unique = String::from_utf8_lossy(reply.get(4..4 + n).unwrap_or_default()).into_owned();
        Ok(c)
    }

    /// Send a method call and wait for its reply body. Signals that arrive meanwhile stay in
    /// `leftover` order-preserved (they are dropped here: only used before events flow).
    #[allow(clippy::too_many_arguments)]
    pub fn call(
        &mut self,
        dest: &str,
        path: &str,
        iface: &str,
        member: &str,
        sig: Option<&str>,
        body: &[u8],
        timeout: Duration,
    ) -> Result<Vec<u8>> {
        let serial = self.next_serial;
        self.next_serial += 1;
        let mut out = Vec::new();
        encode_message(
            &mut out,
            METHOD_CALL,
            serial,
            &Fields {
                path: Some(path),
                interface: Some(iface),
                member: Some(member),
                destination: Some(dest),
                signature: sig,
                ..Fields::default()
            },
            body,
        );
        self.stream.write_all(&out)?;
        let deadline = Instant::now() + timeout;
        let mut buf = std::mem::take(&mut self.leftover);
        let mut chunk = vec![0u8; 64 * 1024];
        loop {
            loop {
                match parse(&buf) {
                    Parse::Msg(m, used) => {
                        let hit = m.reply_serial == Some(serial);
                        let (mtype, body) = (m.mtype, m.body.to_vec());
                        buf.drain(..used);
                        if hit {
                            self.leftover = buf;
                            if mtype == ERROR {
                                bail!("{member} failed: {}", String::from_utf8_lossy(&body));
                            }
                            return Ok(body);
                        }
                    }
                    Parse::NeedMore => break,
                    Parse::Bad => bail!("garbage from the bus"),
                }
            }
            if Instant::now() >= deadline {
                bail!("{member}: no reply within {timeout:?}");
            }
            let left = deadline.saturating_duration_since(Instant::now());
            self.stream
                .set_read_timeout(Some(left.max(Duration::from_millis(1))))?;
            let n = self.stream.read(&mut chunk).context("reading a reply")?;
            if n == 0 {
                bail!("EOF while waiting for {member}");
            }
            buf.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn add_match(&mut self, rule: &str, timeout: Duration) -> Result<()> {
        let mut e = Enc::default();
        e.string(rule);
        self.call(
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            Some("s"),
            &e.buf,
            timeout,
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_round_trips() {
        let mut body = Enc::default();
        encode_event_body(
            &mut body,
            "insert",
            7,
            -3,
            Any::Ref(":1.5", "/a/b"),
            Some(99),
        );
        let mut out = Vec::new();
        encode_message(
            &mut out,
            SIGNAL,
            42,
            &Fields {
                path: Some("/org/a11y/atspi/accessible/77"),
                interface: Some("org.a11y.atspi.Event.Object"),
                member: Some("TextChanged"),
                sender: Some(":1.9"),
                signature: Some("siiva{sv}"),
                ..Fields::default()
            },
            &body.buf,
        );
        let Parse::Msg(m, used) = parse(&out) else {
            panic!("did not parse");
        };
        assert_eq!(used, out.len());
        assert_eq!(m.mtype, SIGNAL);
        assert_eq!(m.serial, 42);
        assert_eq!(m.path, Some("/org/a11y/atspi/accessible/77"));
        assert_eq!(m.member, Some("TextChanged"));
        assert_eq!(m.sender, Some(":1.9"));
        assert_eq!(m.signature, Some("siiva{sv}"));
        assert_eq!(m.body, &body.buf[..]);
        assert!(matches!(parse(&out[..out.len() - 1]), Parse::NeedMore));
        assert_eq!(peek_len(&out), Some(out.len()));
    }

    #[test]
    fn two_messages_in_one_buffer() {
        let mut out = Vec::new();
        for s in 1..=2 {
            encode_message(
                &mut out,
                SIGNAL,
                s,
                &Fields {
                    path: Some("/x"),
                    interface: Some("a.b"),
                    member: Some("M"),
                    ..Fields::default()
                },
                &[],
            );
        }
        let Parse::Msg(a, n) = parse(&out) else {
            panic!("first");
        };
        assert_eq!(a.serial, 1);
        let Parse::Msg(b, _) = parse(&out[n..]) else {
            panic!("second");
        };
        assert_eq!(b.serial, 2);
    }
}
