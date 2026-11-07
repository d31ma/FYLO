use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::KeyInit;

/// The keys of one collection an identity holds, newest epoch first, and
/// whether it may write the collection.
#[derive(Clone)]
pub(crate) struct CollectionKeys {
    epochs: Vec<(u32, Arc<Aes256Gcm>)>,
}

impl CollectionKeys {
    pub(crate) fn new() -> CollectionKeys {
        CollectionKeys { epochs: Vec::new() }
    }

    pub(crate) fn add(&mut self, epoch: u32, key: &[u8; 32]) {
        self.epochs.retain(|(held, _)| *held != epoch);
        self.epochs
            .push((epoch, Arc::new(Aes256Gcm::new(key.into()))));
        self.epochs
            .sort_by_key(|(epoch, _)| std::cmp::Reverse(*epoch));
    }

    pub(crate) fn newest(&self) -> Option<Arc<Aes256Gcm>> {
        self.epochs.first().map(|(_, key)| Arc::clone(key))
    }

    pub(crate) fn all(&self) -> Vec<Arc<Aes256Gcm>> {
        self.epochs.iter().map(|(_, key)| Arc::clone(key)).collect()
    }
}
