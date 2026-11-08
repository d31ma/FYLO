use fylo_storage_native::{CollectionKind, GenerationState, LogPosition};

use crate::lazy_migration::MigrationGuard;

/// What a collection was when its resident entries were read: its kind, the
/// stable generation and write-ahead log end they belong to, the fields its
/// schema encrypts, and the fields its index cannot answer for while it has
/// documents below its head schema version.
#[derive(Debug)]
pub(crate) struct ResidentCollection {
    kind: CollectionKind,
    generation: u64,
    log: Option<LogPosition>,
    encrypted_fields: Vec<String>,
    migration_guard: Option<MigrationGuard>,
}

impl ResidentCollection {
    pub(crate) fn new(
        kind: CollectionKind,
        state: GenerationState,
        encrypted_fields: Vec<String>,
        migration_guard: Option<MigrationGuard>,
    ) -> Self {
        Self {
            kind,
            generation: state.generation,
            log: state.log,
            encrypted_fields,
            migration_guard,
        }
    }

    /// Whether storage is still exactly as it was when this was read: the
    /// same generation and the same log end. A generation number alone can
    /// come back after a collection is replaced; a log segment's random
    /// identity and record count cannot.
    pub(crate) fn read_at(&self, state: &GenerationState) -> bool {
        state.generation == self.generation && state.log == self.log
    }

    pub(crate) const fn kind(&self) -> CollectionKind {
        self.kind
    }

    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn encrypted_fields(&self) -> &[String] {
        &self.encrypted_fields
    }

    pub(crate) const fn migration_guard(&self) -> Option<&MigrationGuard> {
        self.migration_guard.as_ref()
    }

    pub(crate) fn is_document_collection(&self) -> bool {
        self.kind == CollectionKind::Document
    }
}
