use serde::Serialize;

/// What a policy rule allows (ADR 0021). `write` includes `read`, and
/// `admin` includes both.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessAction {
    /// Read documents and metadata, and run queries.
    Read,
    /// Put, patch and delete documents.
    Write,
    /// Change the policy, create and drop collections.
    Admin,
}

impl AccessAction {
    /// The action a policy names in lower case.
    pub(crate) fn from_name(name: &str) -> Result<AccessAction, String> {
        match name {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "admin" => Ok(Self::Admin),
            _ => Err(format!("{name:?} is not an action")),
        }
    }
}
