use std::cell::RefCell;
use std::marker::PhantomData;

use byckr_engine::{
    EngineError, ReadDocument, ReadOnlyEngine, StoreChange, StoreOverlay, StorePending,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::entity_codec::EntityCodec;
use super::query::Query;
use super::scroll::Scroll;
use super::transaction::Transaction;
use crate::{Document, Error, Root};
use byckr_query::{StorePage, StoreScrollPage};

/// One collection as a TACHYON-shaped Store (`api/store/v1`, ADR 0035):
/// the Repository base (`find_by_id`, `find_all`, `save`, `delete_by_id`),
/// `patch`, and `find`, `count`, `exists`, `delete` and `scroll` by query.
///
/// `T` is the entity type: any serde type whose id field (default `id`) is
/// an optional string, or [`Document`] for plain JSON. Each call opens the
/// engine it needs under the root's lease; a write is durable before the
/// call returns. Inside [`Store::begin`] writes are held until commit.
///
/// ```no_run
/// use byckr::store::{Page, Query};
/// use serde_json::json;
///
/// let root = byckr::Database::open("/srv/catalog", byckr::OpenOptions::default())?;
/// root.create_collection("tickets")?;
/// let tickets = root.store::<byckr::Document>("tickets");
/// let saved = tickets.save(json!({ "status": "open" }).as_object().unwrap())?;
/// let open = tickets.count(&Query::new().eq("status", "open"))?;
/// assert_eq!(open, 1);
/// # let _ = (saved, Page::new(20, 0));
/// # Ok::<(), byckr::Error>(())
/// ```
pub struct Store<'root, T = Document> {
    root: &'root Root,
    collection: String,
    id_field: String,
    pending: Option<RefCell<StorePending>>,
    entity: PhantomData<fn() -> T>,
}

impl<'root, T: Serialize + DeserializeOwned> Store<'root, T> {
    const DEFAULT_ID_FIELD: &'static str = "id";
    const NOT_FOUND: &'static str = "ENATIVE_NOT_FOUND";

    pub(crate) fn new(root: &'root Root, collection: &str) -> Self {
        Self {
            root,
            collection: collection.to_owned(),
            id_field: Self::DEFAULT_ID_FIELD.to_owned(),
            pending: None,
            entity: PhantomData,
        }
    }

    /// The same Store with the id under `field` instead of `id`.
    #[must_use]
    pub fn with_id_field(mut self, field: &str) -> Self {
        field.clone_into(&mut self.id_field);
        self
    }

    /// The collection's name.
    #[must_use]
    pub fn collection(&self) -> &str {
        &self.collection
    }

    /// The entity `id`, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` for a missing collection; `EBADREQUEST` when the
    /// document does not fit `T`.
    pub fn find_by_id(&self, id: &str) -> Result<Option<T>, Error> {
        let fields =
            self.read(|engine, overlay| engine.store_record(&self.collection, id, overlay))?;
        fields
            .map(|fields| EntityCodec::entity(id, fields, &self.id_field))
            .transpose()
    }

    /// One page of every entity, ordered by id.
    ///
    /// # Errors
    ///
    /// As [`Self::find`].
    pub fn find_all(&self, page: StorePage) -> Result<Vec<T>, Error> {
        self.find(&Query::new(), page)
    }

    /// One page of the entities `query` matches, in its order.
    ///
    /// # Errors
    ///
    /// A query code (`EQUERY_INVALID`, `EQUERYLIMIT`) for a bad query,
    /// `ENATIVE_NOT_FOUND` for a missing collection.
    pub fn find(&self, query: &Query, page: StorePage) -> Result<Vec<T>, Error> {
        let query = query.checked(&self.id_field)?;
        let records = self
            .read(|engine, overlay| engine.store_find(&self.collection, &query, page, overlay))?;
        self.entities(&records)
    }

    /// How many entities `query` matches.
    ///
    /// # Errors
    ///
    /// As [`Self::find`].
    pub fn count(&self, query: &Query) -> Result<u64, Error> {
        let query = query.checked(&self.id_field)?;
        self.read(|engine, overlay| engine.store_count(&self.collection, &query, overlay))
    }

    /// Whether `query` matches any entity; `false` for a missing collection.
    ///
    /// # Errors
    ///
    /// A query code for a bad query.
    pub fn exists(&self, query: &Query) -> Result<bool, Error> {
        let query = query.checked(&self.id_field)?;
        self.read(|engine, overlay| engine.store_exists(&self.collection, &query, overlay))
    }

    /// One cursor page of the entities `query` matches.
    ///
    /// # Errors
    ///
    /// As [`Self::find`], plus `ECURSOR` for a cursor of another query.
    pub fn scroll(&self, query: &Query, page: &StoreScrollPage) -> Result<Scroll<T>, Error> {
        let query = query.checked(&self.id_field)?;
        let scroll = self
            .read(|engine, overlay| engine.store_scroll(&self.collection, &query, page, overlay))?;
        Ok(Scroll {
            content: self.entities(&scroll.content)?,
            next_cursor: scroll.next_cursor,
        })
    }

    /// Create the entity when it names no id or an id that does not exist,
    /// else replace it, and answer it as stored.
    ///
    /// # Errors
    ///
    /// `EBADREQUEST` for an entity that is not an object or has a non-string
    /// id; any error of the write.
    pub fn save(&self, entity: &T) -> Result<T, Error> {
        self.save_with(entity, false)
    }

    /// Replace the entity only: an id that is missing or does not exist
    /// fails with `ENATIVE_NOT_FOUND` and writes nothing (ADR 0035,
    /// Amendment 1).
    ///
    /// # Errors
    ///
    /// As [`Self::save`], plus `ENATIVE_NOT_FOUND`.
    pub fn save_existing(&self, entity: &T) -> Result<T, Error> {
        self.save_with(entity, true)
    }

    /// Merge `fields` into the entity `id`, or answer `None` when it does
    /// not exist.
    ///
    /// # Errors
    ///
    /// Any error of the write.
    pub fn patch(&self, id: &str, fields: &Document) -> Result<Option<T>, Error> {
        let planned = self.read(|engine, overlay| {
            engine.store_plan_patch(&self.collection, (id, fields), overlay)
        })?;
        let Some(change) = planned else {
            return Ok(None);
        };
        self.write(&change)?;
        self.find_by_id(id)
    }

    /// Delete the entity `id`; answers how many were deleted, 0 or 1.
    ///
    /// # Errors
    ///
    /// Any error of the write.
    pub fn delete_by_id(&self, id: &str) -> Result<u64, Error> {
        let planned = self.read(|engine, overlay| {
            engine.store_plan_delete_by_id(&self.collection, id, overlay)
        })?;
        let Some(change) = planned else {
            return Ok(0);
        };
        self.write(&change)?;
        Ok(1)
    }

    /// Delete every entity `query` matches, in one log record; answers how
    /// many were deleted.
    ///
    /// # Errors
    ///
    /// As [`Self::find`], and any error of the write.
    pub fn delete(&self, query: &Query) -> Result<u64, Error> {
        let query = query.checked(&self.id_field)?;
        let change = self
            .read(|engine, overlay| engine.store_plan_delete(&self.collection, &query, overlay))?;
        self.write(&change)?;
        Ok(u64::try_from(change.deleted()).unwrap_or(u64::MAX))
    }

    /// Open a one-collection transaction: its writes are held until
    /// [`Transaction::commit`], and its reads see them. Dropping it without
    /// a commit rolls it back.
    #[must_use]
    pub fn begin(&self) -> Transaction<'root, T> {
        Transaction::new(Self {
            root: self.root,
            collection: self.collection.clone(),
            id_field: self.id_field.clone(),
            pending: Some(RefCell::default()),
            entity: PhantomData,
        })
    }

    pub(crate) const fn root(&self) -> &'root Root {
        self.root
    }

    /// The writes this Store holds, or none outside a transaction.
    pub(crate) fn held(&self) -> StorePending {
        self.pending
            .as_ref()
            .map(|pending| pending.borrow().clone())
            .unwrap_or_default()
    }

    fn save_with(&self, entity: &T, require_existing: bool) -> Result<T, Error> {
        let (id, fields) = EntityCodec::fields(entity, &self.id_field)?;
        let id = self.save_target(id, require_existing)?;
        let change = self.read(|engine, overlay| {
            engine.store_plan_save(&self.collection, (&id, fields), overlay)
        })?;
        self.require_existing(&id, &change, require_existing)?;
        self.write(&change)?;
        self.find_by_id(&id)?
            .ok_or_else(|| Error::new(Self::NOT_FOUND, format!("document {id} was not saved")))
    }

    fn save_target(&self, id: Option<String>, require_existing: bool) -> Result<String, Error> {
        match (id, require_existing) {
            (Some(id), _) => Ok(id),
            (None, true) => Err(self.missing("no id")),
            (None, false) => Ok(self.root.writer()?.allocate_identifier()?),
        }
    }

    fn require_existing(
        &self,
        id: &str,
        change: &StoreChange,
        require_existing: bool,
    ) -> Result<(), Error> {
        let creates = matches!(change, StoreChange::Put { existed: false, .. });
        if require_existing && creates {
            return Err(self.missing(&format!("document {id}")));
        }
        Ok(())
    }

    fn missing(&self, named: &str) -> Error {
        Error::new(
            Self::NOT_FOUND,
            format!(
                "collection {}: {named} does not exist, and requireExisting is true",
                self.collection
            ),
        )
    }

    fn entities(&self, records: &[ReadDocument]) -> Result<Vec<T>, Error> {
        records
            .iter()
            .map(|record| {
                EntityCodec::entity(
                    &record.metadata.id,
                    record.document.fields().clone(),
                    &self.id_field,
                )
            })
            .collect()
    }

    /// Read through the engine with this Store's transaction laid over the
    /// committed documents, or with nothing over them outside one.
    fn read<R>(
        &self,
        read: impl FnOnce(&ReadOnlyEngine, &StoreOverlay) -> Result<R, EngineError>,
    ) -> Result<R, Error> {
        let engine = self.root.reader()?;
        let outcome = match &self.pending {
            Some(pending) => read(&engine, pending.borrow().overlay()),
            None => read(&engine, &StoreOverlay::default()),
        };
        outcome.map_err(Error::from)
    }

    /// Hold the change in this Store's transaction, or write it at once
    /// outside one.
    fn write(&self, change: &StoreChange) -> Result<(), Error> {
        let outcome = match &self.pending {
            Some(pending) => pending.borrow_mut().hold(change),
            None => self
                .root
                .write_engine()?
                .store_apply(&self.collection, change),
        };
        outcome.map_err(Error::from)
    }
}
