use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use p256::SecretKey;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;
use crate::private_key_pem::PrivateKeyPem;

/// `file:<path>`: a PKCS#8 PEM file. On Unix it is created with mode `0600`,
/// and a file another user owns, or that group or other users can read or
/// write, is refused. Windows has no such check yet: its ACLs are left to the
/// Windows lane, and `cng:` is the store meant for Windows.
pub(crate) struct FileKeyStore {
    path: PathBuf,
}

impl FileKeyStore {
    #[cfg(unix)]
    const OWNER_ONLY_MODE: u32 = 0o600;
    #[cfg(unix)]
    const GROUP_AND_OTHER_BITS: u32 = 0o077;

    pub(crate) fn new(path: PathBuf) -> FileKeyStore {
        FileKeyStore { path }
    }

    fn reference(&self) -> String {
        format!("file:{}", self.path.display())
    }

    fn create_new_file(&self) -> Result<File, IdentityError> {
        Self::owner_only_options()
            .open(&self.path)
            .map_err(|error| self.io_error(&error))
    }

    fn open_existing_file(&self) -> Result<File, IdentityError> {
        File::open(&self.path).map_err(|error| self.io_error(&error))
    }

    fn read_text(&self, mut file: File) -> Result<Zeroizing<String>, IdentityError> {
        let mut text = Zeroizing::new(String::new());
        file.read_to_string(&mut text)
            .map_err(|error| self.io_error(&error))?;
        Ok(text)
    }

    fn write_and_sync(&self, mut file: File, text: &str) -> Result<(), IdentityError> {
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|error| self.io_error(&error))?;
        Self::sync_parent(&self.path).map_err(|error| self.io_error(&error))
    }

    fn io_error(&self, error: &io::Error) -> IdentityError {
        match error.kind() {
            io::ErrorKind::NotFound => {
                IdentityError::NotFound(format!("{} does not exist", self.reference()))
            }
            io::ErrorKind::AlreadyExists => IdentityError::AlreadyExists(format!(
                "{} already exists; FYLO never replaces a key",
                self.reference()
            )),
            _ => IdentityError::Store(format!("{}: {error}", self.reference())),
        }
    }

    #[cfg(unix)]
    fn owner_only_options() -> OpenOptions {
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create_new(true)
            .mode(Self::OWNER_ONLY_MODE);
        options
    }

    #[cfg(not(unix))]
    fn owner_only_options() -> OpenOptions {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        options
    }

    #[cfg(unix)]
    fn sync_parent(path: &Path) -> io::Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        File::open(parent)?.sync_all()
    }

    #[cfg(not(unix))]
    #[allow(clippy::unnecessary_wraps)]
    fn sync_parent(_path: &Path) -> io::Result<()> {
        Ok(())
    }

    #[cfg(unix)]
    fn refuse_exposed(&self, file: &File) -> Result<(), IdentityError> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata = file.metadata().map_err(|error| self.io_error(&error))?;
        let owner = rustix::process::geteuid().as_raw();
        Some(metadata.mode())
            .filter(|mode| mode & Self::GROUP_AND_OTHER_BITS == 0 && metadata.uid() == owner)
            .map(|_| ())
            .ok_or_else(|| {
                IdentityError::ExposedKeyFile(format!(
                    "{} has mode {:o} and owner {}; a key file must be owned by user {owner} and have mode 0600 or 0400",
                    self.reference(),
                    metadata.mode() & 0o7777,
                    metadata.uid(),
                ))
            })
    }

    #[cfg(not(unix))]
    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    fn refuse_exposed(&self, _file: &File) -> Result<(), IdentityError> {
        Ok(())
    }

    fn refuse_non_file(&self) -> Result<(), IdentityError> {
        fs::metadata(&self.path)
            .map_err(|error| self.io_error(&error))
            .and_then(|metadata| {
                Some(metadata)
                    .filter(fs::Metadata::is_file)
                    .map(|_| ())
                    .ok_or_else(|| {
                        IdentityError::InvalidKey(format!("{} is not a file", self.reference()))
                    })
            })
    }
}

impl KeyStore for FileKeyStore {
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError> {
        let text = PrivateKeyPem::encode(secret_key)?;
        let file = self.create_new_file()?;
        self.write_and_sync(file, &text)
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        self.refuse_non_file()?;
        let file = self.open_existing_file()?;
        self.refuse_exposed(&file)?;
        let text = self.read_text(file)?;
        PrivateKeyPem::decode(&text, &self.reference())
    }
}
