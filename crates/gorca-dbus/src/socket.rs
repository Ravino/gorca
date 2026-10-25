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

/// Connect to a Unix stream socket by path under a deadline. `std` offers only a blocking
/// `connect(2)`, which waits for ever once the server's backlog is full (a frozen server); this
/// connects non-blocking and retries `EAGAIN` until `timeout` has passed (`ErrorKind::TimedOut`).
/// The stream is returned in blocking mode.
pub fn connect_unix_timeout(
    path: &std::path::Path,
    timeout: std::time::Duration,
) -> io::Result<std::os::unix::net::UnixStream> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::OsStrExt;
    let bytes = path.as_os_str().as_bytes();
    // SAFETY: an all-zero `sockaddr_un` is a valid value; it is filled in below.
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    if bytes.is_empty() || bytes.len() >= addr.sun_path.len() || bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "bad socket path",
        ));
    }
    addr.sun_family = libc::sa_family_t::try_from(libc::AF_UNIX).unwrap_or(1);
    for (dst, src) in addr.sun_path.iter_mut().zip(bytes) {
        *dst = libc::c_char::from_ne_bytes([*src]);
    }
    // SAFETY: plain `socket(2)`; the result is checked.
    let fd = unsafe {
        libc::socket(
            libc::AF_UNIX,
            libc::SOCK_STREAM | libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC,
            0,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `fd` is a fresh descriptor nobody else owns; the stream takes ownership.
    let stream = unsafe { std::os::unix::net::UnixStream::from_raw_fd(fd) };
    let len = libc::socklen_t::try_from(std::mem::size_of::<libc::sockaddr_un>()).unwrap_or(110);
    let end = std::time::Instant::now() + timeout;
    loop {
        // SAFETY: `fd` is open for the call and `addr` is a live `sockaddr_un` of `len` bytes.
        let rc = unsafe { libc::connect(fd, (&raw const addr).cast(), len) };
        if rc == 0 {
            stream.set_nonblocking(false)?;
            return Ok(stream);
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(libc::EAGAIN) {
            return Err(err);
        }
        if std::time::Instant::now() >= end {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out"));
        }
        // Blocking pool thread (never an async worker): wait a moment for a backlog slot.
        #[allow(clippy::disallowed_methods)]
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
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
