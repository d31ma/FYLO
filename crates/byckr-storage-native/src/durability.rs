/// What an acknowledged write survives.
///
/// Every write is in the collection's write-ahead log before it returns.
/// This chooses how hard the log is flushed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Durability {
    /// Flush with `fsync`. Survives process death everywhere, and power loss
    /// where `fsync` reaches stable storage (Linux, Windows). On Apple
    /// platforms power loss can lose writes after the last checkpoint.
    #[default]
    Process,
    /// Also flush the drive's cache (`F_FULLFSYNC` on Apple platforms) on
    /// every group commit, so every acknowledged write survives power loss.
    PowerLoss,
}

impl Durability {
    /// Parse the `fylo.durability` setting.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "process" => Some(Self::Process),
            "power-loss" => Some(Self::PowerLoss),
            _ => None,
        }
    }
}
