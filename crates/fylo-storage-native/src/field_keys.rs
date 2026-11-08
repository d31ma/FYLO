use zeroize::Zeroizing;

/// The keys of one epoch of a collection's `$encrypted` fields in a
/// protected `.vdbx` database (ADR 0021 section 7): an AES-256-GCM key for
/// the values and an HMAC-SHA256 key for their blind-index tokens.
#[derive(Clone)]
pub struct FieldKey {
    epoch: u32,
    encryption: Zeroizing<[u8; 32]>,
    index: Zeroizing<[u8; 32]>,
}

impl FieldKey {
    pub(crate) fn new(epoch: u32, material: &[u8; 64]) -> FieldKey {
        let mut encryption = Zeroizing::new([0_u8; 32]);
        encryption.copy_from_slice(&material[..32]);
        let mut index = Zeroizing::new([0_u8; 32]);
        index.copy_from_slice(&material[32..]);
        FieldKey {
            epoch,
            encryption,
            index,
        }
    }

    /// The collection key epoch this key is derived from.
    #[must_use]
    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    /// The AES-256-GCM key of field values.
    #[must_use]
    pub fn encryption(&self) -> &[u8; 32] {
        &self.encryption
    }

    /// The HMAC-SHA256 key of blind-index tokens.
    #[must_use]
    pub fn index(&self) -> &[u8; 32] {
        &self.index
    }
}

impl std::fmt::Debug for FieldKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FieldKey")
            .field("epoch", &self.epoch)
            .finish_non_exhaustive()
    }
}

/// Every field key of one collection an identity holds, newest epoch
/// first. A value is written with the newest and read with each in turn.
#[derive(Clone, Debug)]
pub struct FieldKeys {
    keys: Vec<FieldKey>,
}

impl FieldKeys {
    pub(crate) fn new(keys: Vec<FieldKey>) -> FieldKeys {
        FieldKeys { keys }
    }

    /// The key new values are written with.
    #[must_use]
    pub fn newest(&self) -> Option<&FieldKey> {
        self.keys.first()
    }

    /// Every key, newest first.
    #[must_use]
    pub fn all(&self) -> &[FieldKey] {
        &self.keys
    }
}
