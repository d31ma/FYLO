use std::ffi::OsStr;
use std::fs::Metadata;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::path::Path;
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_SUCCESS, HANDLE, LocalFree};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
    GetNamedSecurityInfoW, SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_SIZE_INFORMATION, AclSizeInformation,
    DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetAclInformation, GetTokenInformation,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_INFORMATION_CLASS, TOKEN_OWNER, TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateDirectoryW, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, GetDiskFreeSpaceExW,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// The Windows calls behind a private temp cache folder (ADR 0030 section
/// 4.2): a folder created with a protected DACL that grants only the
/// current user, the owner and DACL checks of an existing folder, and free
/// space. ADR 0031: each unsafe
/// block has a `SAFETY` comment and an entry in
/// `docs/security/unsafe-inventory.md`.
pub(crate) struct WindowsFolder;

impl WindowsFolder {
    /// Open a folder itself, not the target of a reparse point.
    pub(crate) const FOLDER_FLAGS: u32 = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
    /// Open a file itself, not the target of a reparse point.
    pub(crate) const FILE_FLAGS: u32 = FILE_FLAG_OPEN_REPARSE_POINT;
    const ALLOWED_ACE: u8 = 0;
    const DENIED_ACE: u8 = 1;
    const PRIVATE_SDDL_START: &'static str = "D:P(A;OICI;FA;;;";
    const PRIVATE_SDDL_END: &'static str = ")";
    const WORD_BYTES: usize = 8;

    pub(crate) fn is_reparse_point(metadata: &Metadata) -> bool {
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }

    /// Create the folder `path`, private from its first moment: its
    /// security descriptor is passed to the create call, so it never has
    /// its parent's DACL. The DACL is protected and grants full control to
    /// the current user alone, inherited by everything created below it.
    /// It fails with `AlreadyExists` when anything has that name.
    pub(crate) fn create_private(path: &Path) -> io::Result<()> {
        let user = Self::with_token_sid(TokenUser, Self::sid_text)?;
        let sddl = format!(
            "{}{user}{}",
            Self::PRIVATE_SDDL_START,
            Self::PRIVATE_SDDL_END
        );
        Self::with_descriptor(&sddl, |descriptor| Self::create_with(path, descriptor))
    }

    /// Whether `path` is owned by the current user (its user or its
    /// token's default owner) and its DACL grants only the current user.
    pub(crate) fn is_private(path: &Path) -> io::Result<bool> {
        let information = OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION;
        Self::with_security(path, information, |owner, dacl| {
            Ok(Self::is_current_owner(owner)? && Self::grants_only_user(dacl)?)
        })
    }

    /// Whether `path` is owned by the current user or the token's default
    /// owner, whatever its DACL says.
    #[cfg(test)]
    pub(crate) fn owned_by_current_user(path: &Path) -> io::Result<bool> {
        Self::with_security(path, OWNER_SECURITY_INFORMATION, |owner, _| {
            Self::is_current_owner(owner)
        })
    }

    /// Whether the DACL of `path` grants only the current user.
    #[cfg(test)]
    pub(crate) fn grants_only_current_user(path: &Path) -> io::Result<bool> {
        Self::with_security(path, DACL_SECURITY_INFORMATION, |_, dacl| {
            Self::grants_only_user(dacl)
        })
    }

    /// Bytes the current user may still write on the volume of `path`.
    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: GetDiskFreeSpaceExW fills a caller u64"
    )]
    pub(crate) fn free_bytes(path: &Path) -> io::Result<u64> {
        let wide = Self::wide(path.as_os_str());
        let mut available = 0_u64;
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call; `available` is an exclusively borrowed u64 the call writes;
        // the two other outputs are null, which the call allows.
        let done = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &raw mut available, null_mut(), null_mut())
        };
        Self::check(done)?;
        Ok(available)
    }

    fn is_current_owner(owner: PSID) -> io::Result<bool> {
        let user = Self::with_token_sid(TokenUser, |user| Ok(Self::same_sid(owner, user)))?;
        let default =
            Self::with_token_sid(TokenOwner, |default| Ok(Self::same_sid(owner, default)))?;
        Ok(user || default)
    }

    fn grants_only_user(dacl: *const ACL) -> io::Result<bool> {
        if dacl.is_null() {
            return Ok(false);
        }
        Self::with_token_sid(TokenUser, |user| Self::every_ace_private(dacl, user))
    }

    fn every_ace_private(dacl: *const ACL, user: PSID) -> io::Result<bool> {
        for index in 0..Self::ace_count(dacl)? {
            if !Self::ace_is_private(dacl, index, user)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: GetAclInformation fills a caller struct"
    )]
    fn ace_count(dacl: *const ACL) -> io::Result<u32> {
        let mut size = ACL_SIZE_INFORMATION::default();
        let length = u32::try_from(std::mem::size_of::<ACL_SIZE_INFORMATION>()).unwrap_or(u32::MAX);
        // SAFETY: `dacl` is a non-null DACL inside a security descriptor the
        // caller keeps alive for the whole call; `size` is an exclusively
        // borrowed struct of exactly `length` bytes, which outlives the call.
        let done =
            unsafe { GetAclInformation(dacl, (&raw mut size).cast(), length, AclSizeInformation) };
        Self::check(done)?;
        Ok(size.AceCount)
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: GetAce returns a pointer into the DACL"
    )]
    fn ace_is_private(dacl: *const ACL, index: u32, user: PSID) -> io::Result<bool> {
        let mut ace = null_mut();
        // SAFETY: `dacl` is a live, non-null DACL and `index` is below its
        // ACE count; `ace` is an exclusively borrowed pointer slot the call
        // writes with an address inside the DACL.
        let done = unsafe { GetAce(dacl, index, &raw mut ace) };
        Self::check(done)?;
        // SAFETY: GetAce succeeded, so `ace` points at a whole ACE inside
        // the live DACL, and every ACE starts with an `ACE_HEADER`.
        let kind = unsafe { (*ace.cast::<ACE_HEADER>()).AceType };
        Ok(Self::kind_is_private(kind, ace, user))
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: the SID of an access-allowed ACE starts at SidStart"
    )]
    fn kind_is_private(kind: u8, ace: *mut core::ffi::c_void, user: PSID) -> bool {
        match kind {
            Self::DENIED_ACE => true,
            Self::ALLOWED_ACE => {
                // SAFETY: an ACE of type ACCESS_ALLOWED_ACE_TYPE is an
                // `ACCESS_ALLOWED_ACE` whose SID begins at `SidStart` and
                // lies inside the live DACL.
                let sid = unsafe { (&raw mut (*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart).cast() };
                Self::same_sid(sid, user)
            }
            _ => false,
        }
    }

    #[allow(unsafe_code, reason = "ADR 0004/0031: EqualSid reads two SIDs")]
    fn same_sid(first: PSID, second: PSID) -> bool {
        // SAFETY: both SIDs are valid and live for the call: one from a
        // security descriptor or ACE the caller keeps alive, one from a
        // token buffer the caller keeps alive.
        unsafe { EqualSid(first, second) != 0 }
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: GetNamedSecurityInfoW allocates a descriptor the caller frees"
    )]
    fn with_security<T>(
        path: &Path,
        information: u32,
        use_it: impl FnOnce(PSID, *const ACL) -> io::Result<T>,
    ) -> io::Result<T> {
        let wide = Self::wide(path.as_os_str());
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 path that outlives the
        // call; the owner, DACL and descriptor slots are exclusively
        // borrowed and written by the call; the group and SACL slots are
        // null, which the call allows. The owner and DACL point inside the
        // descriptor, which is freed only after `use_it` returns.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                information,
                &raw mut owner,
                null_mut(),
                &raw mut dacl,
                null_mut(),
                &raw mut descriptor,
            )
        };
        Self::check_status(status)?;
        let result = use_it(owner, dacl);
        // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW with
        // LocalAlloc, is freed exactly once, and nothing uses it afterwards.
        unsafe { LocalFree(descriptor) };
        result
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: the SDDL conversion allocates a descriptor the caller frees"
    )]
    fn with_descriptor<T>(
        sddl: &str,
        use_it: impl FnOnce(PSECURITY_DESCRIPTOR) -> io::Result<T>,
    ) -> io::Result<T> {
        let wide = Self::wide(OsStr::new(sddl));
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the
        // call; `descriptor` is an exclusively borrowed slot the call
        // writes; the size output is null, which the call allows.
        let done = unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide.as_ptr(),
                SDDL_REVISION_1,
                &raw mut descriptor,
                null_mut(),
            )
        };
        Self::check(done)?;
        let result = use_it(descriptor);
        // SAFETY: `descriptor` was allocated by the conversion with
        // LocalAlloc, is freed exactly once, and nothing uses it afterwards.
        unsafe { LocalFree(descriptor) };
        result
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: CreateDirectoryW reads caller security attributes"
    )]
    fn create_with(path: &Path, descriptor: PSECURITY_DESCRIPTOR) -> io::Result<()> {
        let wide = Self::wide(path.as_os_str());
        let attributes = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(u32::MAX),
            lpSecurityDescriptor: descriptor,
            bInheritHandle: 0,
        };
        // SAFETY: `wide` is a NUL-terminated UTF-16 path and `attributes` a
        // fully initialized struct of the size it states, pointing at a live
        // self-relative descriptor the caller frees only after this call;
        // both outlive the call, which only reads them and keeps no pointer.
        let done = unsafe { CreateDirectoryW(wide.as_ptr(), &raw const attributes) };
        Self::check(done)
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: the token calls fill caller buffers"
    )]
    fn with_token_sid<T>(
        class: TOKEN_INFORMATION_CLASS,
        use_it: impl FnOnce(PSID) -> io::Result<T>,
    ) -> io::Result<T> {
        let mut token: HANDLE = null_mut();
        // SAFETY: the pseudo handle of the current process needs no
        // closing; `token` is an exclusively borrowed slot the call writes.
        let done = unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) };
        Self::check(done)?;
        let result = Self::token_buffer(token, class).and_then(|buffer| {
            let sid = Self::sid_in(&buffer, class);
            use_it(sid)
        });
        // SAFETY: `token` was opened above, is closed exactly once, and
        // nothing uses it afterwards.
        unsafe { CloseHandle(token) };
        result
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: GetTokenInformation fills a caller buffer"
    )]
    fn token_buffer(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
        let mut needed = 0_u32;
        // SAFETY: a null buffer of length zero asks only for the size, which
        // the call writes to the exclusively borrowed `needed`.
        unsafe { GetTokenInformation(token, class, null_mut(), 0, &raw mut needed) };
        let words = usize::try_from(needed).unwrap_or(0) / Self::WORD_BYTES + 1;
        let mut buffer = vec![0_u64; words];
        let length = u32::try_from(words * Self::WORD_BYTES).unwrap_or(u32::MAX);
        // SAFETY: `buffer` is an exclusively borrowed, 8-byte aligned buffer
        // of `length` bytes, at least the size the first call asked for;
        // `needed` is an exclusively borrowed u32. Both outlive the call.
        let done = unsafe {
            GetTokenInformation(
                token,
                class,
                buffer.as_mut_ptr().cast(),
                length,
                &raw mut needed,
            )
        };
        Self::check(done)?;
        Ok(buffer)
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: a token buffer starts with TOKEN_USER or TOKEN_OWNER"
    )]
    fn sid_in(buffer: &[u64], class: TOKEN_INFORMATION_CLASS) -> PSID {
        if class == TokenOwner {
            // SAFETY: GetTokenInformation wrote a `TOKEN_OWNER` at the start
            // of this aligned buffer; its SID points inside the buffer,
            // which the caller keeps alive while it uses the SID.
            return unsafe { (*buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner };
        }
        // SAFETY: as above, for the `TOKEN_USER` every other class this
        // type asks for writes.
        unsafe { (*buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    #[allow(
        unsafe_code,
        reason = "ADR 0004/0031: ConvertSidToStringSidW allocates a string the caller frees"
    )]
    fn sid_text(sid: PSID) -> io::Result<String> {
        let mut text = null_mut();
        // SAFETY: `sid` is a valid SID from a live token buffer; `text` is an
        // exclusively borrowed slot the call writes.
        let done = unsafe { ConvertSidToStringSidW(sid, &raw mut text) };
        Self::check(done)?;
        let mut length = 0;
        // SAFETY: the call wrote a NUL-terminated UTF-16 string; it is read
        // up to its NUL, never past it, before it is freed.
        while unsafe { *text.add(length) } != 0 {
            length += 1;
        }
        // SAFETY: `text` holds `length` initialized UTF-16 units.
        let units = unsafe { std::slice::from_raw_parts(text, length) };
        let sid = String::from_utf16_lossy(units);
        // SAFETY: `text` was allocated by ConvertSidToStringSidW with
        // LocalAlloc, is freed exactly once, and nothing uses it afterwards.
        unsafe { LocalFree(text.cast()) };
        Ok(sid)
    }

    fn wide(text: &OsStr) -> Vec<u16> {
        text.encode_wide().chain(std::iter::once(0)).collect()
    }

    fn check(done: windows_sys::core::BOOL) -> io::Result<()> {
        if done == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    fn check_status(status: u32) -> io::Result<()> {
        if status == ERROR_SUCCESS {
            return Ok(());
        }
        Err(io::Error::from_raw_os_error(
            i32::try_from(status).unwrap_or(i32::MAX),
        ))
    }
}
