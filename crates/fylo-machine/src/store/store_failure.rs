use fylo_engine::EngineError;
use fylo_query::{QueryError, QueryErrorCode};
use fylo_storage_native::NativeStorageErrorCode;

use crate::{MachineError, engine_error};

/// How the Store operations turn lower-level failures into machine errors
/// (`api/store/v1`, "Error codes").
pub(crate) struct StoreFailure;

impl StoreFailure {
    pub(crate) const NOT_FOUND: &'static str = "ENATIVE_NOT_FOUND";
    pub(crate) const BAD_REQUEST: &'static str = "EBADREQUEST";
    pub(crate) const SCOPE: &'static str = "ETXNSCOPE";
    pub(crate) const LAYOUT: &'static str = "ETXNLAYOUT";

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

    pub(crate) fn is_missing(error: &EngineError) -> bool {
        error.storage_code() == Some(NativeStorageErrorCode::NotFound)
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

    pub(crate) fn require_existing(
        collection: &str,
        id: &str,
        existed: bool,
        require_existing: bool,
    ) -> Result<(), MachineError> {
        if require_existing && !existed {
            return Err(Self::missing_document(collection, Some(id)));
        }
        Ok(())
    }

    /// A transaction covers one collection; any other is out of its scope.
    pub(crate) fn require_scope(held: &str, asked: &str) -> Result<(), MachineError> {
        if held == asked {
            return Ok(());
        }
        Err(Self::scope(&format!(
            "the transaction covers collection {held}, not {asked}; it was rolled back"
        )))
    }

    pub(crate) fn scope(message: &str) -> MachineError {
        MachineError::new(Self::SCOPE, message)
    }

    pub(crate) fn layout() -> MachineError {
        MachineError::new(
            Self::LAYOUT,
            "this root's filesystem profile has no commit log, so it cannot hold a transaction",
        )
    }

    /// Whether a failure inside a transaction rolls it back.
    pub(crate) fn ends_transaction(error: &MachineError) -> bool {
        error.code == Self::SCOPE || error.code == super::store_transaction::StoreTransaction::LIMIT
    }
}
