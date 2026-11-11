use fylo_storage_native::{ChangePage, DocumentChange};
use serde_json::{Value, json};

use crate::MachineError;

/// A position in a collection's change feed (ADR 0024): the collection's
/// change lineage, then the sequence number and identifier of the last
/// change a reader has seen. Unlike a query cursor it holds no state in the
/// process, so it survives restarts and moves between processes.
pub(crate) struct ChangeCursor {
    lineage: String,
    lsn: u64,
    id: String,
}

impl ChangeCursor {
    const PREFIX: &'static str = "fch1";
    const NO_LINEAGE: &'static str = "0";
    const PARTS: usize = 4;
    const DEFAULT_LIMIT: u64 = 256;
    const MAX_LIMIT: u64 = 1000;
    const PUT: &'static str = "put";
    const DELETE: &'static str = "delete";

    pub(crate) fn from_request(request: &Value) -> Result<Self, MachineError> {
        match request.get("after") {
            None | Some(Value::Null) => Ok(Self::start()),
            Some(Value::String(text)) => Self::parse(text),
            Some(_) => Err(MachineError::new(
                "EBADREQUEST",
                "machine request field \"after\" must be a change cursor or null",
            )),
        }
    }

    pub(crate) fn limit(request: &Value) -> Result<usize, MachineError> {
        let limit = request
            .get("limit")
            .map_or(Some(Self::DEFAULT_LIMIT), Value::as_u64);
        Self::bounded(limit)
    }

    pub(crate) fn position(&self) -> (u64, &str) {
        (self.lsn, self.id.as_str())
    }

    pub(crate) fn reply(&self, page: &ChangePage) -> Result<Value, MachineError> {
        self.require_current(page)?;
        let lineage = page.lineage.as_deref().unwrap_or(Self::NO_LINEAGE);
        let next = page.changes.last().map_or_else(
            || self.with_lineage(lineage),
            |last| Self::after_change(lineage, last),
        );
        let changes: Vec<Value> = page.changes.iter().map(Self::change).collect();
        Ok(json!({
            "changes": changes,
            "cursor": next.encode(),
            "more": page.more,
        }))
    }

    fn start() -> Self {
        Self {
            lineage: String::new(),
            lsn: 0,
            id: String::new(),
        }
    }

    fn parse(text: &str) -> Result<Self, MachineError> {
        let mut parts = text.splitn(Self::PARTS, '.');
        let fields = (parts.next(), parts.next(), parts.next(), parts.next());
        let (Some(Self::PREFIX), Some(lineage), Some(lsn), Some(id)) = fields else {
            return Err(Self::invalid());
        };
        if lineage.is_empty() {
            return Err(Self::invalid());
        }
        let lsn = lsn.parse::<u64>().map_err(|_| Self::invalid())?;
        Ok(Self {
            lineage: lineage.to_owned(),
            lsn,
            id: id.to_owned(),
        })
    }

    fn bounded(limit: Option<u64>) -> Result<usize, MachineError> {
        limit
            .filter(|limit| (1..=Self::MAX_LIMIT).contains(limit))
            .and_then(|limit| usize::try_from(limit).ok())
            .ok_or_else(|| {
                MachineError::new(
                    "EBADREQUEST",
                    format!(
                        "machine request field \"limit\" must be an integer from 1 to {}",
                        Self::MAX_LIMIT
                    ),
                )
            })
    }

    fn require_current(&self, page: &ChangePage) -> Result<(), MachineError> {
        if self.lineage.is_empty() {
            return Ok(());
        }
        let lineage = page.lineage.as_deref().unwrap_or(Self::NO_LINEAGE);
        if self.lineage == lineage && self.lsn < page.next_lsn {
            return Ok(());
        }
        Err(MachineError::new(
            "ECURSOR_EXPIRED",
            "the change cursor is from before the collection's change history started again \
             (a drop and re-create, a pack, a rekey, a layout move or an older copy); read the \
             collection again and continue from \"after\": null",
        ))
    }

    fn with_lineage(&self, lineage: &str) -> Self {
        Self {
            lineage: lineage.to_owned(),
            lsn: self.lsn,
            id: self.id.clone(),
        }
    }

    fn after_change(lineage: &str, change: &DocumentChange) -> Self {
        Self {
            lineage: lineage.to_owned(),
            lsn: change.lsn,
            id: change.id.clone(),
        }
    }

    fn encode(&self) -> String {
        format!("{}.{}.{}.{}", Self::PREFIX, self.lineage, self.lsn, self.id)
    }

    fn change(change: &DocumentChange) -> Value {
        let kind = if change.deleted {
            Self::DELETE
        } else {
            Self::PUT
        };
        json!({ "id": change.id, "kind": kind })
    }

    fn invalid() -> MachineError {
        MachineError::new(
            "EINVALIDCURSOR",
            "machine request field \"after\" is not a change cursor this build wrote",
        )
    }
}
