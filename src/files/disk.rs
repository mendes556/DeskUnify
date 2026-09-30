// DeskUnify changes, 2026-10-01; derived from Lan Mouse, GPL-3.0-or-later.
use std::{
    fs::File,
    io,
    path::{Path, PathBuf},
};

#[cfg(target_os = "macos")]
fn fsync(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    loop {
        // Rust's sync_all/sync_data use F_FULLFSYNC on Darwin. Stage every
        // file with fsync, then flush the drive once after the whole batch.
        if unsafe { libc::fsync(file.as_raw_fd()) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "macos")]
pub(super) fn sync_file(file: &File) -> io::Result<()> {
    fsync(file)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn sync_file(file: &File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(target_os = "macos")]
pub(super) fn finish_batch(
    root: &Path,
    directories: &[PathBuf],
    barrier_directory: &Path,
) -> io::Result<()> {
    use std::io::Write;
    for path in directories {
        fsync(&File::open(path)?)?;
    }
    fsync(&File::open(root)?)?;
    // Use a regular file on the same filesystem: full-sync on a directory is
    // not universally supported. Keep it outside the delivered batch so a
    // process crash cannot add a temporary marker to the user's received files.
    let mut barrier = tempfile::Builder::new()
        .prefix(".lan-bridge-commit-")
        .tempfile_in(barrier_directory)?;
    barrier.write_all(b"LBF2")?;
    barrier.as_file().sync_all()
}

#[cfg(not(target_os = "macos"))]
pub(super) fn finish_batch(_: &Path, _: &[PathBuf], _: &Path) -> io::Result<()> {
    Ok(())
}
