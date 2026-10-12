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
