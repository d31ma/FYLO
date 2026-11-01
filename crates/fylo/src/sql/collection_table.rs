use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use fylo_engine::ReadDocument;
use fylo_query::{QueryLimits, StructuredQuery};
use fylo_storage_native::{DocumentWrite, NativeStorageErrorCode};
use parking_lot::RwLock;
use serde_json::{Map, Value as Json};
use turso_core::{
    Connection, InternalVirtualTable, InternalVirtualTableCursor, LimboError, Numeric, Value,
};
use turso_ext::{ConstraintInfo, IndexInfo, OrderByInfo, ResultCode};

use super::scan_plan::{Lookup, ScanPlan};
use super::{Shared, value};
use crate::Error;

/// One FYLO collection as a writable SQL table (ADR 0019 section 3).
///
/// Columns: `id` (the TTID), `doc` (the document as JSON text), one column
/// per head-schema field, then hidden `meta`, `created_at` and `updated_at`.
///
/// Writes are buffered from `begin` to `commit`, which Turso calls around
/// every autocommit statement and every `BEGIN … COMMIT`, and the buffer
/// commits as one ADR 0013 log record. Scans read the transaction's own
/// buffered writes. Only one collection may hold buffered writes in a
/// transaction; a second fails with `EFYLO_TXN_SCOPE`.
#[derive(Debug)]
pub(crate) struct CollectionTable {
    shared: Arc<Shared>,
    name: String,
    fields: Vec<String>,
    state: Arc<Mutex<TableState>>,
}

#[derive(Debug, Default)]
struct TableState {
    rowid_of: HashMap<String, i64>,
    id_of: HashMap<i64, String>,
    spare_rowid: i64,
    pending: Option<Pending>,
}

/// The writes of the open transaction, at most one per document.
#[derive(Debug, Default)]
struct Pending {
    order: Vec<String>,
    changes: HashMap<String, Change>,
}

#[derive(Clone, Debug)]
enum Change {
    Put(Map<String, Json>),
    Replace(Map<String, Json>),
    Delete,
}

struct Row {
    rowid: i64,
    id: String,
    document: Map<String, Json>,
    created_at: Option<u64>,
    updated_at: Option<u64>,
}

pub(crate) struct CollectionCursor {
    table: Arc<TableCore>,
    rows: Vec<Row>,
    position: usize,
}

/// What a cursor needs of its table, shared without the table's lock.
struct TableCore {
    shared: Arc<Shared>,
    name: String,
    fields: Vec<String>,
    state: Arc<Mutex<TableState>>,
}

impl CollectionTable {
    const ID: usize = 0;
    const DOC: usize = 1;
    const FIELDS: usize = 2;
    const HIDDEN: usize = 3;
    const RESERVED: [&'static str; 5] = ["id", "doc", "meta", "created_at", "updated_at"];

    pub(crate) fn new(shared: Arc<Shared>, name: &str, schema_fields: Vec<String>) -> Self {
        let fields = schema_fields
            .into_iter()
            .filter(|field| !Self::RESERVED.contains(&field.as_str()))
            .collect();
        Self {
            shared,
            name: name.to_owned(),
            fields,
            state: Arc::new(Mutex::new(TableState::default())),
        }
    }

    fn core(&self) -> Arc<TableCore> {
        Arc::new(TableCore {
            shared: Arc::clone(&self.shared),
            name: self.name.clone(),
            fields: self.fields.clone(),
            state: Arc::clone(&self.state),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, TableState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn columns(&self) -> usize {
        Self::FIELDS + self.fields.len() + Self::HIDDEN
    }

    fn insert(&self, columns: &[Value]) -> Result<i64, Error> {
        let mut document = value::document_of(&columns[Self::DOC])
            .map_err(|message| Error::new("EFYLO_SQL_VALUE", message))?;
        for (field, column) in self.fields.iter().zip(&columns[Self::FIELDS..]) {
            if !matches!(column, Value::Null) {
                document.insert(field.clone(), field_value(column)?);
            }
        }
        let identifier = match &columns[Self::ID] {
            Value::Null => self.shared.root.writer()?.allocate_identifier()?,
            Value::Text(text) => text.as_str().to_owned(),
            _ => {
                return Err(Error::new(
                    "EFYLO_SQL_VALUE",
                    "id must be a TTID text or NULL",
                ));
            }
        };
        let mut state = self.state();
        let rowid = state.rowid(&identifier);
        state.record(&self.shared, &self.name, identifier, Change::Put(document))?;
        Ok(rowid)
    }

    fn update(&self, rowids: (&Value, &Value), columns: &[Value]) -> Result<(), Error> {
        let identifier = self.identifier_of(rowids.0)?;
        if rowids.0 != rowids.1
            || !matches!(&columns[Self::ID], Value::Text(text) if text.as_str() == identifier)
        {
            return Err(Error::new(
                "EFYLO_SQL_ID",
                "the id and rowid of a FYLO row cannot change",
            ));
        }
        let mut document = value::document_of(&columns[Self::DOC])
            .map_err(|message| Error::new("EFYLO_SQL_VALUE", message))?;
        let before = self.current(&identifier)?.unwrap_or_default();
        for (field, column) in self.fields.iter().zip(&columns[Self::FIELDS..]) {
            let stored = before.get(field).map_or(Json::Null, |stored| {
                value::to_json(&value::from_json(stored)).unwrap_or(Json::Null)
            });
            let assigned = field_value(column)?;
            if assigned != stored {
                document.insert(field.clone(), assigned);
            }
        }
        let mut state = self.state();
        state.record(
            &self.shared,
            &self.name,
            identifier,
            Change::Replace(document),
        )
    }

    fn delete(&self, rowid: &Value) -> Result<(), Error> {
        let identifier = self.identifier_of(rowid)?;
        let mut state = self.state();
        state.record(&self.shared, &self.name, identifier, Change::Delete)
    }

    fn identifier_of(&self, rowid: &Value) -> Result<String, Error> {
        let Value::Numeric(Numeric::Integer(rowid)) = rowid else {
            return Err(Error::new(
                "EFYLO_SQL_ID",
                "a FYLO row has an integer rowid",
            ));
        };
        self.state()
            .id_of
            .get(rowid)
            .cloned()
            .ok_or_else(|| Error::new("EFYLO_SQL_ID", "no FYLO row has this rowid"))
    }

    /// The document as the open transaction sees it.
    fn current(&self, identifier: &str) -> Result<Option<Map<String, Json>>, Error> {
        if let Some(change) = self
            .state()
            .pending
            .as_ref()
            .and_then(|pending| pending.changes.get(identifier).cloned())
        {
            return Ok(match change {
                Change::Put(document) | Change::Replace(document) => Some(document),
                Change::Delete => None,
            });
        }
        self.shared.root.collection(&self.name).get(identifier)
    }

    fn apply(&self, arguments: &[Value]) -> Result<Option<i64>, Error> {
        let direct = self.state().pending.is_none();
        if direct {
            self.start()?;
        }
        let outcome = match arguments {
            [rowid] | [rowid, Value::Null] => self.delete(rowid).map(|()| None),
            [Value::Null, _, columns @ ..] if columns.len() == self.columns() => {
                self.insert(columns).map(Some)
            }
            [old, new, columns @ ..] if columns.len() == self.columns() => {
                self.update((old, new), columns).map(|()| None)
            }
            _ => Err(Error::new(
                "EFYLO_SQL_VALUE",
                "unexpected write layout for a FYLO table",
            )),
        };
        if direct {
            return match outcome {
                Ok(rowid) => self.finish().map(|()| rowid),
                Err(error) => {
                    self.discard();
                    Err(error)
                }
            };
        }
        outcome
    }

    fn start(&self) -> Result<(), Error> {
        self.shared.enter(&self.name)?;
        self.state().pending = Some(Pending::default());
        Ok(())
    }

    fn finish(&self) -> Result<(), Error> {
        let pending = self.state().pending.take();
        self.shared.leave(&self.name);
        let Some(pending) = pending else {
            return Ok(());
        };
        let writes = pending.into_writes();
        if writes.is_empty() {
            return Ok(());
        }
        self.shared
            .root
            .write_engine()?
            .commit_document_writes(&self.name, &writes)
            .map_err(Error::from)
    }

    fn discard(&self) {
        self.state().pending = None;
        self.shared.leave(&self.name);
    }
}

impl TableState {
    /// A stable rowid for `identifier` on this connection: its TTID creation
    /// segment read as a base-36 number when that is free, otherwise the
    /// next negative spare.
    fn rowid(&mut self, identifier: &str) -> i64 {
        if let Some(rowid) = self.rowid_of.get(identifier) {
            return *rowid;
        }
        let creation = identifier.split('-').next().unwrap_or_default();
        let rowid = i64::from_str_radix(creation, 36)
            .ok()
            .filter(|rowid| *rowid > 0 && !self.id_of.contains_key(rowid))
            .unwrap_or_else(|| {
                self.spare_rowid -= 1;
                self.spare_rowid
            });
        self.rowid_of.insert(identifier.to_owned(), rowid);
        self.id_of.insert(rowid, identifier.to_owned());
        rowid
    }

    fn record(
        &mut self,
        shared: &Shared,
        name: &str,
        identifier: String,
        change: Change,
    ) -> Result<(), Error> {
        shared.enter(name)?;
        let pending = self.pending.get_or_insert_with(Pending::default);
        let merged = match (pending.changes.remove(&identifier), change) {
            (None | Some(Change::Replace(_)), change) => Some(change),
            (Some(Change::Put(_)), Change::Put(_)) => {
                return Err(Error::new(
                    "ENATIVE_CORRUPT_METADATA",
                    "document identifier already exists",
                ));
            }
            (Some(Change::Put(_)), Change::Replace(document)) => Some(Change::Put(document)),
            (Some(Change::Delete), Change::Put(document)) => Some(Change::Replace(document)),
            (Some(Change::Put(_)), Change::Delete) => None,
            (Some(Change::Delete), _) => {
                return Err(Error::new(
                    "ENATIVE_NOT_FOUND",
                    "the document was deleted in this transaction",
                ));
            }
        };
        match merged {
            Some(change) => {
                if !pending.order.contains(&identifier) {
                    pending.order.push(identifier.clone());
                }
                pending.changes.insert(identifier, change);
            }
            None => pending.order.retain(|known| *known != identifier),
        }
        Ok(())
    }
}

impl Pending {
    fn into_writes(self) -> Vec<DocumentWrite> {
        let mut changes = self.changes;
        self.order
            .into_iter()
            .filter_map(|identifier| {
                let change = changes.remove(&identifier)?;
                Some(match change {
                    Change::Put(fields) => DocumentWrite::Put { identifier, fields },
                    Change::Replace(fields) => DocumentWrite::Replace { identifier, fields },
                    Change::Delete => DocumentWrite::Delete { identifier },
                })
            })
            .collect()
    }
}

fn field_value(column: &Value) -> Result<Json, Error> {
    value::to_json(column).ok_or_else(|| {
        Error::new(
            "EFYLO_SQL_VALUE",
            "a blob cannot be stored in a FYLO document field",
        )
    })
}

fn sql_error(error: &Error) -> LimboError {
    LimboError::ExtensionError(error.to_string())
}

impl InternalVirtualTable for CollectionTable {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn open(
        &self,
        _conn: Arc<Connection>,
    ) -> turso_core::Result<Arc<RwLock<dyn InternalVirtualTableCursor>>> {
        Ok(Arc::new(RwLock::new(CollectionCursor {
            table: self.core(),
            rows: Vec::new(),
            position: 0,
        })))
    }

    fn best_index(
        &self,
        constraints: &[ConstraintInfo],
        _order_by: &[OrderByInfo],
    ) -> Result<IndexInfo, ResultCode> {
        Ok(ScanPlan::choose(
            constraints,
            self.fields.len(),
            self.shared.pushdown,
        ))
    }

    fn sql(&self) -> String {
        let columns: Vec<String> = ["id TEXT".to_owned(), "doc TEXT".to_owned()]
            .into_iter()
            .chain(self.fields.iter().map(|field| quoted(field)))
            .chain(["meta HIDDEN", "created_at HIDDEN", "updated_at HIDDEN"].map(String::from))
            .collect();
        format!(
            "CREATE TABLE {} ({})",
            quoted(&self.name),
            columns.join(", ")
        )
    }

    fn readonly(&self) -> bool {
        false
    }

    fn update(&self, arguments: &[Value]) -> turso_core::Result<Option<i64>> {
        let rowid = self.apply(arguments).map_err(|error| sql_error(&error))?;
        self.shared
            .rows_written
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(rowid)
    }

    fn begin(&self) -> turso_core::Result<()> {
        if self.state().pending.is_some() {
            return Ok(());
        }
        self.start().map_err(|error| sql_error(&error))
    }

    fn commit(&self) -> turso_core::Result<()> {
        self.finish().map_err(|error| sql_error(&error))
    }

    fn rollback(&self) -> turso_core::Result<()> {
        self.discard();
        Ok(())
    }
}

pub(crate) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl TableCore {
    fn read(&self, lookup: Lookup) -> Result<Vec<ReadDocument>, Error> {
        let reader = self.shared.root.reader()?;
        match lookup {
            Lookup::Nothing => Ok(Vec::new()),
            Lookup::Id(identifier) => match reader.get(&self.name, &identifier) {
                Ok(document) => Ok(vec![document]),
                Err(error)
                    if error.storage_code() == Some(NativeStorageErrorCode::NotFound)
                        || error.storage_code()
                            == Some(NativeStorageErrorCode::InvalidDocumentId) =>
                {
                    Ok(Vec::new())
                }
                Err(error) => Err(error.into()),
            },
            Lookup::All => Ok(reader.find(&self.name, &Self::query(&Json::Object(Map::new()))?)?),
            Lookup::Query(query) => Ok(reader.find(&self.name, &Self::query(&query)?)?),
        }
    }

    fn query(query: &Json) -> Result<StructuredQuery, Error> {
        StructuredQuery::from_value(query, QueryLimits::default())
            .map_err(|error| Error::new("EQUERY_INVALID", error.to_string()))
    }

    /// The rows a scan returns: FYLO's documents with the open transaction's
    /// writes laid over them, then the documents it put.
    fn rows(&self, lookup: Lookup) -> Result<Vec<Row>, Error> {
        let only = match &lookup {
            Lookup::Id(identifier) => Some(identifier.clone()),
            _ => None,
        };
        let stored = self.read(lookup)?;
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let changes = state
            .pending
            .as_ref()
            .map(|pending| (pending.order.clone(), pending.changes.clone()))
            .unwrap_or_default();
        let mut rows = Vec::with_capacity(stored.len());
        for document in stored {
            let id = document.metadata.id.clone();
            let fields = match changes.1.get(&id) {
                Some(Change::Delete) => continue,
                Some(Change::Put(fields) | Change::Replace(fields)) => fields.clone(),
                None => document.document.into_fields(),
            };
            rows.push(Row {
                rowid: state.rowid(&id),
                id,
                document: fields,
                created_at: Some(document.metadata.created_at),
                updated_at: Some(document.metadata.updated_at),
            });
        }
        for id in changes.0 {
            if only.as_ref().is_some_and(|only| *only != id) {
                continue;
            }
            if let Some(Change::Put(fields)) = changes.1.get(&id)
                && !rows.iter().any(|row| row.id == id)
            {
                rows.push(Row {
                    rowid: state.rowid(&id),
                    id,
                    document: fields.clone(),
                    created_at: None,
                    updated_at: None,
                });
            }
        }
        Ok(rows)
    }
}

impl CollectionCursor {
    fn row(&self) -> Option<&Row> {
        self.rows.get(self.position)
    }
}

impl InternalVirtualTableCursor for CollectionCursor {
    fn next(&mut self) -> Result<bool, LimboError> {
        self.position += 1;
        Ok(self.position < self.rows.len())
    }

    fn rowid(&self) -> i64 {
        self.row().map_or(0, |row| row.rowid)
    }

    fn column(&self, column: usize) -> Result<Value, LimboError> {
        let Some(row) = self.row() else {
            return Ok(Value::Null);
        };
        let fields = self.table.fields.len();
        let integer = |number: Option<u64>| {
            number
                .and_then(|number| i64::try_from(number).ok())
                .map_or(Value::Null, |number| {
                    Value::Numeric(Numeric::Integer(number))
                })
        };
        Ok(match column {
            CollectionTable::ID => Value::from_text(row.id.clone()),
            CollectionTable::DOC => {
                Value::from_text(Json::Object(row.document.clone()).to_string())
            }
            column if column < CollectionTable::FIELDS + fields => row
                .document
                .get(&self.table.fields[column - CollectionTable::FIELDS])
                .map_or(Value::Null, value::from_json),
            column if column == CollectionTable::FIELDS + fields => Value::from_text(
                serde_json::json!({
                    "id": row.id,
                    "createdAt": row.created_at,
                    "updatedAt": row.updated_at,
                })
                .to_string(),
            ),
            column if column == CollectionTable::FIELDS + fields + 1 => integer(row.created_at),
            column if column == CollectionTable::FIELDS + fields + 2 => integer(row.updated_at),
            _ => Value::Null,
        })
    }

    fn filter(
        &mut self,
        arguments: &[Value],
        idx_str: Option<String>,
        _idx_num: i32,
    ) -> Result<bool, LimboError> {
        let lookup = ScanPlan::decode(idx_str.as_deref()).lookup(arguments, &self.table.fields);
        self.rows = self.table.rows(lookup).map_err(|error| sql_error(&error))?;
        self.position = 0;
        Ok(!self.rows.is_empty())
    }
}
