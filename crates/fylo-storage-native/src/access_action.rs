use serde::{Deserialize, Serialize};

/// What a policy rule allows (ADR 0021). `write` includes `read`, and
/// `admin` includes both.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccessAction {
    /// Read documents and metadata, and run queries.
    Read,
    /// Put, patch and delete documents.
    Write,
    /// Change the policy, create and drop collections.
    Admin,
}
