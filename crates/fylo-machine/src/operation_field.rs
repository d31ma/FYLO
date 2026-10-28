use serde::Deserialize;

/// Just the `op` of a request frame; every other field is skipped.
#[derive(Deserialize)]
pub(crate) struct OperationField {
    pub(crate) op: Option<String>,
}
