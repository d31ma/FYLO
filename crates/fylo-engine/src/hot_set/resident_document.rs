use fylo_storage_native::AccessDescriptor;

use crate::ReadDocument;

/// One parsed, decoded document and the access descriptor stored with it.
///
/// The descriptor is kept rather than a decision, so every read still checks
/// its own actor.
#[derive(Debug)]
pub(crate) struct ResidentDocument {
    record: ReadDocument,
    access: Option<AccessDescriptor>,
}

impl ResidentDocument {
    pub(crate) const fn new(record: ReadDocument, access: Option<AccessDescriptor>) -> Self {
        Self { record, access }
    }

    pub(crate) const fn record(&self) -> &ReadDocument {
        &self.record
    }

    pub(crate) const fn access(&self) -> Option<AccessDescriptor> {
        self.access
    }
}
