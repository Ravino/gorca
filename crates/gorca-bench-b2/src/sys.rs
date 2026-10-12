//! The few raw system calls the harness needs. All unsafe code of the crate lives here.
#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsFd, AsRawFd};

/// `send(2)` with `MSG_DONTWAIT | MSG_NOSIGNAL`. `Ok(0)` means "would block".
pub fn send_nowait(sock: impl AsFd, buf: &[u8]) -> io::Result<usize> {
    if buf.is_empty() {
        return Ok(0);
    }
    // SAFETY: the fd is valid while `sock` is borrowed; `buf` is a live slice and its exact
    // length is passed.
    let n = unsafe {
        libc::send(
            sock.as_fd().as_raw_fd(),
            buf.as_ptr().cast(),
            buf.len(),
            libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
        )
    };
    if n >= 0 {
        return Ok(n as usize);
    }
    let e = io::Error::last_os_error();
    if e.kind() == io::ErrorKind::WouldBlock {
        Ok(0)
    } else {
        Err(e)
    }
}

/// Bytes the kernel still holds for the peer to read (`SIOCOUTQ`, i.e. `TIOCOUTQ`).
pub fn outq(sock: impl AsFd) -> usize {
    let mut n: libc::c_int = 0;
    // SAFETY: valid fd; TIOCOUTQ writes one `c_int` through the pointer to a live local.
    let rc = unsafe { libc::ioctl(sock.as_fd().as_raw_fd(), libc::TIOCOUTQ, &raw mut n) };
    if rc < 0 {
        0
    } else {
        usize::try_from(n).unwrap_or(0)
    }
}

/// Raise the send buffer of a socket (best effort).
pub fn set_send_buffer(sock: impl AsFd, bytes: usize) {
    let v = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
    // SAFETY: valid fd; the value is a live local `c_int` and its size is passed.
    unsafe {
        libc::setsockopt(
            sock.as_fd().as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            (&raw const v).cast(),
            4,
        );
    }
}

/// Number of online CPUs, for reporting.
pub fn cpus() -> usize {
    std::thread::available_parallelism().map_or(1, usize::from)
}

/// SIGKILL a whole process group (children started with `process_group(0)`).
pub fn kill_group(pgid: u32) {
    let Ok(pid) = libc::pid_t::try_from(pgid) else {
        return;
    };
    // SAFETY: plain `kill(2)` with a negative pid addresses the group; no memory is involved.
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}
