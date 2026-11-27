use std::ffi::OsStr;
use std::fs::{self, File, Metadata};
use std::io;
use std::path::{Component, Path, PathBuf};

use super::file_opening::FileOpening;
#[cfg(unix)]
use super::folder_owner::FolderOwner;
#[cfg(windows)]
use super::windows_folder::WindowsFolder;

/// A temp cache folder only its OS user can reach (ADR 0030 section 4.2).
///
/// It is created with mode `0700` (Windows: a protected DACL that grants
/// only the current user). An existing folder is used only when it is a
/// real folder, not a link or reparse point, owned by the process's user,
/// and (Unix) without group or other permission bits (Windows: its DACL
/// grants only the current user). BYCKR never changes a folder it does not
/// own.
///
/// Files below it are opened with no-follow flags: on Unix relative to the
/// open folder handle, one path part at a time, so a swapped link cannot
/// redirect a read or a write.
#[derive(Debug)]
pub struct PrivateFolder {
    path: PathBuf,
    #[cfg_attr(
        windows,
        expect(
            dead_code,
            reason = "Windows opens files by path; the handle keeps the checked folder open"
        )
    )]
    handle: File,
}

impl PrivateFolder {
    #[cfg(unix)]
    const PRIVATE_MODE: u32 = 0o700;
    #[cfg(unix)]
    const SHARED_BITS: u32 = 0o077;

    /// Open `path` as the current user's private folder, creating it when
    /// it is missing. Its parent must exist.
    ///
    /// # Errors
    /// `PermissionDenied` for a link, something that is not a folder,
    /// another owner, or a folder other users may reach; other OS errors as
    /// they come.
    pub fn open_or_create(path: &Path) -> io::Result<Self> {
        Self::create(path)?;
        Self::admit(path)
    }

    /// Open `path` as the current user's private folder, or `None` when it
    /// does not exist. Nothing is created.
    ///
    /// # Errors
    /// As [`Self::open_or_create`].
    pub fn open_existing(path: &Path) -> io::Result<Option<Self>> {
        Self::exists(path)?.then(|| Self::admit(path)).transpose()
    }

    /// Open `path`, creating it when missing, as the private folder of
    /// `owner` rather than of the process's user.
    ///
    /// # Errors
    /// As [`Self::open_or_create`].
    #[cfg(unix)]
    pub fn open_or_create_for(path: &Path, owner: FolderOwner) -> io::Result<Self> {
        Self::create(path)?;
        Self::admit_for(path, owner)
    }

    /// The folder's path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Open the file `relative` names below this folder for reading.
    ///
    /// # Errors
    /// `InvalidInput` for a path that is empty or leaves the folder; the OS
    /// error for a link in any part, or a missing file.
    pub fn open_file(&self, relative: &Path) -> io::Result<File> {
        self.open_below(relative, FileOpening::Read)
    }

    /// Create the new file `relative` names below this folder, mode `0600`,
    /// failing when anything already has that name.
    ///
    /// # Errors
    /// As [`Self::open_file`], and `AlreadyExists`.
    pub fn create_file(&self, relative: &Path) -> io::Result<File> {
        self.open_below(relative, FileOpening::CreateNew)
    }

    /// Open the file `relative` names below this folder for writing,
    /// creating it with mode `0600` or emptying it.
    ///
    /// # Errors
    /// As [`Self::open_file`].
    pub fn replace_file(&self, relative: &Path) -> io::Result<File> {
        self.open_below(relative, FileOpening::Replace)
    }

    /// Create every folder of `relative` below this folder that is
    /// missing, each private like this one.
    ///
    /// # Errors
    /// As [`Self::open_file`].
    pub fn create_folders(&self, relative: &Path) -> io::Result<()> {
        let names = Self::names(relative)?;
        self.create_each(&names)
    }

    /// Rename the file `from` to `to`, both in the folder `folder` names
    /// below this one, replacing any file named `to`.
    ///
    /// # Errors
    /// As [`Self::open_file`].
    pub fn rename_file(&self, folder: &Path, from: &str, to: &str) -> io::Result<()> {
        let parents = Self::names(folder)?;
        let from = Self::single_name(from)?;
        let to = Self::single_name(to)?;
        self.rename_below(&parents, from, to)
    }

    /// Remove the file `relative` names below this folder.
    ///
    /// # Errors
    /// As [`Self::open_file`].
    pub fn remove_file(&self, relative: &Path) -> io::Result<()> {
        let names = Self::names(relative)?;
        let (name, parents) = names.split_last().ok_or_else(Self::outside)?;
        self.remove_below(parents, name)
    }

    fn create(path: &Path) -> io::Result<()> {
        Self::ignore_existing(Self::make_folder(path))
    }

    fn ignore_existing(result: io::Result<()>) -> io::Result<()> {
        match result {
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            other => other,
        }
    }

    fn exists(path: &Path) -> io::Result<bool> {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
            other => other.map(|_| true),
        }
    }

    fn names(relative: &Path) -> io::Result<Vec<&OsStr>> {
        let names = relative
            .components()
            .map(Self::plain_name)
            .collect::<io::Result<Vec<_>>>()?;
        Self::require_some(names)
    }

    fn single_name(name: &str) -> io::Result<&OsStr> {
        let names = Self::names(Path::new(name))?;
        match names.as_slice() {
            [single] => Ok(single),
            _ => Err(Self::outside()),
        }
    }

    fn plain_name(component: Component<'_>) -> io::Result<&OsStr> {
        match component {
            Component::Normal(name) => Ok(name),
            _ => Err(Self::outside()),
        }
    }

    fn require_some(names: Vec<&OsStr>) -> io::Result<Vec<&OsStr>> {
        if names.is_empty() {
            return Err(Self::outside());
        }
        Ok(names)
    }

    fn outside() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a temp cache path must name a file below the cache folder",
        )
    }

    fn refused(path: &Path, why: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("temp cache folder {} {why}", path.display()),
        )
    }

    fn require_directory(path: &Path, metadata: &Metadata) -> io::Result<()> {
        if metadata.is_dir() {
            return Ok(());
        }
        Err(Self::refused(path, "is not a folder"))
    }
}

#[cfg(unix)]
impl PrivateFolder {
    fn make_folder(path: &Path) -> io::Result<()> {
        use std::os::unix::fs::DirBuilderExt;

        fs::DirBuilder::new().mode(Self::PRIVATE_MODE).create(path)
    }

    fn admit(path: &Path) -> io::Result<Self> {
        Self::admit_for(path, FolderOwner::current())
    }

    fn admit_for(path: &Path, owner: FolderOwner) -> io::Result<Self> {
        let listed = fs::symlink_metadata(path)?;
        Self::require_not_link(path, &listed)?;
        Self::require_directory(path, &listed)?;
        Self::require_owner(path, &listed, owner)?;
        Self::require_unshared(path, &listed)?;
        let handle = Self::open_handle(path)?;
        Self::require_same(path, &listed, &handle.metadata()?)?;
        Self::fill_owner_bits(&handle, &listed)?;
        Ok(Self {
            path: path.to_path_buf(),
            handle,
        })
    }

    fn require_not_link(path: &Path, metadata: &Metadata) -> io::Result<()> {
        if metadata.file_type().is_symlink() {
            return Err(Self::refused(path, "is a symbolic link"));
        }
        Ok(())
    }

    fn require_owner(path: &Path, metadata: &Metadata, owner: FolderOwner) -> io::Result<()> {
        if owner.owns(metadata) {
            return Ok(());
        }
        Err(Self::refused(path, "belongs to another user"))
    }

    fn require_unshared(path: &Path, metadata: &Metadata) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & Self::SHARED_BITS == 0 {
            return Ok(());
        }
        Err(Self::refused(path, "can be reached by other users"))
    }

    fn open_handle(path: &Path) -> io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(path)
    }

    fn require_same(path: &Path, listed: &Metadata, opened: &Metadata) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;

        if (listed.dev(), listed.ino()) == (opened.dev(), opened.ino()) {
            return Ok(());
        }
        Err(Self::refused(path, "changed while it was opened"))
    }

    fn fill_owner_bits(handle: &File, listed: &Metadata) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        if listed.permissions().mode() & Self::PRIVATE_MODE == Self::PRIVATE_MODE {
            return Ok(());
        }
        handle.set_permissions(fs::Permissions::from_mode(Self::PRIVATE_MODE))
    }

    fn open_below(&self, relative: &Path, opening: FileOpening) -> io::Result<File> {
        let names = Self::names(relative)?;
        let (name, parents) = names.split_last().ok_or_else(Self::outside)?;
        let parent = self.descend_all(parents)?;
        let flags = Self::flags(opening) | Self::NO_FOLLOW;
        let mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
        Ok(File::from(rustix::fs::openat(&parent, *name, flags, mode)?))
    }

    const NO_FOLLOW: rustix::fs::OFlags =
        rustix::fs::OFlags::NOFOLLOW.union(rustix::fs::OFlags::CLOEXEC);

    fn flags(opening: FileOpening) -> rustix::fs::OFlags {
        use rustix::fs::OFlags;

        match opening {
            FileOpening::Read => OFlags::RDONLY,
            FileOpening::CreateNew => OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
            FileOpening::Replace => OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        }
    }

    fn descend_all(&self, names: &[&OsStr]) -> io::Result<std::os::fd::OwnedFd> {
        use std::os::fd::AsFd;

        let mut folder = self.handle.as_fd().try_clone_to_owned()?;
        for name in names {
            folder = Self::descend(&folder, name)?;
        }
        Ok(folder)
    }

    fn descend(folder: &std::os::fd::OwnedFd, name: &OsStr) -> io::Result<std::os::fd::OwnedFd> {
        use rustix::fs::{Mode, OFlags};

        let flags = OFlags::RDONLY | OFlags::DIRECTORY | Self::NO_FOLLOW;
        Ok(rustix::fs::openat(folder, name, flags, Mode::empty())?)
    }

    fn rename_below(&self, parents: &[&OsStr], from: &OsStr, to: &OsStr) -> io::Result<()> {
        let parent = self.descend_all(parents)?;
        Ok(rustix::fs::renameat(&parent, from, &parent, to)?)
    }

    fn remove_below(&self, parents: &[&OsStr], name: &OsStr) -> io::Result<()> {
        let parent = self.descend_all(parents)?;
        Ok(rustix::fs::unlinkat(
            &parent,
            name,
            rustix::fs::AtFlags::empty(),
        )?)
    }

    fn create_each(&self, names: &[&OsStr]) -> io::Result<()> {
        use std::os::fd::AsFd;

        let mut folder = self.handle.as_fd().try_clone_to_owned()?;
        for name in names {
            Self::ignore_existing(Self::make_below(&folder, name))?;
            folder = Self::descend(&folder, name)?;
        }
        Ok(())
    }

    fn make_below(folder: &std::os::fd::OwnedFd, name: &OsStr) -> io::Result<()> {
        let mode = rustix::fs::Mode::RWXU;
        Ok(rustix::fs::mkdirat(folder, name, mode)?)
    }
}

#[cfg(windows)]
impl PrivateFolder {
    fn make_folder(path: &Path) -> io::Result<()> {
        WindowsFolder::create_private(path)
    }

    fn admit(path: &Path) -> io::Result<Self> {
        let listed = fs::symlink_metadata(path)?;
        Self::require_not_reparse(path, &listed)?;
        Self::require_directory(path, &listed)?;
        Self::require_private(path)?;
        let handle = Self::open_handle(path)?;
        Self::require_not_reparse(path, &handle.metadata()?)?;
        Ok(Self {
            path: path.to_path_buf(),
            handle,
        })
    }

    fn require_not_reparse(path: &Path, metadata: &Metadata) -> io::Result<()> {
        if WindowsFolder::is_reparse_point(metadata) {
            return Err(Self::refused(path, "is a link or reparse point"));
        }
        Ok(())
    }

    fn require_private(path: &Path) -> io::Result<()> {
        if WindowsFolder::is_private(path)? {
            return Ok(());
        }
        Err(Self::refused(
            path,
            "belongs to another user or grants access to other accounts",
        ))
    }

    fn open_handle(path: &Path) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(WindowsFolder::FOLDER_FLAGS)
            .open(path)
    }

    fn open_below(&self, relative: &Path, opening: FileOpening) -> io::Result<File> {
        use std::os::windows::fs::OpenOptionsExt;

        let path = self.checked_path(relative)?;
        let file = Self::options(opening)
            .custom_flags(WindowsFolder::FILE_FLAGS)
            .open(&path)?;
        Self::require_not_reparse(&path, &file.metadata()?)?;
        Ok(file)
    }

    fn options(opening: FileOpening) -> fs::OpenOptions {
        let mut options = fs::OpenOptions::new();
        match opening {
            FileOpening::Read => options.read(true),
            FileOpening::CreateNew => options.write(true).create_new(true),
            FileOpening::Replace => options.write(true).create(true).truncate(true),
        };
        options
    }

    fn rename_below(&self, parents: &[&OsStr], from: &OsStr, to: &OsStr) -> io::Result<()> {
        let folder = self.checked_folder(parents)?;
        fs::rename(folder.join(from), folder.join(to))
    }

    fn remove_below(&self, parents: &[&OsStr], name: &OsStr) -> io::Result<()> {
        let path = self.checked_folder(parents)?.join(name);
        Self::require_not_reparse(&path, &fs::symlink_metadata(&path)?)?;
        fs::remove_file(path)
    }

    fn checked_folder(&self, parents: &[&OsStr]) -> io::Result<PathBuf> {
        let mut path = self.path.clone();
        for parent in parents {
            path.push(parent);
            Self::require_plain_folder(&path)?;
        }
        Ok(path)
    }

    fn checked_path(&self, relative: &Path) -> io::Result<PathBuf> {
        let names = Self::names(relative)?;
        let (name, parents) = names.split_last().ok_or_else(Self::outside)?;
        let mut path = self.path.clone();
        for parent in parents {
            path.push(parent);
            Self::require_plain_folder(&path)?;
        }
        path.push(name);
        Ok(path)
    }

    fn require_plain_folder(path: &Path) -> io::Result<()> {
        let metadata = fs::symlink_metadata(path)?;
        Self::require_not_reparse(path, &metadata)?;
        Self::require_directory(path, &metadata)
    }

    fn create_each(&self, names: &[&OsStr]) -> io::Result<()> {
        let mut path = self.path.clone();
        for name in names {
            path.push(name);
            Self::ignore_existing(fs::create_dir(&path))?;
            Self::require_plain_folder(&path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
