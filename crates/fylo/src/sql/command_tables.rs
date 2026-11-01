use std::sync::{Arc, Mutex, PoisonError};

use parking_lot::RwLock;
use serde_json::Value as Json;
use turso_core::{
    Connection, InternalVirtualTable, InternalVirtualTableCursor, LimboError, Numeric, Value,
};
use turso_ext::{
    ConstraintInfo, ConstraintOp, ConstraintUsage, IndexInfo, OrderByInfo, ResultCode,
};

use super::{Shared, SqlValue};
use crate::{ClaimOptions, Error, PublishOptions};

/// FYLO operations that are not documents, as small SQL tables.
///
/// Turso registers scalar functions only through its C extension interface,
/// which FYLO's no-`unsafe` rule keeps out, so the queue and the version are
/// tables instead (ADR 0019 section 3):
///
/// - `INSERT INTO fylo_queue_publish (topic, payload, delay_ms, idempotency_key)`;
///   selecting from it lists this connection's publications with their `id`.
/// - `SELECT id, receipt, attempt, payload FROM fylo_queue_claim WHERE topic = ?
///   AND group_name = ? [AND max_messages = ?] [AND lease_ms = ?] [AND max_attempts = ?]`
///   claims deliveries.
/// - `INSERT INTO fylo_queue_ack (topic, group_name, id, receipt)` and
///   `INSERT INTO fylo_queue_nack (topic, group_name, id, receipt, delay_ms, reason)`
///   settle them.
/// - `SELECT value FROM fylo_info WHERE name = 'version'`.
///
/// Queue writes take effect when the statement runs, not at `COMMIT`: the
/// queue is not part of a document transaction.
#[derive(Debug)]
pub(crate) struct CommandTable {
    shared: Arc<Shared>,
    command: Command,
    published: Arc<Mutex<Vec<Vec<SqlValue>>>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Command {
    Publish,
    Claim,
    Ack,
    Nack,
    Info,
}

pub(crate) struct ListCursor {
    rows: Vec<Vec<SqlValue>>,
    position: usize,
    claim: Option<Arc<Shared>>,
}

impl Command {
    pub(crate) const ALL: [Self; 5] = [
        Self::Publish,
        Self::Claim,
        Self::Ack,
        Self::Nack,
        Self::Info,
    ];

    const fn name(self) -> &'static str {
        match self {
            Self::Publish => "fylo_queue_publish",
            Self::Claim => "fylo_queue_claim",
            Self::Ack => "fylo_queue_ack",
            Self::Nack => "fylo_queue_nack",
            Self::Info => "fylo_info",
        }
    }

    const fn columns(self) -> &'static str {
        match self {
            Self::Publish => {
                "topic TEXT, payload TEXT, delay_ms INTEGER, idempotency_key TEXT, id TEXT, deduplicated INTEGER"
            }
            Self::Claim => {
                "id TEXT, receipt TEXT, attempt INTEGER, payload TEXT, topic HIDDEN, group_name HIDDEN, \
                 max_messages HIDDEN, lease_ms HIDDEN, max_attempts HIDDEN"
            }
            Self::Ack => "topic TEXT, group_name TEXT, id TEXT, receipt TEXT",
            Self::Nack => {
                "topic TEXT, group_name TEXT, id TEXT, receipt TEXT, delay_ms INTEGER, reason TEXT"
            }
            Self::Info => "name TEXT, value TEXT",
        }
    }
}

impl CommandTable {
    const CLAIM_FIRST_ARGUMENT: u32 = 4;
    const CLAIM_ARGUMENTS: u32 = 5;

    pub(crate) fn new(shared: Arc<Shared>, command: Command) -> Self {
        Self {
            shared,
            command,
            published: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn write(&self, columns: &[Value]) -> Result<(), Error> {
        let root = &self.shared.root;
        match self.command {
            Command::Publish => {
                let options = PublishOptions {
                    delay_ms: number(&columns[2], 0)?,
                    idempotency_key: optional_text(&columns[3])?,
                };
                let published = root
                    .queue(&text(&columns[0], "topic")?)
                    .publish(payload(&columns[1]), &options)?;
                let mut log = self
                    .published
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                log.push(vec![
                    SqlValue::from_turso(&columns[0]),
                    SqlValue::from_turso(&columns[1]),
                    SqlValue::Integer(i64::try_from(options.delay_ms).unwrap_or(i64::MAX)),
                    options
                        .idempotency_key
                        .map_or(SqlValue::Null, SqlValue::Text),
                    SqlValue::Text(published.id),
                    SqlValue::Integer(i64::from(published.deduplicated)),
                ]);
                Ok(())
            }
            Command::Ack => root
                .queue(&text(&columns[0], "topic")?)
                .ack(
                    &text(&columns[1], "group_name")?,
                    &text(&columns[2], "id")?,
                    &text(&columns[3], "receipt")?,
                )
                .map(|_| ()),
            Command::Nack => root
                .queue(&text(&columns[0], "topic")?)
                .nack(
                    &text(&columns[1], "group_name")?,
                    &text(&columns[2], "id")?,
                    &text(&columns[3], "receipt")?,
                    number(&columns[4], 0)?,
                    &optional_text(&columns[5])?.unwrap_or_default(),
                )
                .map(|_| ()),
            Command::Claim | Command::Info => Err(Error::new(
                "EFYLO_SQL_READONLY",
                format!("{} is read-only", self.command.name()),
            )),
        }
    }

    fn rows(&self) -> Vec<Vec<SqlValue>> {
        match self.command {
            Command::Publish => self
                .published
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            Command::Info => vec![
                vec![
                    SqlValue::from("version"),
                    SqlValue::from(env!("CARGO_PKG_VERSION")),
                ],
                vec![
                    SqlValue::from("turso_core"),
                    SqlValue::from(super::TURSO_PIN),
                ],
            ],
            _ => Vec::new(),
        }
    }

    fn claim_index(constraints: &[ConstraintInfo]) -> IndexInfo {
        let mut arguments = Vec::new();
        let usages = constraints
            .iter()
            .map(|constraint| {
                let column = constraint.column_index;
                let pushed = constraint.usable
                    && constraint.op == ConstraintOp::Eq
                    && (Self::CLAIM_FIRST_ARGUMENT
                        ..Self::CLAIM_FIRST_ARGUMENT + Self::CLAIM_ARGUMENTS)
                        .contains(&column)
                    && !arguments.contains(&column);
                if pushed {
                    arguments.push(column);
                }
                ConstraintUsage {
                    argv_index: pushed.then(|| u32::try_from(arguments.len()).unwrap_or(u32::MAX)),
                    omit: pushed,
                }
            })
            .collect();
        let complete = [Self::CLAIM_FIRST_ARGUMENT, Self::CLAIM_FIRST_ARGUMENT + 1]
            .iter()
            .all(|column| arguments.contains(column));
        IndexInfo {
            idx_num: 0,
            idx_str: Some(
                arguments
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(","),
            ),
            order_by_consumed: false,
            estimated_cost: if complete { 1.0 } else { 1e12 },
            estimated_rows: 100,
            constraint_usages: usages,
        }
    }
}

impl ListCursor {
    fn claim(
        shared: &Shared,
        arguments: &[Value],
        plan: Option<&str>,
    ) -> Result<Vec<Vec<SqlValue>>, Error> {
        let mut values: [Option<&Value>; 5] = [None; 5];
        for (column, argument) in plan.unwrap_or_default().split(',').zip(arguments) {
            let index = column
                .parse::<u32>()
                .ok()
                .and_then(|column| column.checked_sub(CommandTable::CLAIM_FIRST_ARGUMENT))
                .and_then(|index| usize::try_from(index).ok());
            if let Some(slot) = index.and_then(|index| values.get_mut(index)) {
                *slot = Some(argument);
            }
        }
        let required = |slot: Option<&Value>, name: &str| {
            slot.ok_or_else(|| {
                Error::new(
                    "EBADREQUEST",
                    format!("fylo_queue_claim needs {name} = … in its WHERE clause"),
                )
            })
            .and_then(|value| text(value, name))
        };
        let topic = required(values[0], "topic")?;
        let group = required(values[1], "group_name")?;
        let defaults = ClaimOptions::default();
        let options = ClaimOptions {
            max_messages: values[2]
                .map(|value| number(value, 1))
                .transpose()?
                .map_or(Ok(defaults.max_messages), usize::try_from)
                .map_err(|_| Error::new("EBADREQUEST", "max_messages is out of range"))?,
            visibility_timeout_ms: values[3]
                .map(|value| number(value, defaults.visibility_timeout_ms))
                .transpose()?
                .unwrap_or(defaults.visibility_timeout_ms),
            max_attempts: values[4]
                .map(|value| number(value, 3))
                .transpose()?
                .map_or(Ok(defaults.max_attempts), u32::try_from)
                .map_err(|_| Error::new("EBADREQUEST", "max_attempts is out of range"))?,
            max_bytes: defaults.max_bytes,
        };
        let deliveries = shared.root.queue(&topic).claim(&group, options)?;
        Ok(deliveries
            .into_iter()
            .map(|delivery| {
                vec![
                    SqlValue::Text(delivery.id),
                    SqlValue::Text(delivery.receipt),
                    SqlValue::Integer(i64::from(delivery.attempt)),
                    SqlValue::Text(delivery.payload.to_string()),
                ]
            })
            .collect())
    }
}

fn text(value: &Value, name: &str) -> Result<String, Error> {
    match value {
        Value::Text(text) => Ok(text.as_str().to_owned()),
        _ => Err(Error::new("EBADREQUEST", format!("{name} must be text"))),
    }
}

fn optional_text(value: &Value) -> Result<Option<String>, Error> {
    match value {
        Value::Null => Ok(None),
        Value::Text(text) => Ok(Some(text.as_str().to_owned())),
        _ => Err(Error::new("EBADREQUEST", "expected text or NULL")),
    }
}

fn number(value: &Value, default: u64) -> Result<u64, Error> {
    match value {
        Value::Null => Ok(default),
        Value::Numeric(Numeric::Integer(number)) => u64::try_from(*number)
            .map_err(|_| Error::new("EBADREQUEST", "expected a non-negative integer")),
        _ => Err(Error::new("EBADREQUEST", "expected a non-negative integer")),
    }
}

/// A payload as JSON: its text parsed when it is JSON, otherwise the text.
fn payload(value: &Value) -> Json {
    match value {
        Value::Text(text) => serde_json::from_str(text.as_str())
            .unwrap_or_else(|_| Json::String(text.as_str().to_owned())),
        other => super::value::to_json(other).unwrap_or(Json::Null),
    }
}

impl InternalVirtualTable for CommandTable {
    fn name(&self) -> String {
        self.command.name().to_owned()
    }

    fn open(
        &self,
        _conn: Arc<Connection>,
    ) -> turso_core::Result<Arc<RwLock<dyn InternalVirtualTableCursor>>> {
        Ok(Arc::new(RwLock::new(ListCursor {
            rows: self.rows(),
            position: 0,
            claim: (self.command == Command::Claim).then(|| Arc::clone(&self.shared)),
        })))
    }

    fn best_index(
        &self,
        constraints: &[ConstraintInfo],
        _order_by: &[OrderByInfo],
    ) -> Result<IndexInfo, ResultCode> {
        if self.command == Command::Claim {
            return Ok(Self::claim_index(constraints));
        }
        Ok(IndexInfo {
            constraint_usages: constraints
                .iter()
                .map(|_| ConstraintUsage {
                    argv_index: None,
                    omit: false,
                })
                .collect(),
            ..IndexInfo::default()
        })
    }

    fn sql(&self) -> String {
        format!(
            "CREATE TABLE {} ({})",
            self.command.name(),
            self.command.columns()
        )
    }

    fn readonly(&self) -> bool {
        matches!(self.command, Command::Claim | Command::Info)
    }

    fn update(&self, arguments: &[Value]) -> turso_core::Result<Option<i64>> {
        let [Value::Null, _, columns @ ..] = arguments else {
            return Err(LimboError::ExtensionError(format!(
                "EFYLO_SQL_READONLY: {} accepts INSERT only",
                self.command.name()
            )));
        };
        self.write(columns)
            .map_err(|error| LimboError::ExtensionError(error.to_string()))?;
        self.shared
            .rows_written
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        let rows = self
            .published
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len();
        Ok(Some(i64::try_from(rows).unwrap_or(i64::MAX)))
    }
}

impl InternalVirtualTableCursor for ListCursor {
    fn next(&mut self) -> Result<bool, LimboError> {
        self.position += 1;
        Ok(self.position < self.rows.len())
    }

    fn rowid(&self) -> i64 {
        i64::try_from(self.position).unwrap_or(i64::MAX)
    }

    fn column(&self, column: usize) -> Result<Value, LimboError> {
        Ok(self
            .rows
            .get(self.position)
            .and_then(|row| row.get(column))
            .map_or(Value::Null, SqlValue::to_turso))
    }

    fn filter(
        &mut self,
        arguments: &[Value],
        idx_str: Option<String>,
        _idx_num: i32,
    ) -> Result<bool, LimboError> {
        if let Some(shared) = &self.claim {
            self.rows = Self::claim(shared, arguments, idx_str.as_deref())
                .map_err(|error| LimboError::ExtensionError(error.to_string()))?;
        }
        self.position = 0;
        Ok(!self.rows.is_empty())
    }
}
