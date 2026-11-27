use crate::{ContainerFile, ContainerObject, ContainerObjectError};

/// An object copied to lower extents by [`crate::ContainerObjectMover`],
/// waiting for its relocation record (ADR 0034 section 6).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerObjectMove {
    /// The object with its new runs: the fields of the relocation record.
    pub object: ContainerObject,
    /// The extents the frames were copied from.
    pub moved_from: Vec<u64>,
    /// The extents the frames were copied to.
    pub moved_to: Vec<u64>,
}

impl ContainerObjectMove {
    /// Release the old extents, once the relocation record is committed.
    /// They become free at the next checkpoint, so a reader that still
    /// holds the old runs reads the old frames until then.
    ///
    /// # Errors
    ///
    /// Returns the container's failure.
    pub fn commit(
        self,
        container: &mut ContainerFile,
    ) -> Result<ContainerObject, ContainerObjectError> {
        Self::release_all(container, &self.moved_from)?;
        Ok(self.object)
    }

    /// Release the copies instead, when the relocation record was not
    /// committed.
    ///
    /// # Errors
    ///
    /// Returns the container's failure.
    pub fn cancel(self, container: &mut ContainerFile) -> Result<(), ContainerObjectError> {
        Self::release_all(container, &self.moved_to)
    }

    fn release_all(
        container: &mut ContainerFile,
        extents: &[u64],
    ) -> Result<(), ContainerObjectError> {
        for extent in extents {
            container.release(*extent)?;
        }
        Ok(())
    }
}
