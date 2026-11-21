use std::ops::Deref;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::store::Store;
use crate::{Document, Error};

/// An open one-collection transaction (`api/store/v1`, "Transactions"):
/// every [`Store`] call through it reads its own writes, and its writes are
/// held until [`Transaction::commit`], which writes them as one log record,
/// all or none. Dropping it, or [`Transaction::rollback`], writes nothing.
pub struct Transaction<'root, T = Document> {
    store: Store<'root, T>,
}

impl<'root, T: Serialize + DeserializeOwned> Transaction<'root, T> {
    pub(crate) const fn new(store: Store<'root, T>) -> Self {
        Self { store }
    }

    /// Write every held write in one log record; answers how many writes
    /// the transaction held.
    ///
    /// # Errors
    ///
    /// Any error of the commit; then nothing is written.
    pub fn commit(self) -> Result<usize, Error> {
        let held = self.store.held();
        self.store
            .root()
            .write_engine()?
            .store_commit(self.store.collection(), &held)?;
        Ok(held.writes())
    }

    /// End the transaction without writing anything.
    pub fn rollback(self) {
        drop(self);
    }
}

impl<'root, T> Deref for Transaction<'root, T> {
    type Target = Store<'root, T>;

    fn deref(&self) -> &Self::Target {
        &self.store
    }
}
