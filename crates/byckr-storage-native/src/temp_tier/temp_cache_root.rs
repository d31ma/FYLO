use std::path::{Path, PathBuf};

use crate::FilesystemProfile;

/// The root a session opened, as the temp tier's rules see it (ADR 0030
/// section 4.1).
#[derive(Clone, Debug)]
pub struct TempCacheRoot {
    path: Option<PathBuf>,
    profile: FilesystemProfile,
}

impl TempCacheRoot {
    /// The root at `path`, or none, opened with `profile`.
    #[must_use]
    pub fn new(path: Option<&Path>, profile: FilesystemProfile) -> Self {
        Self {
            path: path.map(Path::to_path_buf),
            profile,
        }
    }

    /// The root's path, if the session has one.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Whether the root is a `.vdbx` database.
    #[must_use]
    pub fn is_database(&self) -> bool {
        self.path
            .as_deref()
            .is_some_and(crate::NativeRoot::names_database)
    }

    /// Whether the root is remote or keeps its records in the sidecar
    /// format: the one fact `auto`'s first rule reads (ADR 0030 section
    /// 4.1). Today that is a root opened with the EFS profile, the only way
    /// a root may sit on a remote filesystem (ADR 0029); a later layout
    /// change may take it from another source, here alone.
    #[must_use]
    pub fn is_remote_or_sidecar(&self) -> bool {
        self.profile == FilesystemProfile::EfsSidecarV1
    }
}
