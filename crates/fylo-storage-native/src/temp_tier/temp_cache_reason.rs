/// Why the temp tier is not active for a session (ADR 0030 section 4.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TempCacheReason {
    /// The session has no default root.
    NoRoot,
    /// The root is a `.vdbx` database, which never uses the tier.
    Vdbx,
    /// `auto` and a root opened with the native profile: it is on a local
    /// disk, so a temp folder is not faster.
    RootIsLocal,
    /// The cache folder is a link, belongs to another user, or others may
    /// reach it.
    UnsafeFolder,
    /// The cache folder is on a network filesystem, or one FYLO cannot name.
    CacheFolderRemote,
    /// `auto` and the cache folder is on the root's own device.
    SameDevice,
    /// The cache folder's filesystem has less than 64 MiB free.
    LowSpace,
    /// Every rule passed, but this release has no temp-tier read path yet.
    ReadPathNotBuilt,
}

impl TempCacheReason {
    /// The text the handshake reports.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoRoot => "no root",
            Self::Vdbx => "vdbx",
            Self::RootIsLocal => "root is local",
            Self::UnsafeFolder => "unsafe folder",
            Self::CacheFolderRemote => "cache folder is remote",
            Self::SameDevice => "same device",
            Self::LowSpace => "low space",
            Self::ReadPathNotBuilt => "read path not built",
        }
    }
}
