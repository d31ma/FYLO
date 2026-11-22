use serde::{Deserialize, Serialize};

/// The manifest of a namespace log checkpoint record. The record that opens
/// the log names no collection; a later one says a collection checkpointed
/// its own log at `generation`, so every part at or below it is durable in
/// that collection's files.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct NamespaceMark {
    pub(crate) format: String,
    pub(crate) collection: Option<String>,
    pub(crate) generation: u64,
}

impl NamespaceMark {
    pub(crate) const FORMAT: &'static str = "fylo.log-namespace-mark.v1";

    pub(crate) fn opening() -> Self {
        Self {
            format: Self::FORMAT.into(),
            collection: None,
            generation: 0,
        }
    }

    pub(crate) fn checkpointed(collection: &str, generation: u64) -> Self {
        Self {
            format: Self::FORMAT.into(),
            collection: Some(collection.to_owned()),
            generation,
        }
    }
}
