//! A `.fxdb` container file: create, open and checkpoint (ADR 0016).
//!
//! This layer owns the file's fixed structures — the two superblock copies,
//! the extent map and the collection directory — and hands out extents. It
//! knows nothing about the records inside a segment; the segment store reads
//! and writes extent payloads through [`ContainerFile::read_payload`] and
//! [`ContainerFile::write_payload`].

use std::io;
use std::path::Path;

use fylo_format::{
    ContainerCheckpointSlot, ContainerCollectionHeader, ContainerExtentHeader, ContainerExtentKind,
    ContainerExtentMapPage, ContainerFormatError, ContainerGenerationSlot, ContainerSuperblock,
};
use fylo_vfs::{File, OpenOptions};

use crate::container_allocator::{
    ContainerAllocatorError, ContainerExtentAllocation, ContainerExtentAllocator,
};
use crate::{ContainerCipher, ContainerCipherError, ContainerSealContext};

mod punch;
use punch::punch_hole;
mod snapshot;
mod verify;
pub use verify::{ContainerProblem, ContainerProblemSeverity, ContainerVerifyReport};

/// Why a container operation failed.
#[derive(Debug)]
pub enum ContainerError {
    /// The filesystem refused an operation.
    Io(io::Error),
    /// A structure failed to decode.
    Format(ContainerFormatError),
    /// The extent map refused an operation.
    Allocator(ContainerAllocatorError),
    /// A collection with that name already exists.
    CollectionExists,
    /// No collection block is free in the directory extent.
    DirectoryFull,
    /// The file may only be read (sealed, or a read-only-compatible feature
    /// this build lacks).
    ReadOnly,
    /// Other writers kept the file past the busy timeout (`EBUSY`).
    Busy,
    /// The file is encrypted and has not been unlocked with its key
    /// (`EKEY`).
    Locked,
    /// Encryption refused: a wrong key, or a sealed unit that is damaged.
    Cipher(ContainerCipherError),
    /// The single-writer lease refused or was lost.
    #[cfg(not(target_family = "wasm"))]
    Lease(crate::ContainerLeaseError),
}

impl std::fmt::Display for ContainerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "container I/O failed: {error}"),
            Self::Format(error) => write!(formatter, "container is malformed: {error}"),
            Self::Allocator(error) => write!(formatter, "container extent map refused: {error:?}"),
            Self::CollectionExists => formatter.write_str("container collection already exists"),
            Self::DirectoryFull => formatter.write_str("container collection directory is full"),
            Self::ReadOnly => formatter.write_str("container may only be read"),
            Self::Busy => formatter.write_str("container is busy with other writers"),
            Self::Locked => formatter.write_str("container is encrypted and needs its key"),
            Self::Cipher(error) => write!(formatter, "container encryption: {error}"),
            #[cfg(not(target_family = "wasm"))]
            Self::Lease(error) => write!(formatter, "container lease: {error}"),
        }
    }
}

impl ContainerError {
    /// The stable error code machine clients see.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(error) if error.kind() == io::ErrorKind::NotFound => "ENATIVE_NOT_FOUND",
            Self::Io(error) if error.kind() == io::ErrorKind::PermissionDenied => "EACCES",
            Self::Io(_) => "ENATIVE_IO",
            Self::Format(ContainerFormatError::BadMagic) => "EFXDB_NOT_FXDB",
            Self::Format(
                ContainerFormatError::UnsupportedVersion
                | ContainerFormatError::IncompatibleFeature,
            ) => "EFXDB_VERSION",
            Self::Format(_) | Self::Allocator(_) => "EFXDB_CORRUPT",
            Self::CollectionExists => "EFXDB_EXISTS",
            Self::DirectoryFull => "EFXDB_FULL",
            Self::ReadOnly => "EROFS",
            Self::Busy => "EBUSY",
            Self::Locked | Self::Cipher(ContainerCipherError::WrongKey) => "EKEY",
            Self::Cipher(ContainerCipherError::Unauthenticated) => "EFXDB_CORRUPT",
            Self::Cipher(ContainerCipherError::Random) => "ENATIVE_IO",
            #[cfg(not(target_family = "wasm"))]
            Self::Lease(crate::ContainerLeaseError::Busy { .. }) => "EBUSY",
            #[cfg(not(target_family = "wasm"))]
            Self::Lease(crate::ContainerLeaseError::Lost) => "EFXDB_LEASE_LOST",
            #[cfg(not(target_family = "wasm"))]
            Self::Lease(crate::ContainerLeaseError::Io(_)) => "ENATIVE_IO",
        }
    }
}

impl std::error::Error for ContainerError {}

impl From<io::Error> for ContainerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ContainerFormatError> for ContainerError {
    fn from(error: ContainerFormatError) -> Self {
        Self::Format(error)
    }
}

impl From<ContainerAllocatorError> for ContainerError {
    fn from(error: ContainerAllocatorError) -> Self {
        Self::Allocator(error)
    }
}

impl From<ContainerCipherError> for ContainerError {
    fn from(error: ContainerCipherError) -> Self {
        Self::Cipher(error)
    }
}

/// One collection as the directory records it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContainerCollection {
    /// Its block number in the directory extent.
    pub block: u32,
    /// The static header.
    pub header: ContainerCollectionHeader,
    /// The newest valid generation slot, if any has been published.
    pub generation: Option<ContainerGenerationSlot>,
    /// The newest valid checkpoint slot, if the collection has checkpointed.
    pub checkpoint: Option<ContainerCheckpointSlot>,
}

/// What `fylo inspect` reports about a container. It reads only the
/// superblock, map and directory, never a record.
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerReport {
    /// Always `fylo.fxdb`.
    pub format: &'static str,
    /// Superblock generation in force.
    pub generation: u64,
    /// Bytes per extent.
    pub extent_bytes: u64,
    /// Extents the map describes, including the superblock region.
    pub extent_count: u64,
    /// The file's length.
    pub file_bytes: u64,
    /// Whether records are encrypted. Off unless the database asked for it.
    pub encrypted: bool,
    /// Whether the file is encrypted and was opened without its key, so
    /// the collections are not listed.
    pub locked: bool,
    /// Whether the file is a sealed, read-only pack.
    pub sealed: bool,
    /// Codecs some frame may use.
    pub codecs: Vec<&'static str>,
    /// Extents by state and kind.
    pub extents: crate::ContainerExtentCensus,
    /// The collections, in directory order.
    pub collections: Vec<ContainerCollectionReport>,
}

/// One collection in a [`ContainerReport`].
#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContainerCollectionReport {
    /// Its name.
    pub name: String,
    /// Its id.
    pub id: u32,
    /// Whether it was dropped and awaits release.
    pub dropped: bool,
    /// Records committed, as its newest generation slot counts them.
    pub records: u64,
    /// The newest checkpoint's LSN, if it has checkpointed.
    pub checkpoint_lsn: Option<u64>,
}

/// The file under a container: natively the shared lock handle, which every
/// open of the file in the process uses, and in WebAssembly the host file.
#[cfg(not(target_family = "wasm"))]
pub(crate) type Handle = crate::ByteRangeLockFile;
#[cfg(target_family = "wasm")]
pub(crate) type Handle = File;

/// An open `.fxdb` container.
#[derive(Debug)]
pub struct ContainerFile {
    handle: Handle,
    writable: bool,
    superblock: ContainerSuperblock,
    superblock_in_a: bool,
    allocator: ContainerExtentAllocator,
    collections: Vec<ContainerCollection>,
    cipher: Option<ContainerCipher>,
}

const SUPERBLOCK_BYTES: u64 = ContainerSuperblock::ENCODED_BYTES as u64;
const PAGE_BYTES: u64 = 4096;
const BLOCK_BYTES: u64 = ContainerCollectionHeader::BLOCK_BYTES as u64;

impl ContainerFile {
    /// Create a new, empty container at `path`, which must not exist.
    ///
    /// The file holds the superblock region, one directory extent and one
    /// map extent. Every byte the creation writes is flushed before this
    /// returns; the caller flushes the parent directory.
    ///
    /// # Errors
    ///
    /// Returns the creation, write or flush failure, or `InvalidField` for an
    /// extent shift outside the format's range.
    pub fn create(path: &Path, extent_shift: u8) -> Result<(), ContainerError> {
        Self::create_with(path, extent_shift, None)
    }

    /// Create a new, empty container whose records, directory, dictionaries
    /// and snapshots are sealed with a key derived from `secret` (ADR 0016
    /// section 9). Encryption is opt-in; [`Self::create`] makes a plain file.
    ///
    /// # Errors
    ///
    /// The same as [`Self::create`], and `Cipher(Random)` when no salt can be
    /// drawn.
    pub fn create_encrypted(
        path: &Path,
        extent_shift: u8,
        secret: &[u8],
    ) -> Result<(), ContainerError> {
        Self::create_with(path, extent_shift, Some(secret))
    }

    fn create_with(
        path: &Path,
        extent_shift: u8,
        secret: Option<&[u8]>,
    ) -> Result<(), ContainerError> {
        if !(ContainerSuperblock::MIN_EXTENT_SHIFT..=ContainerSuperblock::MAX_EXTENT_SHIFT)
            .contains(&extent_shift)
        {
            return Err(ContainerFormatError::InvalidField.into());
        }
        let (incompat, key_salt, key_check) = match secret {
            Some(secret) => {
                let salt = ContainerCipher::new_salt()?;
                let check = ContainerCipher::derive(secret, &salt).key_check();
                (ContainerSuperblock::INCOMPAT_ENCRYPTED, salt, check)
            }
            None => (0, [0; 32], [0; 16]),
        };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut allocator = ContainerExtentAllocator::new(1);
        let directory = allocator.allocate(ContainerExtentKind::Directory, 0);
        let map = allocator.allocate(ContainerExtentKind::Map, 0);
        let extent_bytes = 1_u64 << extent_shift;
        file.set_len(allocator.extent_count() * extent_bytes)?;
        write_extent_header(
            &file,
            extent_bytes,
            directory,
            ContainerExtentKind::Directory,
            0,
        )?;
        write_extent_header(&file, extent_bytes, map, ContainerExtentKind::Map, 0)?;
        write_map(&file, extent_bytes, &[map.index], &allocator.map_pages())?;
        let superblock = ContainerSuperblock {
            generation: 1,
            extent_shift,
            extent_count: allocator.extent_count(),
            next_extent_sequence: allocator.next_sequence(),
            directory_extent: directory.index,
            map_first_extent: map.index,
            map_extent_count: 1,
            compat: 0,
            read_only_compat: 0,
            incompat,
            key_salt,
            key_check,
        };
        write_all_at(&file, &superblock.encode(), 0)?;
        crate::sync_handle(&file)?;
        Ok(())
    }

    /// Open an existing container for reading and writing, with no
    /// coordination with other writers: for a host that coordinates them
    /// itself, as the browser does with a Web Lock, and for tests. Natively,
    /// use [`Self::open_writer`].
    ///
    /// # Errors
    ///
    /// Returns the open or read failure, a decoding error for the
    /// superblocks, map or directory, `Allocator(InconsistentMap)` when the
    /// map does not describe the file, and `ReadOnly` for a file this build
    /// may only read.
    pub fn open_unchecked(path: &Path) -> Result<Self, ContainerError> {
        #[cfg(not(target_family = "wasm"))]
        let handle = crate::ByteRangeLockFile::open(path)?;
        #[cfg(target_family = "wasm")]
        let handle = OpenOptions::new().read(true).write(true).open(path)?;
        Self::from_handle(handle, true)
    }

    /// Open a container to read. It takes no lock and is never blocked by
    /// writers; it works on read-only files and mounts, and on sealed files.
    /// Every write fails with `ReadOnly`. An encrypted file opens locked:
    /// call [`Self::unlock`] before reading its collections.
    ///
    /// # Errors
    ///
    /// Returns the open or read failure, or a decoding error.
    pub fn open_reader(path: &Path) -> Result<Self, ContainerError> {
        #[cfg(not(target_family = "wasm"))]
        let handle = crate::ByteRangeLockFile::open_read_only(path)?;
        #[cfg(target_family = "wasm")]
        let handle = OpenOptions::new().read(true).open(path)?;
        Self::from_handle(handle, false)
    }

    pub(crate) fn from_handle(handle: Handle, writable: bool) -> Result<Self, ContainerError> {
        let mut container = Self {
            handle,
            writable,
            superblock: placeholder_superblock(),
            superblock_in_a: true,
            allocator: ContainerExtentAllocator::new(1),
            collections: Vec::new(),
            cipher: None,
        };
        let mut copies = vec![0_u8; 2 * ContainerSuperblock::ENCODED_BYTES];
        read_exact_at(container.file(), &mut copies, 0)?;
        let (superblock, in_a) = ContainerSuperblock::newest(
            &copies[..ContainerSuperblock::ENCODED_BYTES],
            &copies[ContainerSuperblock::ENCODED_BYTES..],
        )?;
        if writable && superblock.requires_read_only() {
            return Err(ContainerError::ReadOnly);
        }
        container.superblock = superblock;
        container.superblock_in_a = in_a;
        container.allocator = container.read_map()?;
        // An encrypted directory waits for the key.
        if !container.is_encrypted() {
            container.collections = container.read_directory()?;
        }
        // An older superblock copy can describe more extents than a later
        // shrink left in the file; give them back their zeros.
        let required = container.allocator.extent_count() * container.extent_bytes();
        if writable && container.file().metadata()?.len() < required {
            container.file().set_len(required)?;
        }
        Ok(container)
    }

    /// Read the superblocks again and, when another process has
    /// checkpointed the map since this open last read it, the map and the
    /// directory too. Returns whether it read them.
    ///
    /// A reader writes nothing, so the map on disk is never older than its
    /// own. A writer under the lease is the only one to checkpoint, so for
    /// it this finds nothing new. A writer sharing the file with writers in
    /// other processes reads their checkpoints too, and loses what its own
    /// map changed since its last checkpoint: it refreshes under the
    /// allocation byte ([`Self::lock_allocation`]) before it changes the
    /// map, and checkpoints before it lets the byte go, so nothing it took
    /// or released is lost; a take-back in memory only ([`Self::reclaim`])
    /// or a seal not yet checkpointed must be made again.
    ///
    /// # Errors
    ///
    /// Returns the read failure or a decoding error; the open keeps what it
    /// had.
    pub fn refresh(&mut self) -> Result<bool, ContainerError> {
        let mut copies = vec![0_u8; 2 * ContainerSuperblock::ENCODED_BYTES];
        read_exact_at(self.file(), &mut copies, 0)?;
        let (superblock, in_a) = ContainerSuperblock::newest(
            &copies[..ContainerSuperblock::ENCODED_BYTES],
            &copies[ContainerSuperblock::ENCODED_BYTES..],
        )?;
        if superblock.generation == self.superblock.generation {
            return Ok(false);
        }
        let previous = std::mem::replace(&mut self.superblock, superblock);
        let allocator = match self.read_map() {
            Ok(allocator) => allocator,
            Err(error) => {
                self.superblock = previous;
                return Err(error);
            }
        };
        let collections = if self.is_locked() {
            Vec::new()
        } else {
            match self.read_directory() {
                Ok(collections) => collections,
                Err(error) => {
                    self.superblock = previous;
                    return Err(error);
                }
            }
        };
        self.superblock_in_a = in_a;
        self.allocator = allocator;
        self.collections = collections;
        Ok(true)
    }

    fn file(&self) -> &File {
        #[cfg(not(target_family = "wasm"))]
        {
            self.handle.file()
        }
        #[cfg(target_family = "wasm")]
        {
            &self.handle
        }
    }

    /// Whether this open may write.
    #[must_use]
    pub fn is_writable(&self) -> bool {
        self.writable
    }

    /// Take the writer byte of the collection in directory block `block`
    /// exclusive, waiting up to `wait`, so one process at a time writes
    /// that collection; dropping the guard lets the next in. Under the
    /// lease the byte is decided inside this process alone.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when another writer keeps it past `wait`, `ReadOnly`
    /// for a reader, or the lock failure.
    #[cfg(not(target_family = "wasm"))]
    pub fn lock_collection_writer(
        &self,
        block: u32,
        wait: std::time::Duration,
    ) -> Result<crate::ByteLockGuard, ContainerError> {
        lock_byte(
            &self.lock_handle()?,
            crate::LockByte::collection_writer(block),
            wait,
        )
    }

    /// Take the allocation byte exclusive, waiting up to `wait`, so one
    /// process at a time takes, seals, releases and checkpoints extents
    /// (ADR 0016 section 7). A writer sharing the file with others holds it
    /// across [`Self::refresh`], its change to the map and the
    /// [`Self::checkpoint`] that records it. Under the lease the byte is
    /// decided inside this process alone.
    ///
    /// # Errors
    ///
    /// Returns `Busy` when another writer keeps it past `wait`, `ReadOnly`
    /// for a reader, or the lock failure.
    #[cfg(not(target_family = "wasm"))]
    pub fn lock_allocation(
        &self,
        wait: std::time::Duration,
    ) -> Result<crate::ByteLockGuard, ContainerError> {
        lock_byte(&self.lock_handle()?, crate::LockByte::ALLOCATION, wait)
    }

    /// The lock handle of a writable open, to take a byte through without
    /// borrowing the open while waiting ([`lock_byte`]).
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` or `Locked`.
    #[cfg(not(target_family = "wasm"))]
    pub(crate) fn lock_handle(&self) -> Result<crate::ByteRangeLockFile, ContainerError> {
        self.require_writable()?;
        Ok(self.handle.clone())
    }

    /// Whether the file was created encrypted.
    #[must_use]
    pub fn is_encrypted(&self) -> bool {
        self.superblock.incompat & ContainerSuperblock::INCOMPAT_ENCRYPTED != 0
    }

    /// Whether the file is encrypted and not yet unlocked.
    #[must_use]
    pub fn is_locked(&self) -> bool {
        self.is_encrypted() && self.cipher.is_none()
    }

    /// Unlock an encrypted file with its secret and read its directory. A
    /// plain file needs no key, and this does nothing for one.
    ///
    /// # Errors
    ///
    /// Returns `Cipher(WrongKey)` for a secret that is not this file's,
    /// `Cipher(Unauthenticated)` for a damaged directory block, or the read
    /// failure.
    pub fn unlock(&mut self, secret: &[u8]) -> Result<(), ContainerError> {
        if !self.is_encrypted() || self.cipher.is_some() {
            return Ok(());
        }
        self.cipher = Some(ContainerCipher::open_file(
            secret,
            &self.superblock.key_salt,
            &self.superblock.key_check,
        )?);
        match self.read_directory() {
            Ok(collections) => {
                self.collections = collections;
                Ok(())
            }
            Err(error) => {
                self.cipher = None;
                Err(error)
            }
        }
    }

    /// The file key, for the segment store to seal records, dictionaries
    /// and snapshots with. `None` for a plain file or a locked one.
    #[must_use]
    pub fn cipher(&self) -> Option<&ContainerCipher> {
        self.cipher.as_ref()
    }

    fn require_writable(&self) -> Result<(), ContainerError> {
        if !self.writable {
            Err(ContainerError::ReadOnly)
        } else if self.is_locked() {
            Err(ContainerError::Locked)
        } else {
            Ok(())
        }
    }

    /// The superblock in force.
    #[must_use]
    pub fn superblock(&self) -> &ContainerSuperblock {
        &self.superblock
    }

    /// The collections in the directory, in block order.
    #[must_use]
    pub fn collections(&self) -> &[ContainerCollection] {
        &self.collections
    }

    /// The extent map, for planning compaction and shrinking.
    #[must_use]
    pub fn allocator(&self) -> &ContainerExtentAllocator {
        &self.allocator
    }

    fn extent_bytes(&self) -> u64 {
        self.superblock.extent_bytes()
    }

    fn payload_offset(&self, extent: u64, offset: u64, length: usize) -> io::Result<u64> {
        let capacity = self.extent_bytes() - ContainerExtentHeader::PAYLOAD_OFFSET;
        let end = offset.checked_add(length as u64);
        if extent == 0
            || extent >= self.allocator.extent_count()
            || end.is_none_or(|end| end > capacity)
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "container payload range is outside its extent",
            ));
        }
        Ok(extent * self.extent_bytes() + ContainerExtentHeader::PAYLOAD_OFFSET + offset)
    }

    /// Bytes of payload one extent holds.
    #[must_use]
    pub fn payload_capacity(&self) -> u64 {
        self.extent_bytes() - ContainerExtentHeader::PAYLOAD_OFFSET
    }

    /// Read payload bytes of one extent.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a range outside the extent, or the read
    /// failure.
    pub fn read_payload(&self, extent: u64, offset: u64, buffer: &mut [u8]) -> io::Result<()> {
        let at = self.payload_offset(extent, offset, buffer.len())?;
        read_exact_at(self.file(), buffer, at)
    }

    /// Write payload bytes of one extent, without flushing.
    ///
    /// # Errors
    ///
    /// Returns `InvalidInput` for a range outside the extent, or the write
    /// failure.
    pub fn write_payload(&self, extent: u64, offset: u64, bytes: &[u8]) -> io::Result<()> {
        if !self.writable {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "container is open to read",
            ));
        }
        let at = self.payload_offset(extent, offset, bytes.len())?;
        write_all_at(self.file(), bytes, at)
    }

    /// Read and check an extent's header against what a location expects.
    ///
    /// # Errors
    ///
    /// Returns the read failure, `InvalidInput` for an extent no file could
    /// hold, or a decoding error; `InvalidField` means the extent was freed
    /// and reused, and the caller retries.
    pub fn expect_extent(
        &self,
        extent: u64,
        kind: ContainerExtentKind,
        owner: u32,
        sequence: u64,
    ) -> Result<ContainerExtentHeader, ContainerError> {
        // The extent comes from a location the file itself recorded, so it is
        // untrusted. One past the map is not refused: a reader's map can
        // predate extents a writer has added since.
        let at = extent.checked_mul(self.extent_bytes()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "container extent is beyond any file",
            )
        })?;
        let mut bytes = [0_u8; ContainerExtentHeader::ENCODED_BYTES];
        read_exact_at(self.file(), &mut bytes, at)?;
        Ok(ContainerExtentHeader::expect(
            &bytes, extent, kind, owner, sequence,
        )?)
    }

    /// Flush every write made so far.
    ///
    /// # Errors
    ///
    /// Returns the flush failure.
    pub fn sync(&self) -> io::Result<()> {
        crate::sync_handle(self.file())
    }

    /// Take an extent and write its header, growing the file if needed. The
    /// header is not flushed; the commit that first uses the extent flushes
    /// it with its own record.
    ///
    /// A reused segment extent is cleared first, by a hole where the
    /// filesystem punches one and with zeros elsewhere, because a segment's
    /// end is found by scanning to its last valid frame and must never reach
    /// a former owner's frames. Other kinds record their lengths.
    ///
    /// # Errors
    ///
    /// Returns the write or resize failure.
    pub fn allocate(
        &mut self,
        kind: ContainerExtentKind,
        owner: u32,
    ) -> Result<ContainerExtentAllocation, ContainerError> {
        self.require_writable()?;
        let allocation = self.allocator.allocate(kind, owner);
        if allocation.grows_file {
            self.file()
                .set_len(self.allocator.extent_count() * self.extent_bytes())?;
        } else if kind == ContainerExtentKind::Segment {
            self.clear_extent(allocation.index)?;
        }
        write_extent_header(self.file(), self.extent_bytes(), allocation, kind, owner)?;
        Ok(allocation)
    }

    fn clear_extent(&self, extent: u64) -> io::Result<()> {
        let start = extent * self.extent_bytes();
        if punch_hole(self.file(), start, self.extent_bytes())? {
            return Ok(());
        }
        let zeros = vec![0_u8; 1 << 20];
        let mut at = start;
        let end = start + self.extent_bytes();
        while at < end {
            let length =
                usize::try_from(end - at).map_or(zeros.len(), |left| left.min(zeros.len()));
            write_all_at(self.file(), &zeros[..length], at)?;
            at += length as u64;
        }
        Ok(())
    }

    /// Mark an extent immutable.
    ///
    /// # Errors
    ///
    /// Returns `Allocator` when the extent is not in use.
    pub fn seal(&mut self, extent: u64, live_bytes: u64) -> Result<(), ContainerError> {
        self.require_writable()?;
        Ok(self.allocator.seal(extent, live_bytes)?)
    }

    /// Release an extent a committed compaction replaced. It is reused only
    /// after the next checkpoint.
    ///
    /// # Errors
    ///
    /// Returns `Allocator` when the extent is not in use.
    pub fn release(&mut self, extent: u64) -> Result<(), ContainerError> {
        self.require_writable()?;
        Ok(self.allocator.release(extent)?)
    }

    /// Add a collection to the directory and flush it.
    ///
    /// # Errors
    ///
    /// Returns `CollectionExists`, `DirectoryFull`, a format error for an
    /// over-long name or descriptor, or the write or flush failure.
    pub fn add_collection(
        &mut self,
        name: &str,
        descriptor: &[u8],
    ) -> Result<&ContainerCollection, ContainerError> {
        self.require_writable()?;
        if self
            .collections
            .iter()
            .any(|collection| !collection.header.dropped && collection.header.name == name)
        {
            return Err(ContainerError::CollectionExists);
        }
        let block = self.collections.len();
        if block as u64 >= self.directory_blocks() {
            return Err(ContainerError::DirectoryFull);
        }
        let id = self
            .collections
            .iter()
            .map(|collection| collection.header.id)
            .max()
            .unwrap_or(0)
            + 1;
        let header = ContainerCollectionHeader {
            id,
            name: name.to_owned(),
            dropped: false,
            descriptor: descriptor.to_vec(),
        };
        let block_number = u32::try_from(block).map_err(|_| ContainerError::DirectoryFull)?;
        let mut bytes = match &self.cipher {
            Some(cipher) => ContainerCollectionHeader::encode_sealed(&cipher.seal(
                ContainerSealContext::Directory {
                    block: block_number,
                },
                &header.encode_body()?,
            )?)?,
            None => header.encode()?,
        };
        bytes.resize(ContainerCollectionHeader::BLOCK_BYTES, 0);
        self.write_payload(
            self.superblock.directory_extent,
            block as u64 * BLOCK_BYTES,
            &bytes,
        )?;
        self.sync()?;
        self.collections.push(ContainerCollection {
            block: block_number,
            header,
            generation: None,
            checkpoint: None,
        });
        Ok(&self.collections[block])
    }

    /// Drop the collection named `name`: rewrite its block's static part
    /// with the dropped flag, flush, and release every extent it owns. The
    /// extents are reused after the next checkpoint; the block never is, so
    /// a block number names one collection for the life of the file.
    /// Returns the dropped collection's block.
    ///
    /// The slots are left as they were. A crash before the flush leaves the
    /// collection in place; one after it leaves its extents owned until the
    /// next writer that opens the file releases them.
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` or `Locked`, `InvalidInput` when no live
    /// collection has that name, or the write or flush failure.
    pub fn drop_collection(&mut self, name: &str) -> Result<u32, ContainerError> {
        self.require_writable()?;
        let collection = self
            .collections
            .iter()
            .find(|collection| !collection.header.dropped && collection.header.name == name)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "no such container collection")
            })?;
        let (block, header_id) = (collection.block, collection.header.id);
        let header = ContainerCollectionHeader {
            dropped: true,
            ..collection.header.clone()
        };
        let bytes = match &self.cipher {
            Some(cipher) => ContainerCollectionHeader::encode_sealed(&cipher.seal(
                ContainerSealContext::Directory { block },
                &header.encode_body()?,
            )?)?,
            None => header.encode()?,
        };
        self.write_payload(
            self.superblock.directory_extent,
            u64::from(block) * BLOCK_BYTES,
            &bytes,
        )?;
        self.sync()?;
        self.collection_mut(block)?.header = header;
        self.release_owned_by(header_id)?;
        Ok(block)
    }

    /// Release every extent a dropped collection still owns: one a crash
    /// left after [`Self::drop_collection`] flushed the dropped flag.
    ///
    /// # Errors
    ///
    /// Returns `ReadOnly` or `Locked`.
    pub fn release_dropped(&mut self) -> Result<(), ContainerError> {
        self.require_writable()?;
        let dropped: Vec<u32> = self
            .collections
            .iter()
            .filter(|collection| collection.header.dropped)
            .map(|collection| collection.header.id)
            .collect();
        for id in dropped {
            self.release_owned_by(id)?;
        }
        Ok(())
    }

    fn release_owned_by(&mut self, owner: u32) -> Result<(), ContainerError> {
        for kind in [
            ContainerExtentKind::Segment,
            ContainerExtentKind::Dictionary,
            ContainerExtentKind::Locations,
            ContainerExtentKind::Index,
        ] {
            for (extent, _) in self.allocator.owned(kind, owner) {
                self.allocator.release(extent)?;
            }
        }
        Ok(())
    }

    /// Publish a collection's generation in place, into the slot that does
    /// not hold its newest one, without flushing (ADR 0015 section 6).
    ///
    /// # Errors
    ///
    /// Returns the write failure, or `InvalidInput` for an unknown block.
    pub fn publish_generation(
        &mut self,
        block: u32,
        slot: ContainerGenerationSlot,
    ) -> Result<(), ContainerError> {
        self.require_writable()?;
        let directory = self.superblock.directory_extent;
        let at =
            self.slot_pair_offset(block, ContainerCollectionHeader::GENERATION_SLOTS_OFFSET)?;
        let mut pair = [0_u8; 2 * ContainerGenerationSlot::ENCODED_BYTES];
        self.read_payload(directory, at, &mut pair)?;
        let into_b = ContainerGenerationSlot::newest(&pair).is_some_and(|(_, in_a)| in_a);
        let offset = if into_b {
            at + ContainerGenerationSlot::ENCODED_BYTES as u64
        } else {
            at
        };
        self.write_payload(directory, offset, &slot.encode())?;
        self.collection_mut(block)?.generation = Some(slot);
        Ok(())
    }

    /// Read a collection's newest valid generation slot from the file, as
    /// another handle on it may have published it since this one opened.
    ///
    /// # Errors
    ///
    /// Returns the read failure, or `InvalidInput` for an unknown block.
    pub fn read_generation(
        &self,
        block: u32,
    ) -> Result<Option<ContainerGenerationSlot>, ContainerError> {
        let at =
            self.slot_pair_offset(block, ContainerCollectionHeader::GENERATION_SLOTS_OFFSET)?;
        let mut pair = [0_u8; 2 * ContainerGenerationSlot::ENCODED_BYTES];
        self.read_payload(self.superblock.directory_extent, at, &mut pair)?;
        Ok(ContainerGenerationSlot::newest(&pair).map(|(slot, _)| slot))
    }

    /// Read a collection's newest valid checkpoint slot from the file.
    ///
    /// # Errors
    ///
    /// Returns the read failure, or `InvalidInput` for an unknown block.
    pub fn read_checkpoint(
        &self,
        block: u32,
    ) -> Result<Option<ContainerCheckpointSlot>, ContainerError> {
        let at =
            self.slot_pair_offset(block, ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET)?;
        let mut pair = [0_u8; 2 * ContainerCheckpointSlot::ENCODED_BYTES];
        self.read_payload(self.superblock.directory_extent, at, &mut pair)?;
        Ok(ContainerCheckpointSlot::newest(&pair).map(|(slot, _)| slot))
    }

    /// Take back an extent the durable map shows free although a slot
    /// names it: one taken after the last checkpoint by a process that
    /// stopped before the next. Its header must show it holds allocation
    /// `sequence` of `kind` for `owner`. The sequence counter moves past it,
    /// so no later allocation repeats the number.
    ///
    /// A reader may take an extent back too: only its map in memory
    /// changes, so it can read the extent a writer's slot names before the
    /// writer's next checkpoint shows it.
    ///
    /// # Errors
    ///
    /// Returns `InvalidField` when the header holds something else,
    /// `Allocator(InconsistentMap)` when the map shows the extent in use,
    /// or the read failure.
    pub fn reclaim(
        &mut self,
        extent: u64,
        kind: ContainerExtentKind,
        owner: u32,
        sequence: u64,
    ) -> Result<(), ContainerError> {
        self.expect_extent(extent, kind, owner, sequence)?;
        Ok(self.allocator.reclaim(extent, kind, owner, sequence)?)
    }

    /// Record a collection's checkpoint in the slot that does not hold its
    /// newest one, and flush.
    ///
    /// # Errors
    ///
    /// Returns the write or flush failure, or `InvalidInput` for an unknown
    /// block.
    pub fn record_checkpoint(
        &mut self,
        block: u32,
        slot: ContainerCheckpointSlot,
    ) -> Result<(), ContainerError> {
        self.require_writable()?;
        let directory = self.superblock.directory_extent;
        let at =
            self.slot_pair_offset(block, ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET)?;
        let mut pair = [0_u8; 2 * ContainerCheckpointSlot::ENCODED_BYTES];
        self.read_payload(directory, at, &mut pair)?;
        let into_b = ContainerCheckpointSlot::newest(&pair).is_some_and(|(_, in_a)| in_a);
        let offset = if into_b {
            at + ContainerCheckpointSlot::ENCODED_BYTES as u64
        } else {
            at
        };
        self.write_payload(directory, offset, &slot.encode())?;
        self.sync()?;
        self.collection_mut(block)?.checkpoint = Some(slot);
        Ok(())
    }

    /// Checkpoint the extent map: write a new copy into fresh extents, flush,
    /// then point the other superblock copy at it and flush again.
    ///
    /// The old map's extents are released before the new map is built, so
    /// the new map already shows them free; they become reusable once the
    /// new superblock is durable. A crash before that leaves the old
    /// superblock pointing at the old, untouched map.
    ///
    /// # Errors
    ///
    /// Returns the write, resize or flush failure.
    pub fn checkpoint(&mut self) -> Result<(), ContainerError> {
        self.require_writable()?;
        let old_first = self.superblock.map_first_extent;
        for extent in old_first..old_first + u64::from(self.superblock.map_extent_count) {
            self.allocator.release(extent)?;
        }
        // The run may grow the file and so the map; size it for the worst
        // case, where every extent of the run is new.
        let pages_per_extent = self.payload_capacity() / PAGE_BYTES;
        let mut run = 1_u64;
        while run * pages_per_extent < map_pages_for(self.allocator.extent_count() + run) {
            run += 1;
        }
        let run = usize::try_from(run).map_err(|_| ContainerFormatError::Oversized)?;
        let map_extents = self
            .allocator
            .allocate_run(ContainerExtentKind::Map, 0, run);
        let required = self.allocator.extent_count() * self.extent_bytes();
        if self.file().metadata()?.len() < required {
            self.file().set_len(required)?;
        }
        let indices: Vec<u64> = map_extents
            .iter()
            .map(|allocation| allocation.index)
            .collect();
        for allocation in &map_extents {
            write_extent_header(
                self.file(),
                self.extent_bytes(),
                *allocation,
                ContainerExtentKind::Map,
                0,
            )?;
        }
        write_map(
            self.file(),
            self.extent_bytes(),
            &indices,
            &self.allocator.map_pages(),
        )?;
        self.sync()?;

        let mut next = self.superblock.clone();
        next.generation += 1;
        next.extent_count = self.allocator.extent_count();
        next.next_extent_sequence = self.allocator.next_sequence();
        next.map_first_extent = indices[0];
        next.map_extent_count =
            u32::try_from(indices.len()).map_err(|_| ContainerFormatError::Oversized)?;
        let at = if self.superblock_in_a {
            SUPERBLOCK_BYTES
        } else {
            0
        };
        write_all_at(self.file(), &next.encode(), at)?;
        self.sync()?;
        self.superblock = next;
        self.superblock_in_a = !self.superblock_in_a;
        self.allocator.checkpoint_completed();
        Ok(())
    }

    /// Return the free extents at the end of the file to the filesystem:
    /// checkpoint a map that no longer describes them, then cut the file.
    /// A crash before the cut leaves a longer file, which opens as before.
    /// Returns the bytes released.
    ///
    /// # Errors
    ///
    /// Returns the checkpoint or resize failure.
    pub fn shrink_free_tail(&mut self) -> Result<u64, ContainerError> {
        self.require_writable()?;
        let before = self.file().metadata()?.len();
        if self.allocator.truncate_free_tail() == self.superblock.extent_count {
            return Ok(0);
        }
        self.checkpoint()?;
        let length = self.allocator.extent_count() * self.extent_bytes();
        if length < before {
            self.file().set_len(length)?;
            self.sync()?;
        }
        Ok(before.saturating_sub(length))
    }

    /// Punch holes over the free extents, where the filesystem can, so their
    /// space goes back to the disk while the file keeps its length. Returns
    /// how many extents were punched; zero where the platform or filesystem
    /// cannot punch holes, which only means the space is kept.
    ///
    /// # Errors
    ///
    /// Returns a failure other than the filesystem not supporting it.
    pub fn punch_free_extents(&self) -> Result<u64, ContainerError> {
        self.require_writable()?;
        let mut punched = 0;
        for extent in self.allocator.free_extents() {
            if !punch_hole(
                self.file(),
                extent * self.extent_bytes(),
                self.extent_bytes(),
            )? {
                break;
            }
            punched += 1;
        }
        Ok(punched)
    }

    /// Describe the container for `fylo inspect`.
    ///
    /// # Errors
    ///
    /// Returns the failure to read the file's length.
    pub fn report(&self) -> Result<ContainerReport, ContainerError> {
        let superblock = &self.superblock;
        let mut codecs = Vec::new();
        if superblock.incompat & ContainerSuperblock::INCOMPAT_CODEC_ZSTD != 0 {
            codecs.push("zstd");
        }
        if superblock.incompat & ContainerSuperblock::INCOMPAT_CODEC_LZ4 != 0 {
            codecs.push("lz4");
        }
        Ok(ContainerReport {
            format: "fylo.fxdb",
            generation: superblock.generation,
            extent_bytes: self.extent_bytes(),
            extent_count: superblock.extent_count,
            file_bytes: self.file().metadata()?.len(),
            encrypted: self.is_encrypted(),
            locked: self.is_locked(),
            sealed: superblock.is_sealed(),
            codecs,
            extents: self.allocator.census(),
            collections: self
                .collections
                .iter()
                .map(|collection| ContainerCollectionReport {
                    name: collection.header.name.clone(),
                    id: collection.header.id,
                    dropped: collection.header.dropped,
                    records: collection
                        .generation
                        .as_ref()
                        .map_or(0, |slot| slot.record_count),
                    checkpoint_lsn: collection.checkpoint.as_ref().map(|slot| slot.lsn),
                })
                .collect(),
        })
    }

    fn directory_blocks(&self) -> u64 {
        self.payload_capacity() / BLOCK_BYTES
    }

    fn slot_pair_offset(&self, block: u32, within: usize) -> Result<u64, ContainerError> {
        if !self
            .collections
            .iter()
            .any(|collection| collection.block == block)
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "unknown collection block").into(),
            );
        }
        Ok(u64::from(block) * BLOCK_BYTES + within as u64)
    }

    fn collection_mut(&mut self, block: u32) -> Result<&mut ContainerCollection, ContainerError> {
        self.collections
            .iter_mut()
            .find(|collection| collection.block == block)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "unknown collection block").into()
            })
    }

    fn read_map(&self) -> Result<ContainerExtentAllocator, ContainerError> {
        let superblock = &self.superblock;
        let pages = map_pages_for(superblock.extent_count);
        let pages_per_extent = self.payload_capacity() / PAGE_BYTES;
        if pages > u64::from(superblock.map_extent_count) * pages_per_extent {
            return Err(ContainerAllocatorError::InconsistentMap.into());
        }
        let mut decoded = Vec::new();
        let mut page_bytes = vec![0_u8; ContainerExtentMapPage::ENCODED_BYTES];
        for page in 0..pages {
            let extent = superblock.map_first_extent + page / pages_per_extent;
            let at = extent * superblock.extent_bytes()
                + ContainerExtentHeader::PAYLOAD_OFFSET
                + (page % pages_per_extent) * PAGE_BYTES;
            read_exact_at(self.file(), &mut page_bytes, at)?;
            decoded.push(ContainerExtentMapPage::decode(&page_bytes)?);
        }
        Ok(ContainerExtentAllocator::from_map(
            superblock.extent_count,
            superblock.next_extent_sequence,
            &decoded,
        )?)
    }

    fn read_directory(&self) -> Result<Vec<ContainerCollection>, ContainerError> {
        let mut collections = Vec::new();
        let mut block_bytes = vec![0_u8; ContainerCollectionHeader::BLOCK_BYTES];
        for block in 0..self.directory_blocks() {
            self.read_payload(
                self.superblock.directory_extent,
                block * BLOCK_BYTES,
                &mut block_bytes,
            )?;
            if block_bytes.iter().all(|byte| *byte == 0) {
                break;
            }
            let block = u32::try_from(block).map_err(|_| ContainerFormatError::Oversized)?;
            let header = match &self.cipher {
                Some(cipher) => ContainerCollectionHeader::decode_body(&cipher.open(
                    ContainerSealContext::Directory { block },
                    ContainerCollectionHeader::decode_sealed(&block_bytes)?,
                )?)?,
                None => ContainerCollectionHeader::decode(&block_bytes)?,
            };
            let checkpoint_at = ContainerCollectionHeader::CHECKPOINT_SLOTS_OFFSET;
            let generation_at = ContainerCollectionHeader::GENERATION_SLOTS_OFFSET;
            collections.push(ContainerCollection {
                block,
                header,
                checkpoint: ContainerCheckpointSlot::newest(
                    &block_bytes[checkpoint_at..generation_at],
                )
                .map(|(slot, _)| slot),
                generation: ContainerGenerationSlot::newest(&block_bytes[generation_at..])
                    .map(|(slot, _)| slot),
            });
        }
        Ok(collections)
    }
}

/// Take `byte` exclusive through `handle`, waiting up to `wait`; a wait
/// that runs out is `Busy`.
///
/// # Errors
///
/// Returns `Busy`, or the lock failure.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn lock_byte(
    handle: &crate::ByteRangeLockFile,
    byte: crate::LockByte,
    wait: std::time::Duration,
) -> Result<crate::ByteLockGuard, ContainerError> {
    handle
        .lock(byte, crate::LockMode::Exclusive, wait)
        .map_err(|error| {
            if error.kind() == io::ErrorKind::TimedOut {
                ContainerError::Busy
            } else {
                ContainerError::Io(error)
            }
        })
}

/// Map pages describing a file of `extent_count` extents; extent 0 has none.
fn map_pages_for(extent_count: u64) -> u64 {
    extent_count
        .saturating_sub(1)
        .div_ceil(ContainerExtentMapPage::ENTRIES_PER_PAGE as u64)
}

fn placeholder_superblock() -> ContainerSuperblock {
    ContainerSuperblock {
        generation: 0,
        extent_shift: ContainerSuperblock::DEFAULT_EXTENT_SHIFT,
        extent_count: 0,
        next_extent_sequence: 0,
        directory_extent: 0,
        map_first_extent: 0,
        map_extent_count: 0,
        compat: 0,
        read_only_compat: 0,
        incompat: 0,
        key_salt: [0; 32],
        key_check: [0; 16],
    }
}

fn write_extent_header(
    file: &File,
    extent_bytes: u64,
    allocation: ContainerExtentAllocation,
    kind: ContainerExtentKind,
    owner: u32,
) -> io::Result<()> {
    let header = ContainerExtentHeader {
        kind,
        owner,
        sequence: allocation.sequence,
        index: allocation.index,
    };
    write_all_at(file, &header.encode(), allocation.index * extent_bytes)
}

fn write_map(
    file: &File,
    extent_bytes: u64,
    extents: &[u64],
    pages: &[ContainerExtentMapPage],
) -> Result<(), ContainerError> {
    let pages_per_extent = (extent_bytes - ContainerExtentHeader::PAYLOAD_OFFSET) / PAGE_BYTES;
    for (page_number, page) in pages.iter().enumerate() {
        let page_number = page_number as u64;
        let extent = extents
            .get(
                usize::try_from(page_number / pages_per_extent)
                    .map_err(|_| ContainerFormatError::Oversized)?,
            )
            .copied()
            .ok_or(ContainerFormatError::Oversized)?;
        let at = extent * extent_bytes
            + ContainerExtentHeader::PAYLOAD_OFFSET
            + (page_number % pages_per_extent) * PAGE_BYTES;
        write_all_at(file, &page.encode()?, at)?;
    }
    Ok(())
}

#[cfg(unix)]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(file, buffer, offset)
}

#[cfg(unix)]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    std::os::unix::fs::FileExt::write_all_at(file, bytes, offset)
}

#[cfg(windows)]
fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buffer.is_empty() {
        match std::os::windows::fs::FileExt::seek_read(file, buffer, offset)? {
            0 => return Err(io::ErrorKind::UnexpectedEof.into()),
            read => {
                buffer = &mut buffer[read..];
                offset += read as u64;
            }
        }
    }
    Ok(())
}

#[cfg(windows)]
fn write_all_at(file: &File, mut bytes: &[u8], mut offset: u64) -> io::Result<()> {
    while !bytes.is_empty() {
        match std::os::windows::fs::FileExt::seek_write(file, bytes, offset)? {
            0 => return Err(io::ErrorKind::WriteZero.into()),
            written => {
                bytes = &bytes[written..];
                offset += written as u64;
            }
        }
    }
    Ok(())
}

// The host table is positional, and `&File` keeps its own cursor; a wasm
// module is single-threaded, so seeking then reading cannot interleave.
#[cfg(target_family = "wasm")]
fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> io::Result<()> {
    use std::io::{Read, Seek, SeekFrom};
    let mut handle = file;
    handle.seek(SeekFrom::Start(offset))?;
    handle.read_exact(buffer)
}

#[cfg(target_family = "wasm")]
fn write_all_at(file: &File, bytes: &[u8], offset: u64) -> io::Result<()> {
    use std::io::{Seek, SeekFrom, Write};
    let mut handle = file;
    handle.seek(SeekFrom::Start(offset))?;
    handle.write_all(bytes)
}

#[cfg(test)]
mod tests;
