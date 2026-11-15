/// Why a root was admitted, as the handshake reports it in
/// `storage.rootLock.placement` (ADR 0032 section 6).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementKind {
    /// The root is on a local disk that is not the system disk.
    DataDisk,
    /// The opt-out admitted a root on the system disk.
    SystemDiskAllowed,
    /// The root is inside the app's own data folder.
    AppOwned,
}

impl PlacementKind {
    /// The handshake's name for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DataDisk => "dataDisk",
            Self::SystemDiskAllowed => "systemDiskAllowed",
            Self::AppOwned => "appOwned",
        }
    }
}
