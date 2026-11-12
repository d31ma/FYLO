use serde::Deserialize;

/// What kind of `.vdbx` database `fylo_open` makes when none exists.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub(crate) enum CreateMode {
    /// Not encrypted, no identities.
    Plain,
    /// Encrypted, with an admin identity and signed records (ADR 0021).
    Protected,
}
