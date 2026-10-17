// SPDX-License-Identifier: GPL-3.0-or-later
//! D-Bus wire framing for the raw drain: the fixed header, the header-field array and the three
//! method calls the drain makes (`Hello`, `AddMatch`). No body is decoded here.

/// A message larger than this is skipped while streaming and counted as malformed (SPEC 3.6,
/// B-5); the D-Bus limit is 128 MiB.
pub(super) const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
const MAX_WIRE_MESSAGE: usize = 128 * 1024 * 1024;
const MAX_FIELDS: usize = 64 * 1024 * 1024;

pub(super) const METHOD_CALL: u8 = 1;
pub(super) const METHOD_RETURN: u8 = 2;
pub(super) const ERROR: u8 = 3;
pub(super) const SIGNAL: u8 = 4;

/// The fixed 16-byte header.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Frame {
    pub(super) big: bool,
    pub(super) mtype: u8,
    pub(super) fields_len: usize,
    /// Offset of the body from the start of the message.
    pub(super) body_at: usize,
    /// Length of the whole message.
    pub(super) total: usize,
}

/// The framing is lost (not a D-Bus stream): the connection cannot continue.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct BadFrame;

fn u32_at(b: &[u8], at: usize, big: bool) -> Option<u32> {
    let s: [u8; 4] = b.get(at..at + 4)?.try_into().ok()?;
    Some(if big {
        u32::from_be_bytes(s)
    } else {
        u32::from_le_bytes(s)
    })
}

/// Parse the fixed header at the start of `buf` (at least 16 bytes).
pub(super) fn peek(buf: &[u8]) -> Result<Frame, BadFrame> {
    let big = match buf.first() {
        Some(b'l') => false,
        Some(b'B') => true,
        _ => return Err(BadFrame),
    };
    if buf.get(3) != Some(&1) {
        return Err(BadFrame);
    }
    let mtype = *buf.get(1).ok_or(BadFrame)?;
    let body_len = u32_at(buf, 4, big).ok_or(BadFrame)? as usize;
    let fields_len = u32_at(buf, 12, big).ok_or(BadFrame)? as usize;
    if fields_len > MAX_FIELDS || body_len > MAX_WIRE_MESSAGE {
        return Err(BadFrame);
    }
    let body_at = 16 + fields_len.next_multiple_of(8);
    Ok(Frame {
        big,
        mtype,
        fields_len,
        body_at,
        total: body_at + body_len,
    })
}

/// The header fields the drain needs, borrowed from the message.
#[derive(Default, Debug)]
pub(super) struct Fields<'a> {
    pub(super) path: Option<&'a [u8]>,
    pub(super) interface: Option<&'a [u8]>,
    pub(super) member: Option<&'a [u8]>,
    pub(super) sender: Option<&'a [u8]>,
    pub(super) signature: Option<&'a [u8]>,
    pub(super) reply_serial: Option<u32>,
}

/// The header-field array is malformed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct BadFields;

/// Walk the header-field array of a complete message.
pub(super) fn fields<'a>(msg: &'a [u8], f: &Frame) -> Result<Fields<'a>, BadFields> {
    let hdr = msg.get(..16 + f.fields_len).ok_or(BadFields)?;
    let mut out = Fields::default();
    let mut pos = 16usize;
    while pos < hdr.len() {
        pos = pos.next_multiple_of(8);
        if pos >= hdr.len() {
            break;
        }
        let code = *hdr.get(pos).ok_or(BadFields)?;
        pos += 1;
        let sig_len = usize::from(*hdr.get(pos).ok_or(BadFields)?);
        pos += 1;
        let sig = hdr.get(pos..pos + sig_len).ok_or(BadFields)?;
        pos += sig_len;
        if hdr.get(pos) != Some(&0) || sig_len != 1 {
            return Err(BadFields);
        }
        pos += 1;
        let ty = *sig.first().ok_or(BadFields)?;
        let value: Option<&[u8]> = match ty {
            b'o' | b's' => {
                pos = pos.next_multiple_of(4);
                let len = u32_at(hdr, pos, f.big).ok_or(BadFields)? as usize;
                pos += 4;
                let s = hdr.get(pos..pos + len).ok_or(BadFields)?;
                pos += len;
                if hdr.get(pos) != Some(&0) {
                    return Err(BadFields);
                }
                pos += 1;
                Some(s)
            }
            b'g' => {
                let len = usize::from(*hdr.get(pos).ok_or(BadFields)?);
                pos += 1;
                let s = hdr.get(pos..pos + len).ok_or(BadFields)?;
                pos += len;
                if hdr.get(pos) != Some(&0) {
                    return Err(BadFields);
                }
                pos += 1;
                Some(s)
            }
            b'u' => {
                pos = pos.next_multiple_of(4);
                let v = u32_at(hdr, pos, f.big).ok_or(BadFields)?;
                pos += 4;
                if code == 5 {
                    out.reply_serial = Some(v);
                }
                None
            }
            _ => return Err(BadFields),
        };
        match (code, value) {
            (1, v) => out.path = v,
            (2, v) => out.interface = v,
            (3, v) => out.member = v,
            (7, v) => out.sender = v,
            (8, v) => out.signature = v,
            _ => {}
        }
    }
    Ok(out)
}

fn pad(out: &mut Vec<u8>, to: usize) {
    while !out.len().is_multiple_of(to) {
        out.push(0);
    }
}

#[allow(clippy::cast_possible_truncation)]
fn string_field(out: &mut Vec<u8>, code: u8, ty: u8, s: &str) {
    pad(out, 8);
    out.extend_from_slice(&[code, 1, ty, 0]);
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out.push(0);
}

/// A little-endian method call to the bus daemon. `body` is the already encoded body and
/// `body_sig` its signature.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn bus_call(serial: u32, member: &str, body_sig: Option<&str>, body: &[u8]) -> Vec<u8> {
    let mut fields = Vec::with_capacity(128);
    string_field(&mut fields, 1, b'o', "/org/freedesktop/DBus");
    string_field(&mut fields, 2, b's', "org.freedesktop.DBus");
    string_field(&mut fields, 3, b's', member);
    string_field(&mut fields, 6, b's', "org.freedesktop.DBus");
    if let Some(sig) = body_sig {
        pad(&mut fields, 8);
        fields.extend_from_slice(&[8, 1, b'g', 0, sig.len() as u8]);
        fields.extend_from_slice(sig.as_bytes());
        fields.push(0);
    }
    let mut out = Vec::with_capacity(16 + fields.len() + 8 + body.len());
    out.extend_from_slice(&[b'l', METHOD_CALL, 0, 1]);
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    out.extend_from_slice(&fields);
    pad(&mut out, 8);
    out.extend_from_slice(body);
    out
}

/// `AddMatch(s)`.
#[allow(clippy::cast_possible_truncation)]
pub(super) fn add_match(serial: u32, rule: &str) -> Vec<u8> {
    let mut body = Vec::with_capacity(rule.len() + 5);
    body.extend_from_slice(&(rule.len() as u32).to_le_bytes());
    body.extend_from_slice(rule.as_bytes());
    body.push(0);
    bus_call(serial, "AddMatch", Some("s"), &body)
}

/// Read `n` consecutive strings (`s`) from a body.
pub(super) fn strings<const N: usize>(body: &[u8], big: bool) -> Option<[&[u8]; N]> {
    let mut out: [&[u8]; N] = [&[]; N];
    let mut pos = 0usize;
    for slot in &mut out {
        pos = pos.next_multiple_of(4);
        let len = u32_at(body, pos, big)? as usize;
        pos += 4;
        *slot = body.get(pos..pos + len)?;
        pos += len;
        if body.get(pos) != Some(&0) {
            return None;
        }
        pos += 1;
    }
    Some(out)
}

/// Message encoder for tests (signals, replies) written independently of the call builder.
#[cfg(test)]
#[allow(clippy::cast_possible_truncation)]
pub(super) mod testutil {
    use super::{pad, string_field};

    pub(in crate::drain) fn message(
        mtype: u8,
        serial: u32,
        strings: &[(u8, u8, &str)],
        reply_serial: Option<u32>,
        sig: Option<&str>,
        body: &[u8],
    ) -> Vec<u8> {
        let mut f = Vec::new();
        for (code, ty, s) in strings {
            string_field(&mut f, *code, *ty, s);
        }
        if let Some(r) = reply_serial {
            pad(&mut f, 8);
            f.extend_from_slice(&[5, 1, b'u', 0]);
            f.extend_from_slice(&r.to_le_bytes());
        }
        if let Some(s) = sig {
            pad(&mut f, 8);
            f.extend_from_slice(&[8, 1, b'g', 0, s.len() as u8]);
            f.extend_from_slice(s.as_bytes());
            f.push(0);
        }
        let mut out = vec![b'l', mtype, 0, 1];
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&serial.to_le_bytes());
        out.extend_from_slice(&(f.len() as u32).to_le_bytes());
        out.extend_from_slice(&f);
        pad(&mut out, 8);
        out.extend_from_slice(body);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_built_call_is_framed_and_parsed_back() {
        let m = add_match(7, "type='signal'");
        let f = peek(&m).unwrap();
        assert_eq!(f.total, m.len());
        assert_eq!(f.mtype, METHOD_CALL);
        let h = fields(&m, &f).unwrap();
        assert_eq!(h.member, Some(&b"AddMatch"[..]));
        assert_eq!(h.path, Some(&b"/org/freedesktop/DBus"[..]));
        assert_eq!(h.signature, Some(&b"s"[..]));
        let body = &m[f.body_at..];
        let [rule] = strings::<1>(body, false).unwrap();
        assert_eq!(rule, b"type='signal'");
    }

    #[test]
    fn hello_has_no_signature_or_body() {
        let m = bus_call(1, "Hello", None, &[]);
        let f = peek(&m).unwrap();
        assert_eq!(f.total, m.len());
        assert!(fields(&m, &f).unwrap().signature.is_none());
    }

    #[test]
    fn garbage_is_not_a_frame() {
        assert_eq!(peek(&[b'x'; 16]), Err(BadFrame));
        let mut m = bus_call(1, "Hello", None, &[]);
        m[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(peek(&m), Err(BadFrame));
    }

    #[test]
    fn truncated_fields_are_malformed_not_a_panic() {
        let m = add_match(2, "x");
        let f = peek(&m).unwrap();
        for cut in 16..(16 + f.fields_len) {
            let _ = fields(&m[..cut], &f);
        }
        let mut bad = m.clone();
        bad[17] = 200; // the signature length of the first field
        assert!(fields(&bad, &f).is_err());
    }
}
