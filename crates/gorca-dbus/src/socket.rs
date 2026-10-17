// SPDX-License-Identifier: GPL-3.0-or-later
//! Socket backlog sampling for drain lag detection (D-2).
//!
//! The only `unsafe` in this crate (ADR-0001 section 11, rule 6): one `ioctl`.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsFd, AsRawFd};

/// Bytes waiting in the receive queue of a socket (`ioctl(SIOCINQ)`, which is `FIONREAD`).
pub fn socket_backlog(sock: impl AsFd) -> io::Result<usize> {
    let mut pending: libc::c_int = 0;
    // SAFETY: the fd is valid for the duration of the call because `sock` borrows it, and
    // FIONREAD writes exactly one `c_int` through the pointer, which refers to a live local.
    let rc = unsafe { libc::ioctl(sock.as_fd().as_raw_fd(), libc::FIONREAD, &raw mut pending) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(usize::try_from(pending).unwrap_or(0))
}

/// `sizeof(int)` as a `socklen_t`.
const INT_LEN: libc::socklen_t = 4;

/// Ask for a larger kernel receive buffer (`SO_RCVBUF`, PC-6). Returns the size the kernel
/// reports back, which is twice the request (bookkeeping overhead) and capped by `rmem_max`.
pub fn set_recv_buffer(sock: impl AsFd, bytes: usize) -> io::Result<usize> {
    let want = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
    let fd = sock.as_fd().as_raw_fd();
    // SAFETY: `fd` is valid while `sock` is borrowed; the option value is a live local `c_int`
    // and the length passed is its size.
    let rc = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&raw const want).cast(),
            INT_LEN,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut got: libc::c_int = 0;
    let mut len = INT_LEN;
    // SAFETY: as above; `got` and `len` are live locals and `len` holds the buffer size.
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            (&raw mut got).cast(),
            &raw mut len,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(usize::try_from(got).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    #[test]
    fn backlog_tracks_unread_bytes() {
        let (mut a, mut b) = UnixStream::pair().unwrap();
        assert_eq!(socket_backlog(&b).unwrap(), 0);
        a.write_all(&[7u8; 1500]).unwrap();
        assert_eq!(socket_backlog(&b).unwrap(), 1500);
        let mut buf = [0u8; 1000];
        b.read_exact(&mut buf).unwrap();
        assert_eq!(socket_backlog(&b).unwrap(), 500);
    }

    #[test]
    fn works_on_pipes_too() {
        let (r, mut w) = std::io::pipe().unwrap();
        w.write_all(&[1u8; 10]).unwrap();
        assert_eq!(socket_backlog(&r).unwrap(), 10);
    }
}
