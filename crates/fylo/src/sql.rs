//! SQL over a FYLO root, through Turso's engine (`turso_core`, pinned at
//! 0.8.3 and patched with its `fylo` feature; ADR 0019 section 3).
//!
//! [`Root::sql`] opens an in-memory SQL catalog, registers one writable table
//! per document collection plus the queue and info tables, and returns a
//! connection that binds parameters. No SQL engine file is ever written:
//! every row is a FYLO document, read and written through FYLO.

mod collection_table;
mod command_tables;
mod io;
mod scan_plan;
mod value;

use std::num::NonZero;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use parking_lot::RwLock;
use turso_core::{Database, LimboError, SqliteDialect, Statement, VirtualTable};
use turso_ext::VTabKind;

use crate::{Error, Root};
use collection_table::CollectionTable;
use command_tables::{Command, CommandTable};
pub use value::SqlValue;

/// The Turso engine version this build pins.
pub const TURSO_PIN: &str = "0.8.3";

/// How [`Root::sql_with`] opens a SQL connection.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct SqlOptions {
    /// Push `=` and range constraints down to FYLO's indexes (the default).
    /// Off, every scan reads the whole collection; results are the same.
    pub pushdown: bool,
}

impl Default for SqlOptions {
    fn default() -> Self {
        Self { pushdown: true }
    }
}

impl SqlOptions {
    /// Turn index pushdown on or off.
    #[must_use]
    pub const fn with_pushdown(mut self, pushdown: bool) -> Self {
        self.pushdown = pushdown;
        self
    }
}

/// The rows a query returned.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SqlRows {
    /// Column names, in order.
    pub columns: Vec<String>,
    /// Each row's values, in column order.
    pub rows: Vec<Vec<SqlValue>>,
}

/// A SQL connection on an open [`Root`].
///
/// Each statement runs synchronously: a write is committed in FYLO's
/// documents, or has failed with FYLO's error, when the call returns.
/// Collections created after the connection opened appear in the next one.
pub struct Sql {
    connection: Arc<turso_core::Connection>,
    database: Arc<Database>,
    tables: Vec<String>,
    shared: Arc<Shared>,
}

/// What every table of one connection shares: the root, the pushdown
/// switch, and the one collection the open transaction writes.
#[derive(Debug)]
pub(crate) struct Shared {
    pub(crate) root: Arc<Root>,
    pub(crate) pushdown: bool,
    scope: Mutex<Option<String>>,
    /// Rows the tables accepted for writing, for each statement's change
    /// count: Turso's own count leaves virtual tables out.
    pub(crate) rows_written: AtomicU64,
}

impl Shared {
    /// Let `collection` buffer writes in the open transaction, unless another
    /// collection already does.
    pub(crate) fn enter(&self, collection: &str) -> Result<(), Error> {
        let mut scope = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
        match scope.as_deref() {
            Some(holder) if holder != collection => Err(Error::new(
                "EFYLO_TXN_SCOPE",
                format!(
                    "a transaction writes one collection; it already writes \"{holder}\", \
                     so \"{collection}\" needs its own transaction"
                ),
            )),
            _ => {
                *scope = Some(collection.to_owned());
                Ok(())
            }
        }
    }

    pub(crate) fn leave(&self, collection: &str) {
        let mut scope = self.scope.lock().unwrap_or_else(PoisonError::into_inner);
        if scope.as_deref() == Some(collection) {
            *scope = None;
        }
    }
}

impl Root {
    /// Open a SQL connection on this root, with index pushdown on.
    ///
    /// # Errors
    ///
    /// As [`Root::sql_with`].
    pub fn sql(self: &Arc<Self>) -> Result<Sql, Error> {
        self.sql_with(SqlOptions::default())
    }

    /// Open a SQL connection on this root. Every document collection becomes
    /// a table named after it (quote names with hyphens: `"hd-events"`).
    ///
    /// # Errors
    ///
    /// Fails when the root's collections cannot be listed or a collection's
    /// schema registry is corrupt.
    pub fn sql_with(self: &Arc<Self>, options: SqlOptions) -> Result<Sql, Error> {
        let shared = Arc::new(Shared {
            root: Arc::clone(self),
            pushdown: options.pushdown,
            scope: Mutex::new(None),
            rows_written: AtomicU64::new(0),
        });
        let io: Arc<dyn turso_core::IO> = Arc::new(io::FyloIo::new());
        let database = Database::open(
            io,
            ":memory:",
            turso_core::OpenOptions::new(Arc::new(SqliteDialect)),
        )
        .map_err(|error| sql_error(&error))?;
        let reader = self.reader()?;
        let mut tables = Vec::new();
        for name in self.document_collections()? {
            let fields = reader.head_schema_fields(&name)?.unwrap_or_default();
            register(
                &database,
                CollectionTable::new(Arc::clone(&shared), &name, fields),
            )?;
            tables.push(name);
        }
        for command in Command::ALL {
            register(&database, CommandTable::new(Arc::clone(&shared), command))?;
        }
        let connection = database.connect().map_err(|error| sql_error(&error))?;
        Ok(Sql {
            connection,
            database,
            tables,
            shared,
        })
    }

    /// Every document collection of the root, by name.
    fn document_collections(&self) -> Result<Vec<String>, Error> {
        let directory = self.path().join(".collections");
        let entries = match fylo_vfs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::new("ENATIVE_IO", error.to_string())),
        };
        let reader = self.reader()?;
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| Error::new("ENATIVE_IO", error.to_string()))?;
            let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
                continue;
            };
            if reader.collection_kind(&name).ok()
                == Some(fylo_storage_native::CollectionKind::Document)
            {
                names.push(name);
            }
        }
        names.sort();
        Ok(names)
    }
}

fn register<T: turso_core::InternalVirtualTable + 'static>(
    database: &Arc<Database>,
    table: T,
) -> Result<(), Error> {
    let name = table.name();
    let sql = table.sql();
    let table = VirtualTable::new_internal(
        name,
        sql,
        VTabKind::VirtualTable,
        Arc::new(RwLock::new(table)),
    )
    .map_err(|error| sql_error(&error))?;
    database
        .register_virtual_table(Arc::new(table))
        .map_err(|error| sql_error(&error))?;
    Ok(())
}

impl Sql {
    /// The collection tables this connection registered.
    #[must_use]
    pub fn tables(&self) -> &[String] {
        &self.tables
    }

    /// Run one statement with positional parameters (`?` or `?NNN`) and
    /// return how many rows it changed.
    ///
    /// # Errors
    ///
    /// A FYLO error keeps its code (`EARRAYOFOBJECTS`, `EFYLO_TXN_SCOPE`, …);
    /// any other SQL error has the code `ESQL`.
    pub fn execute(&self, sql: &str, parameters: &[SqlValue]) -> Result<u64, Error> {
        let mut statement = self.prepare(sql)?;
        Self::bind_positional(&mut statement, parameters)?;
        self.run(statement).map(|(changes, _)| changes)
    }

    /// Run one statement with named parameters (`:name`, `@name` or
    /// `$name`; give the name with or without its prefix).
    ///
    /// # Errors
    ///
    /// As [`Sql::execute`], plus `ESQL` for a name the statement lacks.
    pub fn execute_named(&self, sql: &str, parameters: &[(&str, SqlValue)]) -> Result<u64, Error> {
        let mut statement = self.prepare(sql)?;
        Self::bind_named(&mut statement, parameters)?;
        self.run(statement).map(|(changes, _)| changes)
    }

    /// Run a query with positional parameters and return its rows.
    ///
    /// # Errors
    ///
    /// As [`Sql::execute`].
    pub fn query(&self, sql: &str, parameters: &[SqlValue]) -> Result<SqlRows, Error> {
        let mut statement = self.prepare(sql)?;
        Self::bind_positional(&mut statement, parameters)?;
        self.run(statement).map(|(_, rows)| rows)
    }

    /// Run a query with named parameters and return its rows.
    ///
    /// # Errors
    ///
    /// As [`Sql::execute_named`].
    pub fn query_named(
        &self,
        sql: &str,
        parameters: &[(&str, SqlValue)],
    ) -> Result<SqlRows, Error> {
        let mut statement = self.prepare(sql)?;
        Self::bind_named(&mut statement, parameters)?;
        self.run(statement).map(|(_, rows)| rows)
    }

    fn prepare(&self, sql: &str) -> Result<Statement, Error> {
        self.connection
            .prepare(sql)
            .map_err(|error| sql_error(&error))
    }

    fn bind_positional(statement: &mut Statement, parameters: &[SqlValue]) -> Result<(), Error> {
        for (offset, parameter) in parameters.iter().enumerate() {
            let index = NonZero::new(offset + 1)
                .ok_or_else(|| Error::new("ESQL", "parameter index overflow"))?;
            statement
                .bind_at(index, parameter.to_turso())
                .map_err(|error| sql_error(&error))?;
        }
        Ok(())
    }

    fn bind_named(statement: &mut Statement, parameters: &[(&str, SqlValue)]) -> Result<(), Error> {
        for (name, parameter) in parameters {
            let index = Self::parameter_index(statement, name).ok_or_else(|| {
                Error::new("ESQL", format!("the statement has no parameter {name}"))
            })?;
            statement
                .bind_at(index, parameter.to_turso())
                .map_err(|error| sql_error(&error))?;
        }
        Ok(())
    }

    fn parameter_index(statement: &Statement, name: &str) -> Option<NonZero<usize>> {
        let parameters = statement.parameters();
        if name.starts_with([':', '@', '$']) {
            return parameters.index(name);
        }
        [":", "@", "$"]
            .iter()
            .find_map(|prefix| parameters.index(format!("{prefix}{name}")))
    }

    fn run(&self, mut statement: Statement) -> Result<(u64, SqlRows), Error> {
        let before = self.shared.rows_written.load(Ordering::Acquire);
        let columns = (0..statement.num_columns())
            .map(|index| statement.get_column_name(index).into_owned())
            .collect();
        let rows = statement
            .run_collect_rows()
            .map_err(|error| sql_error(&error))?;
        let rows = rows
            .iter()
            .map(|row| row.iter().map(SqlValue::from_turso).collect())
            .collect();
        let written = self.shared.rows_written.load(Ordering::Acquire) - before;
        let changes = u64::try_from(statement.n_change())
            .unwrap_or(0)
            .max(written);
        drop(statement);
        let _ = &self.database;
        Ok((changes, SqlRows { columns, rows }))
    }
}

/// A Turso error as a FYLO error: the FYLO code it carries (`EXXX: …`
/// anywhere in its message), or `ESQL`.
fn sql_error(error: &LimboError) -> Error {
    let message = error.to_string();
    let code = message
        .split(|character: char| {
            !(character.is_ascii_uppercase()
                || character.is_ascii_digit()
                || character == '_'
                || character == ':')
        })
        .find_map(|token| {
            let code = token.strip_suffix(':')?;
            (code.len() > 2
                && code.starts_with('E')
                && code.chars().all(|character| {
                    character.is_ascii_uppercase() || character.is_ascii_digit() || character == '_'
                }))
            .then_some(code)
        })
        .unwrap_or("ESQL")
        .to_owned();
    Error::new(&code, message)
}
