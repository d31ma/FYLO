/// Which setting turned the system-disk opt-out on (ADR 0032 section 3.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptOutSource {
    /// `--allow-system-disk` on the command line.
    Flag,
    /// `FYLO_ALLOW_SYSTEM_DISK=1` (or `true`) in the environment.
    Environment,
    /// `fylo.allowSystemDisk: true` in `application.json`.
    Config,
}

impl OptOutSource {
    /// The setting as a person would type it, for the stderr warning.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Flag => "--allow-system-disk",
            Self::Environment => "FYLO_ALLOW_SYSTEM_DISK=1",
            Self::Config => "fylo.allowSystemDisk",
        }
    }
}
