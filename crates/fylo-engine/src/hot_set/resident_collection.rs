use fylo_storage_native::CollectionKind;

/// What a collection was when its resident entries were read: its kind, the
/// stable generation they belong to, and the fields its schema encrypts.
#[derive(Debug)]
pub(crate) struct ResidentCollection {
    kind: CollectionKind,
    generation: u64,
    encrypted_fields: Vec<String>,
}

impl ResidentCollection {
    pub(crate) const fn new(
        kind: CollectionKind,
        generation: u64,
        encrypted_fields: Vec<String>,
    ) -> Self {
        Self {
            kind,
            generation,
            encrypted_fields,
        }
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

    pub(crate) fn is_document_collection(&self) -> bool {
        self.kind == CollectionKind::Document
    }
}
