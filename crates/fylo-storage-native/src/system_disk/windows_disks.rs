use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED, ERROR_SUCCESS, ERROR_VIRTDISK_NOT_VIRTUAL_DISK,
    S_OK,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
};
use windows_sys::Win32::Storage::Vhd::{
    GET_STORAGE_DEPENDENCY_FLAG_HOST_VOLUMES, GetStorageDependencyInformation,
    STORAGE_DEPENDENCY_INFO, STORAGE_DEPENDENCY_INFO_TYPE_2, STORAGE_DEPENDENCY_INFO_VERSION_2,
};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::DISK_EXTENT;
use windows_sys::Win32::System::SystemInformation::GetWindowsDirectoryW;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_LocalAppData, FOLDERID_RoamingAppData, SHGetKnownFolderPath,
};
use windows_sys::core::{GUID, PWSTR};

use super::disk_description::DiskDescription;

/// The Windows calls behind the system-disk rule (ADR 0032 section 2): the
/// volume of a path, the physical disks of a volume, the file behind a
/// virtual disk, and the app data folders. Rust's standard library has no
/// volume API. ADR 0032 (the user's "Yes, with review"): every unsafe block
/// has a `SAFETY` comment and an entry in
/// `docs/security/unsafe-inventory.md`, and this is the only file of the
/// rule with unsafe code.
pub(crate) struct WindowsDisks;

/// The answer buffer of `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`: the layout of
/// `VOLUME_DISK_EXTENTS` with room for many extents. `repr(C)` puts
/// `extents` at offset 8, as in the C struct.
#[repr(C)]
struct ExtentBuffer {
    count: u32,
    extents: [DISK_EXTENT; WindowsDisks::MAX_EXTENTS],
}

impl WindowsDisks {
    const MAX_EXTENTS: usize = 64;
    const PATH_UNITS: usize = 32_768;
    const VOLUME_NAME_UNITS: usize = 260;
    const DEPENDENCY_WORDS: usize = 8_192;
    const VERBATIM_PREFIX: &'static str = r"\\?\";
    const VOLUME_KEY: &'static str = "volume:";
    const DISK_KEY: &'static str = "disk:";

    /// The volume of `path` and its physical disks. When the disks cannot
    /// be read, the volume alone decides.
    pub(crate) fn describe(path: &Path) -> Result<DiskDescription, String> {
        let volume = Self::volume(path).map_err(|error| {
            format!("the volume of {} cannot be read ({error})", path.display())
        })?;
        let disks = Self::disk_numbers(&volume).unwrap_or_default();
        Ok(Self::description(&volume, &disks))
    }

    /// The disk of the Windows folder: `%SystemDrive%`, as the OS reports it.
    pub(crate) fn system() -> Result<DiskDescription, String> {
        Self::describe(&Self::windows_folder()?)
    }

    /// The files behind the virtual disk (VHD, VHDX, ISO) that holds the
    /// volume of `disk`; none for a real disk.
    pub(crate) fn images(disk: &DiskDescription) -> Result<Vec<PathBuf>, String> {
        let Some(volume) = disk
            .keys
            .iter()
            .find_map(|key| key.strip_prefix(Self::VOLUME_KEY))
        else {
            return Ok(Vec::new());
        };
        Self::dependencies(volume)
    }

    /// `FOLDERID_LocalAppData` and `FOLDERID_RoamingAppData`.
    pub(crate) fn app_data_folders() -> (Option<PathBuf>, Option<PathBuf>) {
        (
            Self::known_folder(&FOLDERID_LocalAppData),
            Self::known_folder(&FOLDERID_RoamingAppData),
        )
    }

    fn description(volume: &str, disks: &[u32]) -> DiskDescription {
        let volume = volume.to_ascii_lowercase();
        let mut description = DiskDescription::named(&format!("{}{volume}", Self::VOLUME_KEY));
        description
            .keys
            .extend(disks.iter().map(|disk| format!("{}{disk}", Self::DISK_KEY)));
        let numbers: Vec<String> = disks.iter().map(u32::to_string).collect();
        description.label = format!("{volume} (disk {})", numbers.join(", "));
        description
    }

    /// `\\?\Volume{GUID}\` for the volume that holds `path`.
    #[allow(
        unsafe_code,
        reason = "ADR 0032: GetVolumePathNameW and GetVolumeNameForVolumeMountPointW write into caller buffers"
    )]
    fn volume(path: &Path) -> std::io::Result<String> {
        let wide = Self::wide(&Self::plain(path));
        let mut mount_point = vec![0_u16; Self::PATH_UNITS];
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call; `mount_point` is an exclusively borrowed buffer of exactly the
        // length passed, which the call writes a NUL-terminated name into.
        let found = unsafe {
            GetVolumePathNameW(
                wide.as_ptr(),
                mount_point.as_mut_ptr(),
                Self::units(mount_point.len()),
            )
        };
        if found == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut volume = vec![0_u16; Self::VOLUME_NAME_UNITS];
        // SAFETY: `mount_point` was NUL-terminated by the call above and
        // outlives this one; `volume` is an exclusively borrowed buffer of
        // exactly the length passed.
        let named = unsafe {
            GetVolumeNameForVolumeMountPointW(
                mount_point.as_ptr(),
                volume.as_mut_ptr(),
                Self::units(volume.len()),
            )
        };
        if named == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self::text(&volume))
    }

    /// The physical disk numbers under `volume` (`\\?\Volume{GUID}\`).
    #[allow(
        unsafe_code,
        reason = "ADR 0032: DeviceIoControl writes VOLUME_DISK_EXTENTS into a caller buffer"
    )]
    fn disk_numbers(volume: &str) -> std::io::Result<Vec<u32>> {
        let handle = Self::open_volume(volume)?;
        let mut buffer = ExtentBuffer {
            count: 0,
            extents: [DISK_EXTENT::default(); Self::MAX_EXTENTS],
        };
        let mut returned = 0_u32;
        // SAFETY: `handle` is an open volume handle that outlives the call;
        // there is no input buffer; `buffer` is an exclusively borrowed
        // `repr(C)` struct with the layout of `VOLUME_DISK_EXTENTS` and room
        // for `MAX_EXTENTS` extents, and the size passed is its exact size;
        // `returned` is an exclusively borrowed u32; the call is synchronous,
        // so no OVERLAPPED is passed.
        let done = unsafe {
            DeviceIoControl(
                handle.as_raw_handle(),
                IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
                null(),
                0,
                (&raw mut buffer).cast(),
                Self::units(size_of::<ExtentBuffer>()),
                &raw mut returned,
                null_mut(),
            )
        };
        if done == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let count = usize::try_from(buffer.count)
            .unwrap_or(usize::MAX)
            .min(Self::MAX_EXTENTS);
        Ok(buffer.extents[..count]
            .iter()
            .map(|extent| extent.DiskNumber)
            .collect())
    }

    /// The files behind the virtual disk that holds `volume`: each host
    /// volume joined with the file's path on it.
    #[allow(
        unsafe_code,
        reason = "ADR 0032: GetStorageDependencyInformation fills a variable-length struct whose strings point into it"
    )]
    fn dependencies(volume: &str) -> Result<Vec<PathBuf>, String> {
        let handle = Self::open_volume(volume)
            .map_err(|error| format!("{volume} cannot be opened ({error})"))?;
        let mut buffer = vec![0_u64; Self::DEPENDENCY_WORDS];
        let bytes = size_of_val(buffer.as_slice());
        let info = buffer.as_mut_ptr().cast::<STORAGE_DEPENDENCY_INFO>();
        // SAFETY: `buffer` holds `bytes` bytes, 8-byte aligned, which is at
        // least the size and alignment of `STORAGE_DEPENDENCY_INFO`; writing
        // its first field asks for version 2 entries, as the call requires.
        unsafe { (*info).Version = STORAGE_DEPENDENCY_INFO_VERSION_2 };
        let mut used = 0_u32;
        // SAFETY: `handle` is an open volume handle that outlives the call;
        // `info` points at `bytes` writable bytes owned by `buffer`, which
        // outlives the call, and the size passed is exactly `bytes`; `used`
        // is an exclusively borrowed u32.
        let status = unsafe {
            GetStorageDependencyInformation(
                handle.as_raw_handle(),
                GET_STORAGE_DEPENDENCY_FLAG_HOST_VOLUMES,
                Self::units(bytes),
                info,
                &raw mut used,
            )
        };
        if matches!(
            status,
            ERROR_VIRTDISK_NOT_VIRTUAL_DISK | ERROR_NOT_SUPPORTED | ERROR_INVALID_FUNCTION
        ) {
            return Ok(Vec::new());
        }
        if status != ERROR_SUCCESS {
            return Err(format!(
                "the virtual disk of {volume} cannot be read (error {status})"
            ));
        }
        Ok(Self::dependency_paths(&buffer, info))
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0032: reads the entries GetStorageDependencyInformation wrote into the buffer"
    )]
    fn dependency_paths(buffer: &[u64], info: *const STORAGE_DEPENDENCY_INFO) -> Vec<PathBuf> {
        let start = buffer.as_ptr().cast::<u8>();
        let end = start.wrapping_add(size_of_val(buffer));
        // SAFETY: `info` points at the start of `buffer`, which the call
        // filled and which outlives these reads; `NumberEntries` is its
        // second field.
        let count = unsafe { (*info).NumberEntries };
        // SAFETY: the entries array begins inside `buffer`; taking its address
        // reads nothing.
        let first = unsafe {
            (&raw const (*info).Anonymous.Version2Entries).cast::<STORAGE_DEPENDENCY_INFO_TYPE_2>()
        };
        let fits = (end as usize).saturating_sub(first as usize)
            / size_of::<STORAGE_DEPENDENCY_INFO_TYPE_2>();
        let count = usize::try_from(count).unwrap_or(usize::MAX).min(fits);
        (0..count)
            .filter_map(|index| {
                // SAFETY: `index < count <= fits`, so the entry lies wholly
                // inside `buffer`; it is a plain struct of integers and
                // pointers, read by copy.
                let entry = unsafe { first.add(index).read() };
                let host = Self::buffer_string(entry.HostVolumeName, start, end)?;
                let relative = Self::buffer_string(entry.DependentVolumeRelativePath, start, end)?;
                Some(PathBuf::from(host).join(relative))
            })
            .collect()
    }

    /// A NUL-terminated string the call wrote inside `start..end`; `None`
    /// for a null pointer or one that points outside the buffer.
    #[allow(
        unsafe_code,
        reason = "ADR 0032: reads a UTF-16 string at a pointer the call wrote into the buffer"
    )]
    fn buffer_string(text: PWSTR, start: *const u8, end: *const u8) -> Option<OsString> {
        let address = text as usize;
        if text.is_null()
            || address < start as usize
            || address >= end as usize
            || !address.is_multiple_of(2)
        {
            return None;
        }
        let limit = (end as usize - address) / size_of::<u16>();
        // SAFETY: `text` is non-null, 2-byte aligned and inside the buffer,
        // and `limit` units from it stay inside the buffer, which outlives
        // this read.
        let units = unsafe { std::slice::from_raw_parts(text.cast_const(), limit) };
        let length = units.iter().position(|unit| *unit == 0)?;
        Some(OsString::from_wide(&units[..length]))
    }

    /// The Windows folder, which lives on `%SystemDrive%`.
    #[allow(
        unsafe_code,
        reason = "ADR 0032: GetWindowsDirectoryW writes into a caller buffer"
    )]
    fn windows_folder() -> Result<PathBuf, String> {
        let mut folder = vec![0_u16; Self::PATH_UNITS];
        // SAFETY: `folder` is an exclusively borrowed buffer of exactly the
        // length passed, which the call writes a NUL-terminated path into.
        let length =
            unsafe { GetWindowsDirectoryW(folder.as_mut_ptr(), Self::units(folder.len())) };
        if length == 0 {
            return Err(format!(
                "the Windows folder cannot be read ({})",
                std::io::Error::last_os_error()
            ));
        }
        Ok(PathBuf::from(Self::text(&folder)))
    }

    /// A known folder's path, or `None` when Windows has none.
    #[allow(
        unsafe_code,
        reason = "ADR 0032: SHGetKnownFolderPath returns a string the caller frees with CoTaskMemFree"
    )]
    fn known_folder(id: &GUID) -> Option<PathBuf> {
        let mut text: PWSTR = null_mut();
        // SAFETY: `id` is a borrowed GUID that outlives the call; no flags and
        // no token (the current user); `text` is an exclusively borrowed
        // pointer the call sets.
        let result = unsafe { SHGetKnownFolderPath(id, 0, null_mut(), &raw mut text) };
        let path = (result == S_OK && !text.is_null()).then(|| Self::owned_string(text));
        // SAFETY: the call allocated `text` (or left it null), and it must be
        // freed with CoTaskMemFree whether or not the call succeeded; it is
        // not used after this.
        unsafe { CoTaskMemFree(text.cast_const().cast()) };
        path.map(PathBuf::from)
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0032: reads the NUL-terminated string SHGetKnownFolderPath returned"
    )]
    fn owned_string(text: PWSTR) -> OsString {
        let mut length = 0_usize;
        // SAFETY: `text` is the non-null, NUL-terminated string the call
        // returned and still owns until CoTaskMemFree; the search stops at
        // the NUL or at `PATH_UNITS`, the longest path Windows has.
        while length < Self::PATH_UNITS && unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: the `length` units read above are initialized and inside
        // the string.
        let units = unsafe { std::slice::from_raw_parts(text.cast_const(), length) };
        OsString::from_wide(units)
    }

    /// A volume opened with no access rights, which device queries allow.
    fn open_volume(volume: &str) -> std::io::Result<File> {
        OpenOptions::new()
            .access_mode(0)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(volume.trim_end_matches('\\'))
    }

    /// `C:\x` for `\\?\C:\x`: the volume calls expect the plain form.
    fn plain(path: &Path) -> PathBuf {
        let text = path.to_string_lossy();
        let rest = text.strip_prefix(Self::VERBATIM_PREFIX).unwrap_or(&text);
        let drive = rest.as_bytes().get(1) == Some(&b':');
        if drive {
            return PathBuf::from(rest);
        }
        path.to_path_buf()
    }

    fn wide(path: &Path) -> Vec<u16> {
        path.as_os_str().encode_wide().chain(Some(0)).collect()
    }

    fn text(units: &[u16]) -> String {
        let end = units
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(units.len());
        String::from_utf16_lossy(&units[..end])
    }

    fn units(length: usize) -> u32 {
        u32::try_from(length).unwrap_or(u32::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::WindowsDisks;
    use std::path::Path;

    #[test]
    fn the_temp_folder_is_on_the_system_disk() {
        let system = WindowsDisks::system().unwrap();
        let temp =
            WindowsDisks::describe(&std::fs::canonicalize(std::env::temp_dir()).unwrap()).unwrap();
        assert!(temp.shares_disk_with(&system), "{temp:?} {system:?}");
        assert!(
            system.keys.iter().any(|key| key.starts_with("disk:")),
            "{system:?}"
        );
    }

    #[test]
    fn the_system_volume_is_not_a_virtual_disk() {
        let system = WindowsDisks::system().unwrap();
        assert_eq!(
            WindowsDisks::images(&system).unwrap(),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[test]
    fn a_verbatim_drive_path_is_made_plain() {
        assert_eq!(
            WindowsDisks::plain(Path::new(r"\\?\C:\Users\me")),
            Path::new(r"C:\Users\me")
        );
        assert_eq!(
            WindowsDisks::plain(Path::new(r"D:\data")),
            Path::new(r"D:\data")
        );
    }

    #[test]
    fn the_app_data_folders_are_known() {
        let (local, roaming) = WindowsDisks::app_data_folders();
        assert!(local.unwrap().is_absolute());
        assert!(roaming.unwrap().is_absolute());
    }
}
