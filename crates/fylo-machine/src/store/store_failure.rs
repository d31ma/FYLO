use fylo_engine::{EngineError, StoreChange};
use fylo_query::{QueryError, QueryErrorCode};

use crate::{MachineError, engine_error};

/// How the Store operations turn lower-level failures into machine errors
/// (`api/store/v1`, "Error codes").
pub(crate) struct StoreFailure;

impl StoreFailure {
    pub(crate) const NOT_FOUND: &'static str = "ENATIVE_NOT_FOUND";
    pub(crate) const BAD_REQUEST: &'static str = "EBADREQUEST";
    pub(crate) const SCOPE: &'static str = "ETXNSCOPE";
    pub(crate) const LAYOUT: &'static str = "ETXNLAYOUT";
    pub(crate) const LIMIT: &'static str = "ETXNLIMIT";

    pub(crate) fn query(error: &QueryError) -> MachineError {
        MachineError::new(error.code().as_str(), error.message())
    }

    pub(crate) fn page(error: &QueryError) -> MachineError {
        if error.code() == QueryErrorCode::InvalidQuery {
            return MachineError::new(Self::BAD_REQUEST, error.message());
        }
        Self::query(error)
    }

    pub(crate) fn engine(error: &EngineError) -> MachineError {
        engine_error(error)
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> MachineError {
        MachineError::new(Self::BAD_REQUEST, message)
    }

    pub(crate) fn missing_document(collection: &str, id: Option<&str>) -> MachineError {
        let named = id.map_or_else(|| "no id".to_owned(), |id| format!("document {id}"));
        MachineError::new(
            Self::NOT_FOUND,
            format!("collection {collection}: {named} does not exist, and requireExisting is true"),
        )
    }

    /// With `requireExisting`, a save that would create a document is
    /// refused and writes nothing.
    pub(crate) fn require_existing(
        collection: &str,
        id: &str,
        change: &StoreChange,
        require_existing: bool,
    ) -> Result<(), MachineError> {
        let creates = matches!(change, StoreChange::Put { existed: false, .. });
        if require_existing && creates {
            return Err(Self::missing_document(collection, Some(id)));
        }
        Ok(())
    }

    /// A transaction covers one namespace; any other is out of its scope
    /// (ADR 0035, Amendment on several collections).
    pub(crate) fn require_scope(
        held: &str,
        asked: &str,
        collection: &str,
    ) -> Result<(), MachineError> {
        if held == asked {
            return Ok(());
        }
        Err(Self::scope_for(
            collection,
            &format!("the transaction covers namespace {held}, not {asked}; it was rolled back"),
        ))
    }

    /// An out-of-scope frame that names a collection: the error also names it.
    pub(crate) fn scope_for(collection: &str, message: &str) -> MachineError {
        MachineError::new(Self::SCOPE, message).naming_collection(collection)
    }

    pub(crate) fn scope(message: &str) -> MachineError {
        MachineError::new(Self::SCOPE, message)
    }

    pub(crate) fn layout() -> MachineError {
        MachineError::new(
            Self::LAYOUT,
            "this collection does not write through a commit log (a raw-file collection, or a filesystem profile without one), so it cannot hold a transaction",
        )
    }

    /// A collection refused from a transaction over several collections
    /// (ADR 0035, Amendment on several collections).
    pub(crate) fn layout_for(collection: &str) -> MachineError {
        MachineError::new(
            Self::LAYOUT,
            format!(
                "collection {collection} cannot join a transaction over several collections; only a document collection whose writes go through the commit log (not a raw-file or segment collection) can"
            ),
        )
        .naming_collection(collection)
    }

    /// A transaction that passes its shared write or byte limit.
    pub(crate) fn limit() -> MachineError {
        MachineError::new(
            Self::LIMIT,
            "a transaction holds at most 1,000 writes and 8 MiB of documents; it was rolled back",
        )
    }

    /// Whether a failure inside a transaction rolls it back.
    pub(crate) fn ends_transaction(error: &MachineError) -> bool {
        error.code == Self::SCOPE || error.code == Self::LIMIT || error.code == Self::LAYOUT
    }
}
