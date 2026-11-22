use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::attribute_limit::AttributeLimit;
use super::attribute_test::AttributeTest;
use super::probe_failure::ProbeFailure;
use super::probe_files::ProbeFiles;
use crate::layout::{MetadataStore, ProbeRecord};
use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode};

/// Tests what FYLO needs from the file system of an app folder (ADR 0033
/// section 6): exclusive create, rename over a file, extended attributes and
/// hard links. [`Self::run`] runs once when the app folder is created, and
/// its result is recorded in `format.json`. [`Self::verify`] runs at every
/// writing open and checks that the recorded way still works.
///
/// Every probe works on short-lived `.fylo-filesystem-probe-*.tmp` files in
/// the folder it is given (the app folder's `.fylo/`), and removes them
/// before it returns.
pub struct FilesystemProbe;

impl FilesystemProbe {
    const OLD_BYTES: &'static [u8] = b"old";
    const NEW_BYTES: &'static [u8] = b"new";

    /// Probe `directory` when its app folder is created, and choose where
    /// metadata lives: extended attributes when the file system keeps them,
    /// sidecar files otherwise. `forced` is the `--filesystem-profile`
    /// override for tests: `efs-sidecar-v1` always chooses sidecar files, and
    /// `native` requires extended attributes.
    ///
    /// # Errors
    ///
    /// Returns `EFSPROBE` when exclusive create or rename over a file does
    /// not work, or when `forced` is `native` and extended attributes do not
    /// work.
    pub fn run(
        directory: &Path,
        forced: Option<FilesystemProfile>,
    ) -> Result<ProbeRecord, NativeStorageError> {
        let mut files = ProbeFiles::new(directory);
        Self::check_exclusive_create(directory, &mut files)?;
        Self::check_rename_over(directory, &mut files)?;
        let attributes = AttributeTest::run(&mut files);
        let metadata = Self::choose_metadata(directory, forced, attributes)?;
        let attribute_limit = Self::attribute_limit(directory, &mut files, metadata)?;
        let hard_links = Self::hard_links_work(directory, &mut files)?;
        Ok(ProbeRecord::new(metadata, hard_links).with_max_attribute_bytes(attribute_limit))
    }

    /// Check, when an app folder is opened for writing, that the way
    /// `record` names still works, for example after the folder was copied
    /// to a share without extended attributes. Sidecar files need nothing
    /// beyond ordinary files, so only recorded attributes and hard links
    /// are tested.
    ///
    /// # Errors
    ///
    /// Returns `EFSPROBE` naming what no longer works.
    pub fn verify(directory: &Path, record: ProbeRecord) -> Result<(), NativeStorageError> {
        let mut files = ProbeFiles::new(directory);
        Self::verify_metadata(directory, &mut files, record.metadata())?;
        let hard_links = Self::hard_links_still_work(directory, &mut files, record.hard_links())?;
        Self::require_hard_links(directory, hard_links)
    }

    /// Refuse a `--filesystem-profile` override that differs from what an
    /// existing app folder recorded. No override always passes.
    ///
    /// # Errors
    ///
    /// Returns `EUNSUPPORTED` when `forced` names another profile.
    pub fn check_override(
        record: ProbeRecord,
        forced: Option<FilesystemProfile>,
    ) -> Result<(), NativeStorageError> {
        match forced {
            Some(profile) if profile != Self::profile_of(record.metadata()) => {
                Err(NativeStorageError::new(
                    NativeStorageErrorCode::Unsupported,
                    format!(
                        "--filesystem-profile {} differs from the metadata storage {} recorded in format.json",
                        profile.as_str(),
                        Self::name_of(record.metadata())
                    ),
                ))
            }
            _ => Ok(()),
        }
    }

    /// The engine profile that reads and writes metadata the way `store`
    /// names.
    #[must_use]
    pub const fn profile_of(store: MetadataStore) -> FilesystemProfile {
        Self::profile_for(store)
    }

    const fn profile_for(store: MetadataStore) -> FilesystemProfile {
        match store {
            MetadataStore::ExtendedAttributes => FilesystemProfile::Native,
            MetadataStore::SidecarFiles => FilesystemProfile::EfsSidecarV1,
        }
    }

    const fn name_of(store: MetadataStore) -> &'static str {
        match store {
            MetadataStore::ExtendedAttributes => "xattr",
            MetadataStore::SidecarFiles => "sidecar",
        }
    }

    fn check_exclusive_create(
        directory: &Path,
        files: &mut ProbeFiles,
    ) -> Result<(), NativeStorageError> {
        let (path, _file) = files
            .create_new()
            .map_err(|error| ProbeFailure::from_io("exclusive create", directory, &error))?;
        let second = OpenOptions::new().write(true).create_new(true).open(&path);
        Self::require_already_exists(directory, second)
    }

    fn require_already_exists(
        directory: &Path,
        second: std::io::Result<File>,
    ) -> Result<(), NativeStorageError> {
        match second {
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
            Err(error) => Err(ProbeFailure::from_io(
                "second exclusive create",
                directory,
                &error,
            )),
            Ok(_) => Err(ProbeFailure::with_detail(
                "exclusive create",
                directory,
                "a second exclusive create of the same file succeeded",
            )),
        }
    }

    fn check_rename_over(
        directory: &Path,
        files: &mut ProbeFiles,
    ) -> Result<(), NativeStorageError> {
        let target = Self::write_new(directory, files, Self::OLD_BYTES)?;
        let source = Self::write_new(directory, files, Self::NEW_BYTES)?;
        std::fs::rename(&source, &target)
            .map_err(|error| ProbeFailure::from_io("rename over a file", directory, &error))?;
        let bytes = std::fs::read(&target)
            .map_err(|error| ProbeFailure::from_io("read after rename", directory, &error))?;
        Self::require_replaced(directory, &bytes)
    }

    fn write_new(
        directory: &Path,
        files: &mut ProbeFiles,
        bytes: &[u8],
    ) -> Result<PathBuf, NativeStorageError> {
        let (path, mut file) = files
            .create_new()
            .map_err(|error| ProbeFailure::from_io("create a file", directory, &error))?;
        file.write_all(bytes)
            .map_err(|error| ProbeFailure::from_io("write a file", directory, &error))?;
        Ok(path)
    }

    fn require_replaced(directory: &Path, bytes: &[u8]) -> Result<(), NativeStorageError> {
        if bytes == Self::NEW_BYTES {
            return Ok(());
        }
        Err(ProbeFailure::with_detail(
            "rename over a file",
            directory,
            "the old contents remained",
        ))
    }

    fn choose_metadata(
        directory: &Path,
        forced: Option<FilesystemProfile>,
        attributes: std::io::Result<()>,
    ) -> Result<MetadataStore, NativeStorageError> {
        match (forced, attributes) {
            (Some(FilesystemProfile::EfsSidecarV1), _) | (None, Err(_)) => {
                Ok(MetadataStore::SidecarFiles)
            }
            (_, Ok(())) => Ok(MetadataStore::ExtendedAttributes),
            (Some(FilesystemProfile::Native), Err(error)) => Err(ProbeFailure::from_io(
                "extended attributes (required by --filesystem-profile native)",
                directory,
                &error,
            )),
        }
    }

    fn attribute_limit(
        directory: &Path,
        files: &mut ProbeFiles,
        metadata: MetadataStore,
    ) -> Result<Option<u32>, NativeStorageError> {
        match metadata {
            MetadataStore::SidecarFiles => Ok(None),
            MetadataStore::ExtendedAttributes => {
                AttributeLimit::measure(files).map(Some).map_err(|error| {
                    ProbeFailure::from_io("measure the largest attribute value", directory, &error)
                })
            }
        }
    }

    fn hard_links_work(
        directory: &Path,
        files: &mut ProbeFiles,
    ) -> Result<bool, NativeStorageError> {
        let (source, _file) = files
            .create_new()
            .map_err(|error| ProbeFailure::from_io("create a file to link", directory, &error))?;
        let link = files.reserve_path();
        Ok(std::fs::hard_link(&source, &link).is_ok())
    }

    fn verify_metadata(
        directory: &Path,
        files: &mut ProbeFiles,
        metadata: MetadataStore,
    ) -> Result<(), NativeStorageError> {
        match metadata {
            MetadataStore::SidecarFiles => Ok(()),
            MetadataStore::ExtendedAttributes => AttributeTest::run(files).map_err(|error| {
                ProbeFailure::from_io(
                    "extended attributes, recorded in format.json, no longer work",
                    directory,
                    &error,
                )
            }),
        }
    }

    fn hard_links_still_work(
        directory: &Path,
        files: &mut ProbeFiles,
        recorded: bool,
    ) -> Result<bool, NativeStorageError> {
        if !recorded {
            return Ok(true);
        }
        Self::hard_links_work(directory, files)
    }

    fn require_hard_links(directory: &Path, works: bool) -> Result<(), NativeStorageError> {
        if works {
            return Ok(());
        }
        Err(ProbeFailure::with_detail(
            "hard links, recorded in format.json",
            directory,
            "no longer work",
        ))
    }
}
