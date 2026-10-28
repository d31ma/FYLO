use std::fmt;
use std::io;
use std::path::Path;

/// The shared secret every client must present before its first frame.
///
/// It is never printed, and comparing it takes the same time for every
/// candidate of the right length.
#[derive(Clone)]
pub struct ServeToken {
    secret: Vec<u8>,
}

impl ServeToken {
    /// Short secrets can be guessed over the network.
    pub const MIN_BYTES: usize = 32;
    const MAX_FILE_BYTES: u64 = 4096;
    #[cfg(unix)]
    const SHARED_PERMISSIONS: u32 = 0o077;

    /// Use `secret` after checking its length.
    ///
    /// # Errors
    /// Rejects a secret shorter than [`Self::MIN_BYTES`] or containing a
    /// line break, which would end the hello frame early.
    pub fn new(secret: impl Into<Vec<u8>>) -> io::Result<Self> {
        let secret = secret.into();
        Self::require_length(&secret)?;
        Self::require_single_line(&secret)?;
        Ok(Self { secret })
    }

    /// Read the secret from a file, ignoring one trailing line break.
    ///
    /// On Unix the file must not be readable by group or others.
    ///
    /// # Errors
    /// Rejects an unreadable, oversized, shared or too-short secret file.
    pub fn from_file(path: &Path) -> io::Result<Self> {
        let metadata = fylo_vfs::metadata(path)?;
        Self::require_private(&metadata)?;
        Self::require_small(metadata.len())?;
        let text = fylo_vfs::read(path)?;
        Self::new(text.trim_ascii_end())
    }

    /// The secret itself, for a client that has to send it.
    #[must_use]
    pub fn secret(&self) -> &[u8] {
        &self.secret
    }

    pub(crate) fn matches(&self, candidate: &[u8]) -> bool {
        let same_length = candidate.len() == self.secret.len();
        let difference = self
            .secret
            .iter()
            .zip(candidate)
            .fold(0_u8, |difference, (left, right)| {
                difference | (left ^ right)
            });
        same_length && difference == 0
    }

    fn require_length(secret: &[u8]) -> io::Result<()> {
        if secret.len() >= Self::MIN_BYTES {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("the serve token must be at least {} bytes", Self::MIN_BYTES),
        ))
    }

    fn require_single_line(secret: &[u8]) -> io::Result<()> {
        if !secret.contains(&b'\n') && !secret.contains(&b'\r') {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the serve token must be one line",
        ))
    }

    fn require_small(length: u64) -> io::Result<()> {
        if length <= Self::MAX_FILE_BYTES {
            return Ok(());
        }
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the serve token file is too large",
        ))
    }

    #[cfg(unix)]
    fn require_private(metadata: &std::fs::Metadata) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & Self::SHARED_PERMISSIONS != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "the serve token file must not be readable by group or others (chmod 600)",
            ));
        }
        Ok(())
    }

    #[cfg(not(unix))]
    #[allow(clippy::unnecessary_wraps)]
    fn require_private(_metadata: &std::fs::Metadata) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Debug for ServeToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ServeToken(<redacted>)")
    }
}
