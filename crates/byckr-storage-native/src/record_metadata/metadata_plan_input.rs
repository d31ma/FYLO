use super::MetadataChanges;

/// What [`super::MetadataPlan::for_record`] plans from: the changes, and
/// bytes the caller already holds so that planning reads nothing twice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MetadataPlanInput<'a> {
    changes: &'a MetadataChanges,
    payload: Option<&'a [u8]>,
    current_sidecar: Option<&'a [u8]>,
}

impl<'a> MetadataPlanInput<'a> {
    pub(crate) const fn new(changes: &'a MetadataChanges) -> Self {
        Self {
            changes,
            payload: None,
            current_sidecar: None,
        }
    }

    /// The payload bytes about to be written; the record binding is made
    /// from them instead of from the file on disk.
    pub(crate) const fn with_payload(mut self, payload: &'a [u8]) -> Self {
        self.payload = Some(payload);
        self
    }

    /// The sidecar bytes the caller already read, so planning does not read
    /// the sidecar file again.
    pub(crate) const fn with_current_sidecar(mut self, sidecar: &'a [u8]) -> Self {
        self.current_sidecar = Some(sidecar);
        self
    }

    pub(crate) const fn changes(&self) -> &'a MetadataChanges {
        self.changes
    }

    pub(crate) const fn payload(&self) -> Option<&'a [u8]> {
        self.payload
    }

    pub(crate) const fn current_sidecar(&self) -> Option<&'a [u8]> {
        self.current_sidecar
    }
}
