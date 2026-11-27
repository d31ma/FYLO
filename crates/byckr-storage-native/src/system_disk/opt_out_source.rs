/// Which setting turned the system-disk opt-out on (ADR 0032 section 3.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OptOutSource {
    /// `--allow-system-disk` on the command line.
    Flag,
    /// `YCKR_ALLOW_SYSTEM_DISK=1` (or `true`) in the environment.
    Environment,
    /// `yckr.allowSystemDisk: true` in `application.json`.
    Config,
}

impl OptOutSource {
    /// The setting as a person would type it, for the stderr warning.
    #[must_use]
    pub const fn describe(self) -> &'static str {
        match self {
            Self::Flag => "--allow-system-disk",
            Self::Environment => "YCKR_ALLOW_SYSTEM_DISK=1",
            Self::Config => "yckr.allowSystemDisk",
        }
    }
}
