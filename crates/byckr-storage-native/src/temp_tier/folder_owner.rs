use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;

/// The OS user a private cache folder must belong to: the process's
/// effective user, or another one a test names.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FolderOwner {
    id: u32,
}

impl FolderOwner {
    /// The process's effective user.
    #[must_use]
    pub fn current() -> Self {
        Self::with_id(rustix::process::geteuid().as_raw())
    }

    /// The user with the numeric id `id`.
    #[must_use]
    pub const fn with_id(id: u32) -> Self {
        Self { id }
    }

    /// The user's numeric id.
    #[must_use]
    pub const fn id(self) -> u32 {
        self.id
    }

    pub(crate) fn owns(self, metadata: &Metadata) -> bool {
        metadata.uid() == self.id
    }
}
