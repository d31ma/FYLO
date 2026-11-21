use std::time::Duration;

use fylo_storage_native::FilesystemProfile;
use serde_json::{Value, json};

use super::store_batch_write::StoreBatchWrite;
use super::store_failure::StoreFailure;
use super::store_operations::StoreOperations;
use super::store_request::StoreRequest;
use super::store_transaction::StoreTransaction;
use super::store_transactions::StoreTransactions;
use crate::{MachineError, Session, require_string};

/// One-collection transactions (`api/store/v1`, "Transactions"; ADR 0035
/// section 5): a batch in one frame, or begin, Store operations with the
/// handle, and commit or rollback. A commit writes one log record.
impl Session {
    const DEFAULT_TIMEOUT_MS: u64 = 30_000;
    const MAX_TIMEOUT_MS: u64 = 300_000;
    const BATCH_TIMEOUT_MS: u64 = 300_000;

    pub(crate) fn store_transaction_operation(
        &self,
        operation: &str,
        request: &Value,
    ) -> Result<Value, MachineError> {
        match operation {
            "transactionBegin" => self.transaction_begin(request),
            "transactionCommit" => self.transaction_commit(request),
            "transactionRollback" => self.transaction_rollback(request),
            _ => self.store_batch(request),
        }
    }

    /// A frame that names a transaction but is not a Store operation, such
    /// as a queue or schema operation, is out of the transaction's scope:
    /// it is refused and the transaction rolls back.
    pub(crate) fn store_scope_refusal(
        &self,
        operation: &str,
        request: &Value,
    ) -> Result<(), MachineError> {
        let inside = StoreOperations::NAMES.contains(&operation)
            || StoreOperations::TRANSACTION_NAMES.contains(&operation);
        let handle = request.get("transaction").and_then(Value::as_str);
        match (inside, handle) {
            (false, Some(handle)) => {
                self.store_transactions.borrow_mut().abandon(handle);
                Err(StoreFailure::scope(&format!(
                    "{operation} cannot run inside a transaction; transaction {handle} was rolled back"
                )))
            }
            _ => Ok(()),
        }
    }

    fn transaction_begin(&self, request: &Value) -> Result<Value, MachineError> {
        let store = StoreRequest::parse(request)?;
        self.store_require_log()?;
        let timeout = Self::store_timeout(request)?;
        self.store_require_collection(&store)?;
        let mut transactions = self.store_transactions.borrow_mut();
        if transactions.holds(store.collection()) {
            return Err(StoreTransactions::busy(store.collection()));
        }
        let transaction = StoreTransaction::new(store.collection(), store.target(), timeout);
        Ok(json!({ "transaction": transactions.begin(transaction) }))
    }

    fn transaction_commit(&self, request: &Value) -> Result<Value, MachineError> {
        let handle = require_string(request, "transaction")?;
        let transaction = self.store_transactions.borrow_mut().end(handle)?;
        self.store_commit(&transaction)?;
        Ok(json!({ "committed": true, "writes": transaction.writes() }))
    }

    fn transaction_rollback(&self, request: &Value) -> Result<Value, MachineError> {
        let handle = require_string(request, "transaction")?;
        self.store_transactions.borrow_mut().end(handle)?;
        Ok(json!({ "rolledBack": true }))
    }

    /// `storeTransaction`: every write in order, then one commit. A failing
    /// write answers its own code with `details.index`, and nothing is
    /// written.
    fn store_batch(&self, request: &Value) -> Result<Value, MachineError> {
        let store = StoreRequest::parse(request)?;
        self.store_require_log()?;
        self.store_refuse_busy("storeSave", store.collection())?;
        let writes = Self::store_batch_writes(request)?;
        let timeout = Duration::from_millis(Self::BATCH_TIMEOUT_MS);
        let mut transaction = StoreTransaction::new(store.collection(), store.target(), timeout);
        let mut results = Vec::with_capacity(writes.len());
        for (index, write) in writes.iter().enumerate() {
            let result = self.store_batch_write(request, &mut transaction, write);
            results.push(result.map_err(|error| error.at_index(index))?);
        }
        self.store_commit(&transaction)?;
        Ok(Value::Array(results))
    }

    fn store_batch_write(
        &self,
        batch: &Value,
        transaction: &mut StoreTransaction,
        write: &Value,
    ) -> Result<Value, MachineError> {
        let (operation, frame) = StoreBatchWrite::frame(batch, write)?;
        let store = StoreRequest::parse(&frame)?;
        self.store_inside(operation, &store, transaction)
    }

    fn store_batch_writes(request: &Value) -> Result<&Vec<Value>, MachineError> {
        let writes = request
            .get("writes")
            .and_then(Value::as_array)
            .filter(|writes| !writes.is_empty())
            .ok_or_else(|| {
                StoreFailure::bad_request(
                    "machine request field \"writes\" must be a non-empty list",
                )
            })?;
        if writes.len() > StoreTransaction::MAX_WRITES {
            return Err(MachineError::new(
                StoreTransaction::LIMIT,
                format!(
                    "a transaction holds at most {} writes",
                    StoreTransaction::MAX_WRITES
                ),
            ));
        }
        Ok(writes)
    }

    fn store_commit(&self, transaction: &StoreTransaction) -> Result<(), MachineError> {
        self.write_engine(transaction.target())?
            .commit_document_writes(transaction.collection(), &transaction.document_writes())
            .map_err(|error| StoreFailure::engine(&error))
    }

    /// A transaction needs the commit log, which the EFS sidecar profile
    /// does not keep.
    fn store_require_log(&self) -> Result<(), MachineError> {
        if self.filesystem_profile == FilesystemProfile::Native {
            return Ok(());
        }
        Err(StoreFailure::layout())
    }

    fn store_require_collection(&self, store: &StoreRequest<'_>) -> Result<(), MachineError> {
        self.engine(store.frame())?
            .collection_kind(store.collection())
            .map(drop)
            .map_err(|error| StoreFailure::engine(&error))
    }

    fn store_timeout(request: &Value) -> Result<Duration, MachineError> {
        let millis = match request.get("timeoutMs") {
            None | Some(Value::Null) => Some(Self::DEFAULT_TIMEOUT_MS),
            Some(value) => value.as_u64(),
        };
        let millis = millis
            .filter(|millis| (1..=Self::MAX_TIMEOUT_MS).contains(millis))
            .ok_or_else(|| {
                StoreFailure::bad_request(format!(
                    "\"timeoutMs\" must be an integer from 1 to {}",
                    Self::MAX_TIMEOUT_MS
                ))
            })?;
        Ok(Duration::from_millis(millis))
    }
}
