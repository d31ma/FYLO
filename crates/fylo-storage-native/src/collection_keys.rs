use std::sync::Arc;

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::KeyInit;
use zeroize::Zeroizing;

/// The keys of one collection an identity holds, newest epoch first. The
/// key bytes are kept so an admin can wrap them for other identities.
#[derive(Clone)]
pub(crate) struct CollectionKeys {
    epochs: Vec<CollectionEpoch>,
}

#[derive(Clone)]
struct CollectionEpoch {
    epoch: u32,
    key: Zeroizing<[u8; 32]>,
    cipher: Arc<Aes256Gcm>,
}

impl CollectionKeys {
    pub(crate) fn new() -> CollectionKeys {
        CollectionKeys { epochs: Vec::new() }
    }

    pub(crate) fn add(&mut self, epoch: u32, key: &[u8; 32]) {
        self.epochs.retain(|held| held.epoch != epoch);
        self.epochs.push(CollectionEpoch {
            epoch,
            key: Zeroizing::new(*key),
            cipher: Arc::new(Aes256Gcm::new(key.into())),
        });
        self.epochs
            .sort_by_key(|held| std::cmp::Reverse(held.epoch));
    }

    pub(crate) fn newest(&self) -> Option<Arc<Aes256Gcm>> {
        self.epochs.first().map(|held| Arc::clone(&held.cipher))
    }

    pub(crate) fn all(&self) -> Vec<Arc<Aes256Gcm>> {
        self.epochs
            .iter()
            .map(|held| Arc::clone(&held.cipher))
            .collect()
    }

    /// The epoch after the newest.
    pub(crate) fn next_epoch(&self) -> u32 {
        self.epochs.first().map_or(1, |held| held.epoch + 1)
    }

    /// Every epoch and its key bytes, newest first.
    pub(crate) fn keys(&self) -> Vec<(u32, Zeroizing<[u8; 32]>)> {
        self.epochs
            .iter()
            .map(|held| (held.epoch, held.key.clone()))
            .collect()
    }
}
