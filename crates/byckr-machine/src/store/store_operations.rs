use byckr_engine::{StoreChange, StoreOverlay};
use byckr_query::StoreQuery;
use serde_json::{Value, json};

use super::store_entity::StoreEntity;
use super::store_failure::StoreFailure;
use super::store_outcome::StoreOutcome;
use super::store_request::StoreRequest;
use super::store_transaction::StoreTransaction;
use super::store_transactions::StoreTransactions;
use super::store_view::StoreView;
use crate::{MachineError, Session, storage_error};

/// The names of the Store operations (`api/machine/v2`, "Added operations").
pub(crate) struct StoreOperations;

impl StoreOperations {
    pub(crate) const NAMES: [&'static str; 10] = [
        "storeFindById",
        "storeFindAll",
        "storeSave",
        "storePatch",
        "storeDeleteById",
        "storeFind",
        "storeCount",
        "storeExists",
        "storeDelete",
        "storeScroll",
    ];

    pub(crate) const TRANSACTION_NAMES: [&'static str; 4] = [
        "storeTransaction",
        "transactionBegin",
        "transactionCommit",
        "transactionRollback",
    ];

    const WRITE_NAMES: [&'static str; 4] =
        ["storeSave", "storePatch", "storeDeleteById", "storeDelete"];

    /// The handshake's `capabilities.store` version.
    pub(crate) const CAPABILITY_VERSION: u32 = 1;

    /// The handshake's `capabilities.store`: the contract version, plus the
    /// flag that a transaction may span several collections of one namespace
    /// (ADR 0035, Amendment on several collections).
    pub(crate) fn capability() -> Value {
        json!({
            "version": Self::CAPABILITY_VERSION,
            "multiCollectionTransactions": true,
        })
    }

    pub(crate) fn is_write(operation: &str) -> bool {
        Self::WRITE_NAMES.contains(&operation)
    }
}

impl Session {
    pub(crate) fn store_operation(
        &self,
        operation: &str,
        request: &Value,
    ) -> Result<Value, MachineError> {
        let store = StoreRequest::parse(request)?;
        match store.transaction()? {
            Some(handle) => self.store_in_transaction(operation, &store, handle),
            None => self.store_committed(operation, &store),
        }
    }

    /// An operation outside a transaction: a write goes to storage at once.
    fn store_committed(
        &self,
        operation: &str,
        store: &StoreRequest<'_>,
    ) -> Result<Value, MachineError> {
        self.store_refuse_busy(operation, store.collection())?;
        let nothing = StoreOverlay::default();
        let before = StoreView::new(self.engine(store.frame())?, &nothing);
        let outcome = self.store_outcome(operation, store, &before)?;
        self.store_write(store, outcome.change())?;
        // The hot set is cleared only around the frame, so a read-back inside
        // it would still find the document as it was before the write.
        let after = StoreView::new(self.open_engine(store.frame())?, &nothing);
        outcome.answer(&after, store)
    }

    /// An operation inside the transaction `handle`: a write is held in the
    /// transaction until it commits. A write past a limit, to another
    /// namespace, or to a collection that cannot join the transaction ends it.
    fn store_in_transaction(
        &self,
        operation: &str,
        store: &StoreRequest<'_>,
        handle: &str,
    ) -> Result<Value, MachineError> {
        let mut transactions = self.store_transactions.borrow_mut();
        if StoreOperations::is_write(operation)
            && transactions.holds_other(store.collection(), handle)
        {
            return Err(StoreTransactions::busy(store.collection()));
        }
        let result = self.store_inside(operation, store, transactions.live(handle)?);
        if result.as_ref().is_err_and(StoreFailure::ends_transaction) {
            transactions.abandon(handle);
        }
        result
    }

    pub(crate) fn store_inside(
        &self,
        operation: &str,
        store: &StoreRequest<'_>,
        transaction: &mut StoreTransaction,
    ) -> Result<Value, MachineError> {
        let namespace = self.request_namespace(store.frame())?;
        StoreFailure::require_scope(
            transaction.namespace(),
            namespace.as_str(),
            store.collection(),
        )?;
        if StoreOperations::is_write(operation) {
            self.store_require_log(store)?;
        }
        let engine = self.engine(transaction.target())?;
        let empty = StoreOverlay::default();
        let before = StoreView::new(
            engine.clone(),
            transaction.overlay(store.collection(), &empty),
        );
        let outcome = self.store_outcome(operation, store, &before)?;
        Self::store_hold(transaction, store.collection(), outcome.change())?;
        let after = StoreView::new(engine, transaction.overlay(store.collection(), &empty));
        outcome.answer(&after, store)
    }

    /// Writes to a collection wait for its open transaction; one machine
    /// session runs one frame at a time, so a write that would wait is
    /// refused at once with the retryable `ETXNBUSY`.
    pub(crate) fn store_refuse_busy(
        &self,
        operation: &str,
        collection: &str,
    ) -> Result<(), MachineError> {
        let busy = StoreOperations::is_write(operation)
            && self.store_transactions.borrow_mut().holds(collection);
        if busy {
            return Err(StoreTransactions::busy(collection));
        }
        Ok(())
    }

    fn store_outcome(
        &self,
        operation: &str,
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        match operation {
            "storeFindById" => Self::store_find_by_id(store, view),
            "storeFindAll" => Self::store_find_all(store, view),
            "storeFind" => Self::store_find(store, view),
            "storeCount" => Self::store_count(store, view),
            "storeExists" => Self::store_exists(store, view),
            "storeScroll" => self.store_scroll(store, view),
            "storeSave" => self.store_save(store, view),
            "storePatch" => Self::store_patch(store, view),
            "storeDeleteById" => Self::store_delete_by_id(store, view),
            _ => Self::store_delete(store, view),
        }
    }

    fn store_find_by_id(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let id = store.id()?;
        let fields = view.record(store.collection(), id)?;
        Ok(StoreOutcome::value(StoreEntity::or_null(
            id,
            fields,
            store.id_field(),
        )))
    }

    fn store_find_all(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let query = StoreQuery::from_value(None, store.id_field())
            .map_err(|error| StoreFailure::query(&error))?;
        let records = view.find(store.collection(), &query, store.page()?)?;
        Ok(StoreOutcome::value(StoreEntity::list(
            &records,
            store.id_field(),
        )))
    }

    fn store_find(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let records = view.find(store.collection(), &store.query()?, store.page()?)?;
        Ok(StoreOutcome::value(StoreEntity::list(
            &records,
            store.id_field(),
        )))
    }

    fn store_count(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let count = view.count(store.collection(), &store.query()?)?;
        Ok(StoreOutcome::value(Value::from(count)))
    }

    fn store_exists(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let found = view.exists(store.collection(), &store.query()?)?;
        Ok(StoreOutcome::value(Value::Bool(found)))
    }

    /// A cursor is bound to the request's namespace and collection.
    fn store_scroll(
        &self,
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let namespace = self.request_namespace(store.frame())?;
        let page = store.scroll_page()?.within(namespace.as_str());
        let page = view.scroll(store.collection(), &store.query()?, &page)?;
        Ok(StoreOutcome::value(json!({
            "content": StoreEntity::list(&page.content, store.id_field()),
            "nextCursor": page.next_cursor,
        })))
    }

    /// `save`: create the document when the entity names no id or an id
    /// that does not exist, else replace it; with `requireExisting`, only
    /// replace (ADR 0035, Amendment 1).
    fn store_save(
        &self,
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let (id, fields) = StoreEntity::split(store.object("entity")?, store.id_field())?;
        let require_existing = store.flag("requireExisting")?;
        let id = match id {
            Some(id) => id,
            None => self.store_new_id(store, require_existing)?,
        };
        let change = view.plan_save(store.collection(), (&id, fields))?;
        StoreFailure::require_existing(store.collection(), &id, &change, require_existing)?;
        Ok(StoreOutcome::written(&id, change))
    }

    fn store_new_id(
        &self,
        store: &StoreRequest<'_>,
        require_existing: bool,
    ) -> Result<String, MachineError> {
        if require_existing {
            return Err(StoreFailure::missing_document(store.collection(), None));
        }
        self.writer(store.frame())?
            .allocate_identifier()
            .map_err(|error| storage_error(&error))
    }

    /// `patch`: a shallow merge of top-level fields into the document, or
    /// `null` when it does not exist.
    fn store_patch(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let id = store.id()?;
        let planned = view.plan_patch(store.collection(), (id, store.object("fields")?))?;
        Ok(planned.map_or_else(
            || StoreOutcome::value(Value::Null),
            |change| StoreOutcome::written(id, change),
        ))
    }

    fn store_delete_by_id(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let planned = view.plan_delete_by_id(store.collection(), store.id()?)?;
        Ok(planned.map_or_else(
            || StoreOutcome::value(Value::from(0)),
            |change| StoreOutcome::counted(Value::from(1), change),
        ))
    }

    fn store_delete(
        store: &StoreRequest<'_>,
        view: &StoreView<'_>,
    ) -> Result<StoreOutcome, MachineError> {
        let change = view.plan_delete(store.collection(), &store.query()?)?;
        Ok(StoreOutcome::counted(Value::from(change.deleted()), change))
    }

    fn store_write(
        &self,
        store: &StoreRequest<'_>,
        change: Option<&StoreChange>,
    ) -> Result<(), MachineError> {
        let Some(change) = change else {
            return Ok(());
        };
        self.write_engine(store.frame())?
            .store_apply(store.collection(), change)
            .map_err(|error| StoreFailure::engine(&error))
    }

    fn store_hold(
        transaction: &mut StoreTransaction,
        collection: &str,
        change: Option<&StoreChange>,
    ) -> Result<(), MachineError> {
        change.map_or(Ok(()), |change| transaction.hold(collection, change))
    }
}
