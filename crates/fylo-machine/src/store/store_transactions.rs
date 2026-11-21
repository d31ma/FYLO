use std::collections::{BTreeMap, BTreeSet};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use super::store_transaction::StoreTransaction;
use crate::MachineError;

/// The open Store transactions of one machine session, by handle. A handle
/// belongs to this process only; an unknown handle answers `ETXNUNKNOWN`,
/// and one whose time limit passed answers `ETXNEXPIRED` (`api/store/v1`,
/// "Transactions").
#[derive(Default)]
pub(crate) struct StoreTransactions {
    open: BTreeMap<String, StoreTransaction>,
    expired: BTreeSet<String>,
    sequence: u64,
}

impl StoreTransactions {
    pub(crate) const UNKNOWN: &'static str = "ETXNUNKNOWN";
    pub(crate) const EXPIRED: &'static str = "ETXNEXPIRED";
    pub(crate) const BUSY: &'static str = "ETXNBUSY";

    /// Register an open transaction and answer its handle.
    pub(crate) fn begin(&mut self, transaction: StoreTransaction) -> String {
        self.sequence += 1;
        let handle = format!(
            "fytx1.{}.{}.{}",
            std::process::id(),
            self.sequence,
            Self::stamp()
        );
        self.open.insert(handle.clone(), transaction);
        handle
    }

    /// The live transaction `handle`, for a read or a write inside it.
    pub(crate) fn live(&mut self, handle: &str) -> Result<&mut StoreTransaction, MachineError> {
        self.expire_due(Instant::now());
        self.require_known(handle)?;
        self.open
            .get_mut(handle)
            .ok_or_else(|| Self::unknown(handle))
    }

    /// End the transaction `handle` and hand it to the caller, for a commit
    /// or a rollback.
    pub(crate) fn end(&mut self, handle: &str) -> Result<StoreTransaction, MachineError> {
        self.expire_due(Instant::now());
        self.require_known(handle)?;
        self.open
            .remove(handle)
            .ok_or_else(|| Self::unknown(handle))
    }

    /// Roll back the transaction `handle` if it is open; a failed write
    /// inside it ends it this way.
    pub(crate) fn abandon(&mut self, handle: &str) {
        self.open.remove(handle);
    }

    /// Whether an open transaction holds `collection`, so another write to
    /// it would interleave with the transaction.
    pub(crate) fn holds(&mut self, collection: &str) -> bool {
        self.expire_due(Instant::now());
        self.open
            .values()
            .any(|transaction| transaction.collection() == collection)
    }

    pub(crate) fn busy(collection: &str) -> MachineError {
        MachineError::new(
            Self::BUSY,
            format!(
                "collection {collection} has an open transaction in this process; retry after it ends"
            ),
        )
    }

    fn require_known(&self, handle: &str) -> Result<(), MachineError> {
        if self.expired.contains(handle) {
            return Err(MachineError::new(
                Self::EXPIRED,
                format!("transaction {handle} passed its time limit and was rolled back"),
            ));
        }
        Ok(())
    }

    fn expire_due(&mut self, now: Instant) {
        let due: Vec<String> = self
            .open
            .iter()
            .filter(|(_, transaction)| transaction.is_expired(now))
            .map(|(handle, _)| handle.clone())
            .collect();
        for handle in due {
            self.open.remove(&handle);
            self.expired.insert(handle);
        }
    }

    fn unknown(handle: &str) -> MachineError {
        MachineError::new(
            Self::UNKNOWN,
            format!("transaction {handle} is not open in this fylo process"),
        )
    }

    fn stamp() -> u128 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    }
}
