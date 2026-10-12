//! Process identity helpers (ADR-0001 ID-2).
//!
//! `comm` must be `orca` before `ScreenReaderEnabled` is set, so that Chromium's `/proc`
//! detection finds a screen reader. The daemon decides when to call this; Phase 0 never does.
#![allow(unsafe_code)]

use std::ffi::CString;
use std::io;

/// Set the calling thread's `comm` (`prctl(PR_SET_NAME)`). On the main thread this is the
/// thread-group leader's `comm`, which is what Chromium reads. The kernel truncates to 15 bytes.
pub fn set_thread_comm(name: &str) -> io::Result<()> {
    let c = CString::new(name).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    // SAFETY: `c` is a valid NUL-terminated string that outlives the call. PR_SET_NAME reads at
    // most 16 bytes from it and the remaining prctl arguments are ignored for this option.
    let rc = unsafe { libc::prctl(libc::PR_SET_NAME, c.as_ptr(), 0, 0, 0) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comm_is_visible_in_proc_for_this_thread() {
        std::thread::spawn(|| {
            set_thread_comm("gorca-test").unwrap();
            let comm = std::fs::read_to_string("/proc/thread-self/comm").unwrap();
            assert_eq!(comm.trim(), "gorca-test");
        })
        .join()
        .unwrap();
    }

    #[test]
    fn interior_nul_is_rejected() {
        assert!(set_thread_comm("a\0b").is_err());
    }
}
