//! Hole punching of free `.vdbx` extents (ADR 0016 section 6).
//!
//! Each function deallocates one byte range of a file while keeping the
//! file's length, and returns `false` rather than an error when the platform
//! or filesystem cannot, which only means the space is kept.
//!
//! - Linux and Android: `fallocate(FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE)`
//!   through `rustix`, which needs no `unsafe` here.
//! - macOS and the other Apple targets: `fcntl(F_PUNCHHOLE)`. APFS supports
//!   it; HFS+ and most network filesystems answer `ENOTSUP`.
//! - Windows: the file is marked sparse with `FSCTL_SET_SPARSE`, then the
//!   range is deallocated with `FSCTL_SET_ZERO_DATA`. Without the sparse mark
//!   that call writes zeros and frees nothing, so a filesystem that refuses
//!   the mark (FAT, exFAT, some SMB servers) keeps the space.
//! - Everything else, WebAssembly included: never punched.
//!
//! The offset and length are always whole extents, 1 to 64 MiB aligned, so
//! they are multiples of any filesystem's block or allocation unit size.

use std::io;

use fylo_vfs::File;

/// Deallocate a range, keeping the file's length. `false` when the
/// filesystem or platform cannot.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub(super) fn punch_hole(file: &File, offset: u64, length: u64) -> io::Result<bool> {
    use rustix::fs::FallocateFlags;
    match rustix::fs::fallocate(
        file,
        FallocateFlags::PUNCH_HOLE | FallocateFlags::KEEP_SIZE,
        offset,
        length,
    ) {
        Ok(()) => Ok(true),
        Err(rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::NOSYS) => Ok(false),
        Err(error) => Err(error.into()),
    }
}

/// Deallocate a range, keeping the file's length. `false` when the
/// filesystem cannot.
#[cfg(target_vendor = "apple")]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: fcntl(F_PUNCHHOLE) takes a raw struct pointer"
)]
pub(super) fn punch_hole(file: &File, offset: u64, length: u64) -> io::Result<bool> {
    use std::os::fd::AsRawFd;
    let (Ok(fp_offset), Ok(fp_length)) =
        (libc::off_t::try_from(offset), libc::off_t::try_from(length))
    else {
        return Ok(false);
    };
    let request = libc::fpunchhole_t {
        fp_flags: 0,
        reserved: 0,
        fp_offset,
        fp_length,
    };
    loop {
        // SAFETY: the descriptor is borrowed from a live `File` for the whole
        // call, `F_PUNCHHOLE` takes a `struct fpunchhole *` as its third
        // argument, and `request` is a valid, fully initialized
        // `fpunchhole_t` with its unused fields zero, which outlives the
        // call. The kernel only reads it.
        let result =
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_PUNCHHOLE, &raw const request) };
        if result != -1 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => {}
            // The filesystem cannot punch (`ENOTSUP`, `EOPNOTSUPP`), or will
            // not for this range on this volume (`EINVAL`, for a block size
            // the extent is not a multiple of).
            Some(libc::ENOTSUP | libc::EOPNOTSUPP | libc::EINVAL) => return Ok(false),
            _ => return Err(error),
        }
    }
}

#[cfg(windows)]
use windows_sys::Win32::{
    Foundation::{ERROR_INVALID_FUNCTION, ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, HANDLE},
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FILE_SET_SPARSE_BUFFER, FILE_ZERO_DATA_INFORMATION, FSCTL_SET_SPARSE,
            FSCTL_SET_ZERO_DATA,
        },
    },
};

#[cfg(windows)]
fn raw_handle(file: &File) -> HANDLE {
    use std::os::windows::io::AsRawHandle;
    file.as_raw_handle().cast()
}

/// Whether a failed control request means the filesystem cannot do it,
/// rather than that something went wrong.
#[cfg(windows)]
fn unsupported(error: &io::Error) -> bool {
    [
        ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED,
        ERROR_INVALID_PARAMETER,
    ]
    .iter()
    .any(|&code| error.raw_os_error() == Some(code.cast_signed()))
}

/// Send one buffered control request with no output to a synchronous handle.
#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: DeviceIoControl takes a raw input buffer pointer"
)]
fn control<T>(file: &File, code: u32, input: &T) -> io::Result<()> {
    let size = u32::try_from(std::mem::size_of::<T>()).expect("small struct");
    let mut returned = 0_u32;
    // SAFETY: the handle is borrowed from a live `File` that
    // `ByteRangeLockFile::open` opened through `std::fs::OpenOptions` without
    // `FILE_FLAG_OVERLAPPED`, so with a null `OVERLAPPED` the call completes
    // before it returns and retains no pointer. `input` is a valid,
    // initialized struct of exactly `size` bytes, the one `code` reads, and
    // outlives the call; the request has no output, so the output buffer is
    // null with size zero; `returned` is the valid, exclusively borrowed
    // count a null `OVERLAPPED` requires.
    let done = unsafe {
        DeviceIoControl(
            raw_handle(file),
            code,
            std::ptr::from_ref(input).cast(),
            size,
            std::ptr::null_mut(),
            0,
            &raw mut returned,
            std::ptr::null_mut(),
        )
    };
    if done == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Deallocate a range, keeping the file's length. `false` when the
/// filesystem cannot.
#[cfg(windows)]
pub(super) fn punch_hole(file: &File, offset: u64, length: u64) -> io::Result<bool> {
    use std::os::windows::fs::MetadataExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_SPARSE_FILE;
    let (Ok(start), Some(Ok(end))) = (
        i64::try_from(offset),
        offset.checked_add(length).map(i64::try_from),
    ) else {
        return Ok(false);
    };
    if file.metadata()?.file_attributes() & FILE_ATTRIBUTE_SPARSE_FILE == 0
        && control(
            file,
            FSCTL_SET_SPARSE,
            &FILE_SET_SPARSE_BUFFER { SetSparse: true },
        )
        .is_err()
    {
        // Zeroing a range of a file that is not sparse writes zeros and
        // frees nothing, so a file that cannot be marked keeps its space.
        return Ok(false);
    }
    let range = FILE_ZERO_DATA_INFORMATION {
        FileOffset: start,
        BeyondFinalZero: end,
    };
    match control(file, FSCTL_SET_ZERO_DATA, &range) {
        Ok(()) => Ok(true),
        Err(error) if unsupported(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// The bytes a file has allocated on disk, which a sparse file's holes do
/// not count, for the tests to see a punch free space.
#[cfg(all(windows, test))]
#[allow(
    unsafe_code,
    reason = "ADR 0004/0016: GetCompressedFileSizeW takes a raw path and out-pointer"
)]
pub(super) fn allocated_bytes(path: &std::path::Path) -> io::Result<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::{
        Foundation::NO_ERROR, Storage::FileSystem::GetCompressedFileSizeW,
        Storage::FileSystem::INVALID_FILE_SIZE,
    };
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut high = 0_u32;
    // SAFETY: `wide` is a valid, NUL-terminated UTF-16 path that outlives the
    // call, and `high` is a valid, exclusively borrowed `u32` the function
    // writes the upper half of the size to.
    let low = unsafe { GetCompressedFileSizeW(wide.as_ptr(), &raw mut high) };
    if low == INVALID_FILE_SIZE {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(NO_ERROR.cast_signed()) {
            return Err(error);
        }
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

/// No hole punching on this platform; the space is kept and
/// `shrink_free_tail` returns it.
#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_vendor = "apple",
    windows
)))]
pub(super) fn punch_hole(_file: &File, _offset: u64, _length: u64) -> io::Result<bool> {
    Ok(false)
}
