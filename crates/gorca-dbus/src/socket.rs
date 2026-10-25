// SPDX-License-Identifier: GPL-3.0-or-later
//! Socket backlog sampling for drain lag detection (D-2).
//!
//! The only `unsafe` in this crate (ADR-0001 section 11, rule 6): `ioctl` and `getsockopt` calls.
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

/// The pid of the process at the other end of a connected Unix socket (`SO_PEERCRED`, as the
/// kernel recorded it when the connection was made), in the caller's pid namespace.
pub fn peer_pid(sock: impl AsFd) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).unwrap_or(12);
    // SAFETY: `sock` is an open descriptor for the call; `cred` and `len` are live locals and
    // `len` holds the size of the `ucred` the kernel writes.
    let rc = unsafe {
        libc::getsockopt(
            sock.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut cred).cast(),
            &raw mut len,
        )
    };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(cred.pid).map_err(|_| io::Error::other("no peer pid"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_peer_of_a_socketpair_is_this_process() {
        let (a, _b) = std::os::unix::net::UnixStream::pair().unwrap();
        assert_eq!(peer_pid(&a).unwrap(), std::process::id());
    }

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
