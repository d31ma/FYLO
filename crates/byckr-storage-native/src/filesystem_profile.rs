use crate::{NativeStorageError, NativeStorageErrorCode};

/// Explicit filesystem metadata representation. Native remains the default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum FilesystemProfile {
    /// Existing native xattrs, Windows ADS and portable metadata behavior.
    #[default]
    Native,
    /// Opt-in journaled sidecars under a verified cooperative root owner.
    EfsSidecarV1,
}

impl FilesystemProfile {
    /// Parse an exact profile name before opening or publishing an owner.
    ///
    /// # Errors
    /// Returns an error for an unknown profile.
    pub fn parse(value: &str) -> Result<Self, NativeStorageError> {
        match value {
            "native" => Ok(Self::Native),
            "efs-sidecar-v1" => Ok(Self::EfsSidecarV1),
            _ => Err(NativeStorageError::new(
                NativeStorageErrorCode::Unsupported,
                "unknown FYLO filesystem profile",
            )),
        }
    }

    /// Stable profile name used by explicit host configuration.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::EfsSidecarV1 => "efs-sidecar-v1",
        }
    }
}
