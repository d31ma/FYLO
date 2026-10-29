//! The operating-system calls behind `.fxdb` byte-range locks.
//!
//! Each function locks or unlocks exactly one byte and never waits: waiting is
//! the caller's loop, so a thread is never parked inside the kernel holding a
//! lock request the in-process registry does not know about.
//!
//! - Linux: open-file-description locks (`F_OFD_SETLK`), owned by the open
//!   file, so two opens of the file conflict even inside one process.
//! - Other Unix, macOS included: classic `fcntl` locks (`F_SETLK`), owned by
//!   the process. Closing *any* descriptor for the file drops every lock the
//!   process holds on it; the registry in the parent module keeps one
//!   descriptor per file and defers other closes while a lock is held.
//! - Windows: `LockFileEx` with `LOCKFILE_FAIL_IMMEDIATELY` on a synchronous
//!   handle, owned by the handle.

use std::fs::File;
use std::io;
use std::path::Path;

/// One file's identity, so that two opens of it share one lock owner.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) enum FileKey {
    /// Device and file index from the open handle.
    Id(u64, u128),
    /// The canonical path, for a filesystem that reports no file index.
    #[cfg(windows)]
    Path(std::path::PathBuf),
}

#[cfg(unix)]
pub(super) fn file_key(file: &File, _path: &Path) -> io::Result<FileKey> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok(FileKey::Id(metadata.dev(), u128::from(metadata.ino())))
}

#[cfg(unix)]
fn offset(byte: u64) -> io::Result<libc::off_t> {
    libc::off_t::try_from(byte).map_err(|_| {
        io::Error::new(
            io::ErrorKind::Unsupported,
            "byte-range lock offset needs a 64-bit off_t",
        )
    })
}

/// A `flock` field from a libc constant, which is an `int` or a `short`
/// depending on the target; every one fits a `short`.
#[cfg(unix)]
fn short<T>(value: T) -> libc::c_short
where
    libc::c_short: TryFrom<T>,
{
    libc::c_short::try_from(value)
        .ok()
        .expect("fcntl lock constants fit a short")
}

#[cfg(target_os = "linux")]
const SET_LOCK: libc::c_int = libc::F_OFD_SETLK;
#[cfg(all(unix, not(target_os = "linux")))]
const SET_LOCK: libc::c_int = libc::F_SETLK;

/// Apply one non-waiting `fcntl` lock request to one byte.
///
/// Returns `false` when another owner holds a conflicting lock.
#[cfg(unix)]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: fcntl lock requests take a raw struct pointer"
)]
fn set_lock(file: &File, byte: u64, kind: libc::c_short) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` is a plain C struct of integers, for which all-zero is a
    // valid value. Zero `l_pid` is what `F_OFD_SETLK` requires, and zeroing
    // also clears any target-specific padding or `l_sysid` field.
    let mut request: libc::flock = unsafe { std::mem::zeroed() };
    request.l_type = kind;
    request.l_whence = short(libc::SEEK_SET);
    request.l_start = offset(byte)?;
    request.l_len = 1;
    loop {
        // SAFETY: the descriptor is borrowed from a live `File` for the whole
        // call, `SET_LOCK` takes a `struct flock *` as its third argument, and
        // `request` is a valid, initialized, exclusively borrowed `flock` that
        // outlives the call. The kernel only reads it for a set request.
        let result = unsafe { libc::fcntl(file.as_raw_fd(), SET_LOCK, &raw mut request) };
        if result != -1 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            // POSIX allows either for "held by another owner".
            Some(libc::EAGAIN | libc::EACCES) => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// Take one byte, shared or exclusive, without waiting.
///
/// The file must be open for writing for an exclusive lock and for reading for
/// a shared one.
#[cfg(unix)]
pub(super) fn try_lock(file: &File, byte: u64, exclusive: bool) -> io::Result<bool> {
    let kind = if exclusive {
        libc::F_WRLCK
    } else {
        libc::F_RDLCK
    };
    set_lock(file, byte, short(kind))
}

/// Release one byte this owner holds.
#[cfg(unix)]
pub(super) fn unlock(file: &File, byte: u64) -> io::Result<()> {
    if set_lock(file, byte, short(libc::F_UNLCK))? {
        Ok(())
    } else {
        Err(io::Error::other("unlock of a byte-range lock was refused"))
    }
}

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{ERROR_LOCK_VIOLATION, HANDLE},
    Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle, LOCKFILE_EXCLUSIVE_LOCK,
        LOCKFILE_FAIL_IMMEDIATELY, LockFileEx, UnlockFileEx,
    },
    System::IO::OVERLAPPED,
};

#[cfg(windows)]
fn raw_handle(file: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle().cast()
}

/// The position of one byte, as `LockFileEx` takes it.
#[cfg(windows)]
fn overlapped_at(byte: u64) -> OVERLAPPED {
    let mut overlapped = OVERLAPPED::default();
    // Writing a `Copy` union field is safe; only reading one is not.
    overlapped.Anonymous.Anonymous.Offset = (byte & 0xFFFF_FFFF) as u32;
    overlapped.Anonymous.Anonymous.OffsetHigh = (byte >> 32) as u32;
    overlapped
}

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: GetFileInformationByHandle fills a caller struct"
)]
pub(super) fn file_key(file: &File, path: &Path) -> io::Result<FileKey> {
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the handle is borrowed from a live `File` for the whole call,
    // and `information` is a valid, exclusively borrowed struct of the type
    // the function writes, which outlives the call.
    if unsafe { GetFileInformationByHandle(raw_handle(file), &raw mut information) } == 0 {
        return Err(io::Error::last_os_error());
    }
    let index =
        (u128::from(information.nFileIndexHigh) << 32) | u128::from(information.nFileIndexLow);
    if index == 0 {
        // Some redirectors report no index; two files would then share a key.
        return Ok(FileKey::Path(std::fs::canonicalize(path)?));
    }
    Ok(FileKey::Id(
        u64::from(information.dwVolumeSerialNumber),
        index,
    ))
}

/// Take one byte, shared or exclusive, without waiting.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: LockFileEx takes a raw OVERLAPPED pointer"
)]
pub(super) fn try_lock(file: &File, byte: u64, exclusive: bool) -> io::Result<bool> {
    let mut flags = LOCKFILE_FAIL_IMMEDIATELY;
    if exclusive {
        flags |= LOCKFILE_EXCLUSIVE_LOCK;
    }
    let mut overlapped = overlapped_at(byte);
    // SAFETY: the handle is borrowed from a live `File` that the parent module
    // opened itself through `std::fs::OpenOptions` without
    // `FILE_FLAG_OVERLAPPED`, so the call completes before it returns and
    // never retains `overlapped`, which is valid and exclusively borrowed for
    // the call. The range is one byte at `byte`.
    if unsafe { LockFileEx(raw_handle(file), flags, 0, 1, 0, &raw mut overlapped) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION.cast_signed()) {
        Ok(false)
    } else {
        Err(error)
    }
}

/// Release one byte this handle holds.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: UnlockFileEx takes a raw OVERLAPPED pointer"
)]
pub(super) fn unlock(file: &File, byte: u64) -> io::Result<()> {
    let mut overlapped = overlapped_at(byte);
    // SAFETY: as in `try_lock`: a live synchronous handle, and an
    // `OVERLAPPED` that is valid, exclusively borrowed and not retained. The
    // range matches the one `try_lock` locked, as `UnlockFileEx` requires.
    if unsafe { UnlockFileEx(raw_handle(file), 0, 1, 0, &raw mut overlapped) } != 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
