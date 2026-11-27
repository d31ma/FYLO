use std::collections::HashSet;

use byckr_format::ContainerExtentKind;

use crate::{ContainerFile, ContainerObject, ContainerObjectError};

/// Object extents of one collection that no live record names (ADR 0034
/// section 5): left by a crash after a checkpoint recorded them while the
/// object's write was still running. `fylo verify` reports them and
/// `fylo compact` returns them to the free list.
///
/// Find and reclaim them under the collection's write lock, so no object
/// write is between taking its extents and committing its record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerObjectOrphans {
    /// The orphan extents, lowest first.
    pub extents: Vec<u64>,
}

impl ContainerObjectOrphans {
    /// The object extents of `collection` that none of `live` names.
    #[must_use]
    pub fn find(container: &ContainerFile, collection: u32, live: &[ContainerObject]) -> Self {
        let named = Self::named_extents(collection, live);
        let mut extents: Vec<u64> = container
            .allocator()
            .owned(ContainerExtentKind::Object, collection)
            .into_iter()
            .map(|(extent, _)| extent)
            .filter(|extent| !named.contains(extent))
            .collect();
        extents.sort_unstable();
        Self { extents }
    }

    /// Release every orphan extent; each becomes free at the next
    /// checkpoint. Returns how many were released.
    ///
    /// # Errors
    ///
    /// Returns the container's failure.
    pub fn reclaim(self, container: &mut ContainerFile) -> Result<usize, ContainerObjectError> {
        for extent in &self.extents {
            container.release(*extent)?;
        }
        Ok(self.extents.len())
    }

    fn named_extents(collection: u32, live: &[ContainerObject]) -> HashSet<u64> {
        live.iter()
            .filter(|object| object.target.collection == collection)
            .flat_map(ContainerObject::extents)
            .collect()
    }
}
