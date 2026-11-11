use fylo_engine::{EngineErrorCode, WriteEngine};
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::NativeStorageErrorCode;
use serde_json::{Map, Value};

use crate::{Error, Page, Root};

/// A stored document: its JSON fields, in their stored order.
pub type Document = Map<String, Value>;

/// A document collection in an open [`Root`].
#[derive(Clone, Copy, Debug)]
pub struct Collection<'root> {
    root: &'root Root,
    name: &'root str,
}

/// A structured FYLO query (`api/query/v1`), checked once and reusable.
#[derive(Clone, Debug)]
pub struct Query(StructuredQuery);

impl Query {
    /// Parse a query in its JSON form, such as
    /// `{"$ops": [{"name": {"$eq": "Ada"}}], "$limit": 10}`.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for a malformed query or one beyond the default
    /// limits.
    pub fn from_json(query: &Value) -> Result<Self, Error> {
        StructuredQuery::from_value(query, QueryLimits::default())
            .map(Self)
            .map_err(|error| Error::new("EQUERY_INVALID", error.to_string()))
    }
}

impl<'root> Collection<'root> {
    pub(crate) fn new(root: &'root Root, name: &'root str) -> Self {
        Self { root, name }
    }

    /// Store a new document under a FYLO-assigned TTID, and return the TTID.
    ///
    /// # Errors
    ///
    /// Fails for a missing collection, a document FYLO refuses (an array of
    /// objects, or a schema violation), or a failed durable write.
    pub fn put(&self, document: Document) -> Result<String, Error> {
        let writer = self.root.writer()?;
        let identifier = writer.allocate_identifier()?;
        WriteEngine::from_writer(writer).put_document(self.name, &identifier, document)?;
        Ok(identifier)
    }

    /// Merge `changes` into a stored document, field by field.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` for a missing document, a merged document FYLO
    /// refuses, or a failed durable write.
    pub fn patch(&self, id: &str, changes: &Map<String, Value>) -> Result<(), Error> {
        self.root
            .write_engine()?
            .patch_document_fields(self.name, id, changes)
            .map_err(Error::from)
    }

    /// Read one document, or `None` when it does not exist.
    ///
    /// # Errors
    ///
    /// Fails for a missing collection, an invalid identifier, or a read
    /// failure.
    pub fn get(&self, id: &str) -> Result<Option<Document>, Error> {
        match self.root.reader()?.get(self.name, id) {
            Ok(record) => Ok(Some(record.document.into_fields())),
            Err(error) if error.storage_code() == Some(NativeStorageErrorCode::NotFound) => {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Read one document as `getLatest` does: a document whose storage read
    /// fails is reported as absent, as a missing one is.
    ///
    /// # Errors
    ///
    /// Fails for a missing collection, an invalid identifier, or another read
    /// failure.
    pub fn latest(&self, id: &str) -> Result<Option<Document>, Error> {
        match self.root.reader()?.get(self.name, id) {
            Ok(record) => Ok(Some(record.document.into_fields())),
            Err(error)
                if error.storage_code() == Some(NativeStorageErrorCode::NotFound)
                    || (error.storage_code().is_none()
                        && error.code() == EngineErrorCode::Storage) =>
            {
                Ok(None)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Delete one document, keeping its tombstone.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` for a missing document, or a failed durable write.
    pub fn delete(&self, id: &str) -> Result<(), Error> {
        self.root
            .writer()?
            .delete_document(self.name, id)
            .map_err(Error::from)
    }

    /// Every document matching `query`, as `(id, document)` pairs in TTID
    /// order, or in `$sort` order when it has one.
    ///
    /// # Errors
    ///
    /// Fails for a missing collection or a read failure.
    pub fn find(&self, query: &Query) -> Result<Vec<(String, Document)>, Error> {
        self.repair_word_fields(query)?;
        let rows = self.root.reader()?.find(self.name, &query.0)?;
        Ok(rows
            .into_iter()
            .map(|record| (record.metadata.id, record.document.into_fields()))
            .collect())
    }

    /// One page of a query with `$sort` (and usually `$limit`), in order,
    /// with the cursor for `$after` on the next page (ADR 0023).
    ///
    /// # Errors
    ///
    /// `EENGINE_QUERY` for a query without `$sort`, or a read failure.
    pub fn find_page(&self, query: &Query) -> Result<Page, Error> {
        self.repair_word_fields(query)?;
        let page = self.root.reader()?.find_sorted(self.name, &query.0)?;
        Ok(Page {
            items: page
                .rows
                .into_iter()
                .map(|record| (record.metadata.id, record.document.into_fields()))
                .collect(),
            next_cursor: page.next_cursor,
        })
    }

    /// Replace the collection's word fields, the paths whose words `$match`
    /// finds from the index, and rebuild its index (ADR 0023). An empty list
    /// clears them. Returns the stored, slash-separated paths.
    ///
    /// # Errors
    ///
    /// `EQUERY_INVALID` for more than 16 fields, an empty, repeated or
    /// encrypted field, or a file collection; or the rebuild's failure.
    pub fn set_word_fields(&self, fields: &[String]) -> Result<Vec<String>, Error> {
        let encrypted = self.root.reader()?.encrypted_fields(self.name)?;
        self.root
            .writer()?
            .set_word_fields(self.name, fields, &encrypted)
            .map_err(Error::from)
    }

    fn repair_word_fields(&self, query: &Query) -> Result<(), Error> {
        let fields = query.0.match_fields();
        if fields.is_empty() {
            return Ok(());
        }
        self.root.writer()?.repair_word_fields(self.name, &fields)?;
        Ok(())
    }
}
