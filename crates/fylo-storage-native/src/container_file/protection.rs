//! Protected `.vdbx` files (ADR 0021): the access slots at the end of the
//! directory extent, the unlock with an application identity, and the
//! checks that directory changes make against its grants.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use fylo_format::{ContainerAccessSlot, ContainerExtentHeader, ContainerSuperblock};
use fylo_identity::Identity;

use super::{ACCESS_SLOT_BYTES, ContainerError, ContainerFile, read_exact_at};
use crate::access_binding::AccessBinding;
use crate::{ContainerCipher, DatabaseAccess, DatabaseAccessError, DatabaseIdentity};

impl ContainerFile {
    /// Create a new, empty protected file at `path`, which must not exist:
    /// encrypted, with a random catalog key wrapped for `admin`, and policy
    /// version 1 making `admin` its admin and trust anchor.
    ///
    /// # Errors
    ///
    /// The same as [`Self::create`], and `Access` when the key wrap or the
    /// policy signature fails.
    pub fn create_protected(
        path: &Path,
        extent_shift: u8,
        admin: &Identity,
    ) -> Result<(), ContainerError> {
        let database = *AccessBinding::random_key().map_err(ContainerError::Access)?;
        let catalog_key = AccessBinding::random_key().map_err(ContainerError::Access)?;
        let slot = DatabaseAccess::create(admin, database, &catalog_key)
            .map_err(ContainerError::Access)?;
        let key_check = ContainerCipher::from_key(&catalog_key).key_check();
        let incompat =
            ContainerSuperblock::INCOMPAT_ENCRYPTED | ContainerSuperblock::INCOMPAT_PROTECTED;
        Self::create_raw(
            path,
            extent_shift,
            (incompat, database, key_check),
            Some(&slot),
        )
    }

    /// The access state of a protected file this open unlocked; `None`
    /// otherwise.
    #[must_use]
    pub fn database_access(&self) -> Option<&Arc<DatabaseAccess>> {
        self.cipher.as_ref().and_then(|cipher| cipher.access())
    }

    /// Unlock a protected file as `identity`, and read its directory. A
    /// file that is not protected is left as it is.
    ///
    /// # Errors
    ///
    /// `Access` when the identity has no grant, the policy is refused or the
    /// access document is damaged, or the read failure.
    pub fn unlock_with_identity(
        &mut self,
        identity: &DatabaseIdentity,
    ) -> Result<(), ContainerError> {
        if !self.is_protected() || !self.is_locked() {
            return Ok(());
        }
        let cipher = self.open_access(identity)?;
        self.unlock_with(cipher)
    }

    /// The cipher a protected file opens with as `identity`, which other
    /// handles of the same file share.
    ///
    /// # Errors
    ///
    /// The same as [`Self::unlock_with_identity`].
    pub fn open_access(
        &self,
        identity: &DatabaseIdentity,
    ) -> Result<Arc<ContainerCipher>, ContainerError> {
        let (slot, _) = self.read_access_slot()?;
        DatabaseAccess::open(
            &slot,
            self.superblock.key_salt,
            &self.superblock.key_check,
            identity,
        )
        .map_err(ContainerError::Access)
    }

    /// The newest valid access slot, and whether it is copy A.
    ///
    /// # Errors
    ///
    /// `Access(Damaged)` when neither copy is valid, or the read failure.
    pub fn read_access_slot(&self) -> Result<(ContainerAccessSlot, bool), ContainerError> {
        let slot_bytes = ContainerAccessSlot::SLOT_BYTES;
        let mut copies = vec![0_u8; 2 * slot_bytes];
        let at = self.access_payload_offset(true);
        let offset = self.superblock.directory_extent * self.extent_bytes()
            + ContainerExtentHeader::PAYLOAD_OFFSET
            + at;
        read_exact_at(self.file(), &mut copies, offset)?;
        ContainerAccessSlot::newest(&copies[..slot_bytes], &copies[slot_bytes..]).ok_or_else(|| {
            ContainerError::Access(DatabaseAccessError::Damaged(
                "neither access slot of the protected database is valid".into(),
            ))
        })
    }

    /// The error a locked file gives: no identity for a protected file,
    /// no key for another.
    pub(super) fn locked_error(&self) -> ContainerError {
        if self.is_protected() {
            return ContainerError::Access(DatabaseAccessError::NoIdentity);
        }
        ContainerError::Locked
    }

    /// Tell the access state which collections the directory holds.
    pub(super) fn bind_access(&self) {
        if let Some(access) = self.database_access() {
            let live: Vec<(u32, String)> = self
                .collections
                .iter()
                .filter(|collection| !collection.header.dropped)
                .map(|collection| (collection.header.id, collection.header.name.clone()))
                .collect();
            access.bind_directory(&live);
        }
    }

    /// Take in an access slot another process wrote since this open read
    /// one.
    pub(super) fn reload_access(&self) -> Result<(), ContainerError> {
        let Some(access) = self.database_access() else {
            return Ok(());
        };
        let (slot, _) = self.read_access_slot()?;
        if slot.generation <= access.generation() {
            return Ok(());
        }
        access.reload(&slot).map_err(ContainerError::Access)
    }

    /// Make and wrap the keys of the new collection `id`, and write them to
    /// the access slots before the collection's block names it.
    pub(super) fn add_collection_keys(
        &mut self,
        id: u32,
        name: &str,
    ) -> Result<(), ContainerError> {
        let Some(access) = self.database_access().cloned() else {
            return Ok(());
        };
        let change = access
            .prepare_collection(id, name)
            .map_err(ContainerError::Access)?;
        self.write_access_slot(&change.slot)?;
        access.commit(change).map_err(ContainerError::Access)
    }

    /// Make a change of the access document with `prepare`, from the newest
    /// state another process may have written, write it to the access
    /// slots, flush, and take it in. Returns the policy after it.
    ///
    /// # Errors
    ///
    /// `Access(NoIdentity)` for a file this open did not unlock with an
    /// identity, `prepare`'s refusal, or the write failure.
    pub(crate) fn change_access(
        &mut self,
        prepare: impl FnOnce(
            &DatabaseAccess,
            &BTreeMap<u32, u64>,
        )
            -> Result<crate::database_access::AccessChange, DatabaseAccessError>,
    ) -> Result<crate::AccessPolicy, ContainerError> {
        self.reload_access()?;
        let access = self
            .database_access()
            .cloned()
            .ok_or(ContainerError::Access(DatabaseAccessError::NoIdentity))?;
        let next_lsns = self.next_lsns()?;
        let change = prepare(&access, &next_lsns).map_err(ContainerError::Access)?;
        self.write_access_slot(&change.slot)?;
        access.commit(change).map_err(ContainerError::Access)?;
        Ok(access.policy())
    }

    /// Each live collection's next LSN, from its generation slot as the
    /// file holds it now; a collection that has published none is left out.
    fn next_lsns(&self) -> Result<BTreeMap<u32, u64>, ContainerError> {
        let mut next = BTreeMap::new();
        for collection in self
            .collections
            .iter()
            .filter(|collection| !collection.header.dropped)
        {
            if let Some(slot) = self.read_generation(collection.block)? {
                next.insert(collection.header.id, slot.next_lsn);
            }
        }
        Ok(next)
    }

    /// Refuse unless the file is not protected or this identity is an admin.
    pub(super) fn require_admin(&self) -> Result<(), ContainerError> {
        self.database_access()
            .map_or(Ok(()), |access| access.require_admin())
            .map_err(ContainerError::Access)
    }

    /// Refuse unless the file is not protected or this identity may write
    /// the collection `id`.
    pub(super) fn require_collection_write(&self, id: u32) -> Result<(), ContainerError> {
        self.database_access()
            .map_or(Ok(()), |access| access.require_write(id))
            .map_err(ContainerError::Access)
    }

    /// Write `slot` over the copy that does not hold the newest valid
    /// generation, and flush it: a stop leaves the old slot or the new one.
    pub(crate) fn write_access_slot(
        &self,
        slot: &ContainerAccessSlot,
    ) -> Result<(), ContainerError> {
        let (_, newest_in_a) = self.read_access_slot()?;
        let bytes = slot.encode()?;
        self.write_payload(
            self.superblock.directory_extent,
            self.access_payload_offset(!newest_in_a),
            &bytes,
        )?;
        self.sync()?;
        Ok(())
    }

    fn access_payload_offset(&self, copy_a: bool) -> u64 {
        Self::access_slot_offset(self.extent_bytes(), true) + u64::from(!copy_a) * ACCESS_SLOT_BYTES
    }
}
