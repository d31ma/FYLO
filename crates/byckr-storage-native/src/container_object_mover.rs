use byckr_format::{ContainerExtentKind, ContainerObjectLayout};

use crate::{
    ContainerFile, ContainerObject, ContainerObjectError, ContainerObjectMove,
    ContainerObjectOpener,
};

/// Moves a large object's extents nearer the front of the file, for
/// `fylo compact --shrink` (ADR 0034 section 6).
///
/// The sealed chunk frames are copied unchanged: nothing is encrypted
/// again, so no key ever seals a chunk number twice, and the frames stay
/// valid because neither their checksum nor their tag names an extent.
pub struct ContainerObjectMover;

impl ContainerObjectMover {
    /// Copy every extent of `object` that has a lower free extent into it,
    /// and flush. Returns `None` when no extent can move. The caller then
    /// commits a relocation record with the move's object and calls
    /// [`ContainerObjectMove::commit`].
    ///
    /// # Errors
    ///
    /// Returns the container's failure, after releasing any copies made.
    pub fn relocate(
        container: &mut ContainerFile,
        object: &ContainerObject,
    ) -> Result<Option<ContainerObjectMove>, ContainerObjectError> {
        let slot_bytes = Self::layout_for(container, object).slot_bytes;
        let mut planned = ContainerObjectMove {
            object: object.clone(),
            moved_from: Vec::new(),
            moved_to: Vec::new(),
        };
        let result = Self::copy_lower(container, object, slot_bytes, &mut planned);
        Self::settle(container, planned, result)
    }

    fn layout_for(container: &ContainerFile, object: &ContainerObject) -> ContainerObjectLayout {
        let tag = u64::from(object.salt.is_some()) * ContainerObjectOpener::TAG_BYTES as u64;
        ContainerObjectLayout::for_extent(container.superblock().extent_bytes(), tag)
    }

    fn copy_lower(
        container: &mut ContainerFile,
        object: &ContainerObject,
        slot_bytes: u64,
        planned: &mut ContainerObjectMove,
    ) -> Result<(), ContainerObjectError> {
        let mut extents = Vec::new();
        for extent in object.extents() {
            extents.push(Self::place_extent(
                container,
                object,
                extent,
                (slot_bytes, planned),
            )?);
        }
        planned.object.runs = ContainerObject::runs_from(&extents);
        Ok(())
    }

    fn place_extent(
        container: &mut ContainerFile,
        object: &ContainerObject,
        extent: u64,
        (slot_bytes, planned): (u64, &mut ContainerObjectMove),
    ) -> Result<u64, ContainerObjectError> {
        if !Self::has_lower_free(container, extent) {
            return Ok(extent);
        }
        let target = container
            .allocate(ContainerExtentKind::Object, object.target.collection)?
            .index;
        planned.moved_to.push(target);
        Self::copy_extent(container, (extent, target), slot_bytes)?;
        planned.moved_from.push(extent);
        Ok(target)
    }

    fn has_lower_free(container: &ContainerFile, extent: u64) -> bool {
        container
            .allocator()
            .free_extents()
            .first()
            .is_some_and(|free| *free < extent)
    }

    fn copy_extent(
        container: &ContainerFile,
        (from, to): (u64, u64),
        slot_bytes: u64,
    ) -> Result<(), ContainerObjectError> {
        let capacity = container.payload_capacity();
        let mut buffer = vec![0_u8; Self::to_usize(slot_bytes.min(capacity))?];
        let mut offset = 0;
        while offset < capacity {
            let length = Self::to_usize((capacity - offset).min(slot_bytes))?;
            container.read_payload(from, offset, &mut buffer[..length])?;
            container.write_payload(to, offset, &buffer[..length])?;
            offset += length as u64;
        }
        Ok(())
    }

    fn settle(
        container: &mut ContainerFile,
        planned: ContainerObjectMove,
        result: Result<(), ContainerObjectError>,
    ) -> Result<Option<ContainerObjectMove>, ContainerObjectError> {
        if let Err(error) = result.and_then(|()| Ok(container.sync()?)) {
            planned.cancel(container)?;
            return Err(error);
        }
        Ok(Some(planned).filter(|planned| !planned.moved_from.is_empty()))
    }

    fn to_usize(value: u64) -> Result<usize, ContainerObjectError> {
        usize::try_from(value)
            .map_err(|_| ContainerObjectError::damaged("a size does not fit in memory"))
    }
}
