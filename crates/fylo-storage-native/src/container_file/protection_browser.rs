//! The browser build has no application identities (ADR 0021, phase 5): a
//! protected `.vdbx` file opens locked there, and every hook the native
//! build runs for one does nothing.

use super::{ContainerError, ContainerFile};

impl ContainerFile {
    #[allow(clippy::unused_self)]
    pub(super) fn locked_error(&self) -> ContainerError {
        ContainerError::Locked
    }

    #[allow(clippy::unused_self)]
    pub(super) fn bind_access(&self) {}

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(super) fn reload_access(&self) -> Result<(), ContainerError> {
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(super) fn add_collection_keys(
        &mut self,
        _id: u32,
        _name: &str,
    ) -> Result<(), ContainerError> {
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(super) fn require_admin(&self) -> Result<(), ContainerError> {
        Ok(())
    }

    #[allow(clippy::unnecessary_wraps, clippy::unused_self)]
    pub(super) fn require_collection_write(&self, _id: u32) -> Result<(), ContainerError> {
        Ok(())
    }
}
