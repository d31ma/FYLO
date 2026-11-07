use crate::ReadDocument;

/// One parsed, decoded document.
#[derive(Debug)]
pub(crate) struct ResidentDocument {
    record: ReadDocument,
}

impl ResidentDocument {
    pub(crate) const fn new(record: ReadDocument) -> Self {
        Self { record }
    }

    pub(crate) const fn record(&self) -> &ReadDocument {
        &self.record
    }
}
