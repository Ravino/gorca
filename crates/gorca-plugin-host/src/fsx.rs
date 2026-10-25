// SPDX-License-Identifier: GPL-3.0-or-later
//! Safe opening of untrusted paths: never block on a FIFO, never follow a final symlink, only
//! regular files.

use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;

/// Open `path` read-only with `O_NOFOLLOW | O_NONBLOCK` and require a regular file (`fstat` on
/// the open descriptor, so there is no check/use gap). A FIFO, device, socket, directory or
/// symlink is refused with `InvalidInput` (or the OS error for a symlink).
pub fn open_regular(path: &Path) -> io::Result<File> {
    let f = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if f.metadata()?.file_type().is_file() {
        Ok(f)
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}
