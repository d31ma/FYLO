use serde::{Deserialize, Serialize};

use super::namespace_part::NamespacePart;

/// The manifest of a namespace commit record: one transaction over several
/// collections of one namespace, committed by one append and one flush.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NamespaceManifest {
    pub(crate) format: String,
    pub(crate) parts: Vec<NamespacePart>,
}

impl NamespaceManifest {
    pub(crate) const FORMAT: &'static str = "fylo.log-namespace-commit.v1";

    pub(crate) fn new(parts: Vec<NamespacePart>) -> Self {
        Self {
            format: Self::FORMAT.into(),
            parts,
        }
    }
}
