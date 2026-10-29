use std::path::Path;

use super::super::{NativeWriteRoot, sync_parent};
use super::database_pack_report::DatabasePackReport;
use super::segment_compactor::SegmentCompactor;
use crate::container_database::ContainerDatabase;
use crate::{
    DatabaseKey, NativeCollection, NativeRoot, NativeStorageError, NativeStorageErrorCode,
};

/// What `fylo rekey` leaves a `.vdbx` database encrypted with (ADR 0016
/// section 9).
#[derive(Clone, Copy, Debug)]
pub enum DatabaseRekey<'a> {
    /// Encrypt an encrypted database with another secret: key rotation.
    Rotate(&'a DatabaseKey),
    /// Encrypt a plain database with a secret (`fylo rekey --encrypt`).
    Encrypt(&'a DatabaseKey),
    /// Write an encrypted database plain (`fylo rekey --decrypt`).
    Decrypt,
}

/// `fylo rekey db.vdbx` (ADR 0016 section 9, `docs/vdbx-segment-backend.md`,
/// "Rekeying").
///
/// A rekey is a `fylo pack` of the database into itself. With the old key
/// checked at open, every collection is locked and recovered, then copied
/// into a staging file beside the database, `.<name>.pack-staging`,
/// encrypted with the new secret under a salt drawn for it, and checked
/// against the source with the pack oracle; the staging file is flushed and
/// renamed over the database, which is the commit point. A rename replaces
/// the name in one step on every filesystem, so at any stop the path names
/// the old file, which opens with the old key, or the new one, which opens
/// with the new key. A stop before the rename leaves the staging file,
/// which is never read and which the next pack or rekey of that name
/// removes; a failure the process survives removes it at once.
///
/// The database is held exclusively throughout, as `fylo compact --shrink`
/// holds it: writers in other processes are waited out first, and refused
/// until the rename. One that opened the file while it waited finds a file
/// with another key salt at the path once it gets in, and opens the new
/// file instead (`ContainerFile::open_writer`); a process that had the old
/// file open to read writes only after opening the path again, and is
/// refused when that is another file (`ContainerOpen::writable`). Readers
/// are never kept out: one that has the old file open reads it on, whole,
/// until it opens the database again.
///
/// A sealed database is never written, so it takes no hold: it is resealed
/// into a new sealed file, as `fylo pack --sealed` writes one, and renamed
/// over the old the same way.
impl NativeWriteRoot {
    /// Rewrite this `.vdbx` database under another key, or encrypt or
    /// decrypt it, keeping every collection with its documents, deleted
    /// documents, schema versions, shard width and validation mode. The
    /// database must have been opened with its key, which is the old key's
    /// check. Handles opened on it before the rekey read the old file on
    /// and refuse every write; open it again to write.
    ///
    /// # Errors
    ///
    /// Returns `ENATIVE_UNSUPPORTED` for a directory root, for a key
    /// rotation of a plain database, an encryption of an encrypted one or a
    /// decryption of a plain one; `EKEY` for an encrypted database opened
    /// without its key; `EROOTLOCKED` when writers in other processes keep
    /// the file past the busy timeout; `EROOTLEASELOST` when this process
    /// lost the file to another writer meanwhile; and an error for a copy
    /// that fails verification or an I/O failure. Every failure before the
    /// rename leaves the database as it was.
    pub fn rekey_database(
        &self,
        rekey: DatabaseRekey<'_>,
    ) -> Result<DatabasePackReport, NativeStorageError> {
        if !NativeCollection::SEGMENTS_SUPPORTED {
            return Err(SegmentCompactor::maintenance_unsupported());
        }
        self.root.assert_profile_owned()?;
        let database = self.require_database("fylo rekey <db.vdbx>")?;
        database.require_key()?;
        Self::require_rekeyable(&database, rekey)?;
        let key = match rekey {
            DatabaseRekey::Rotate(key) | DatabaseRekey::Encrypt(key) => Some(key),
            DatabaseRekey::Decrypt => None,
        };
        let path = self.root.path().to_owned();
        let sealed = database.is_sealed();
        let rewrite = || -> Result<DatabasePackReport, NativeStorageError> {
            let writer = if sealed {
                None
            } else {
                Some(database.hold_writer()?)
            };
            // Listed once no other writer can add a collection, in the
            // order they have, so the new file lists them as the old did.
            let names = database.collection_names();
            let collections = self.packed_collections(&names)?;
            // A database's records have their access enforced in FYLO only
            // already, so copying them changes nothing.
            let (report, built) = Self::renewing(&database, writer.is_some(), || {
                self.stage_pack(&collections.1, &path, (true, key, sealed))
            })?;
            Self::replace_database(&database, writer, (&built, &path))?;
            drop(collections);
            Ok(report)
        };
        let outcome = if sealed {
            rewrite()
        } else {
            database.exclusively(rewrite)
        };
        // The next open in this process reads the file at the path afresh:
        // the new one, or the old one after a failure.
        let closed = NativeRoot::close_database(&path);
        let report = outcome?;
        closed?;
        Ok(report)
    }

    /// Refuse a rekey that does not fit the database: a rotation or a
    /// decryption of a plain one, or an encryption of an encrypted one.
    fn require_rekeyable(
        database: &ContainerDatabase,
        rekey: DatabaseRekey<'_>,
    ) -> Result<(), NativeStorageError> {
        let refusal = match (database.is_encrypted(), rekey) {
            (false, DatabaseRekey::Rotate(_)) => {
                "fylo rekey gives an encrypted .vdbx database another key, and this one is \
                 plain; fylo rekey --encrypt encrypts it"
            }
            (false, DatabaseRekey::Decrypt) => "the .vdbx database is plain already",
            (true, DatabaseRekey::Encrypt(_)) => {
                "the .vdbx database is encrypted already; fylo rekey --new-key gives it \
                 another key"
            }
            _ => return Ok(()),
        };
        Err(Self::pack_unsupported(refusal))
    }

    /// Run `work`, which writes another file while this process holds the
    /// database to itself, renewing this process's lease on the database
    /// meanwhile where it writes under one, so a long copy does not let
    /// another writer take the lease over. A lease lost all the same is
    /// found before the commit ([`Self::replace_database`]).
    #[cfg(not(target_family = "wasm"))]
    fn renewing<T>(database: &ContainerDatabase, holding: bool, work: impl FnOnce() -> T) -> T {
        use std::sync::atomic::{AtomicBool, Ordering};
        if !holding {
            return work();
        }
        let done = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let renewer = scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    // A lost lease is reported before the commit.
                    let _ = database.hold_writer();
                    std::thread::park_timeout(Self::RENEW_EVERY);
                }
            });
            let outcome = work();
            done.store(true, Ordering::Release);
            renewer.thread().unpark();
            outcome
        })
    }

    #[cfg(target_family = "wasm")]
    fn renewing<T>(_database: &ContainerDatabase, _holding: bool, work: impl FnOnce() -> T) -> T {
        work()
    }

    /// How often a rekey checks its hold on the database while it copies.
    /// A lease renews only once a third of its takeover time has passed.
    #[cfg(not(target_family = "wasm"))]
    const RENEW_EVERY: std::time::Duration = std::time::Duration::from_millis(500);

    /// Rename the flushed staging file `built` over the database at `path`:
    /// the commit point. Before it, the hold this process took on the
    /// database as writer `writer` must still be its own, so no other
    /// writer can have written the old file; a sealed database has none.
    /// After it, this process's open of the old file never writes again.
    fn replace_database(
        database: &ContainerDatabase,
        writer: Option<u64>,
        (built, path): (&Path, &Path),
    ) -> Result<(), NativeStorageError> {
        let held = match writer {
            Some(writer) => database.hold_writer().and_then(|now| {
                if now == writer {
                    Ok(())
                } else {
                    Err(NativeStorageError::new(
                        NativeStorageErrorCode::RootLeaseLost,
                        "fylo rekey lost its hold on the .vdbx database while it copied it; \
                         another writer may have written it, so nothing was changed",
                    ))
                }
            }),
            None => Ok(()),
        };
        if let Err(error) =
            held.and_then(|()| fylo_vfs::rename(built, path).map_err(NativeStorageError::io))
        {
            let _ = Self::remove_staging(built);
            return Err(error);
        }
        database.retire();
        // The staging file's lease files, marked free when the build closed
        // it; one left behind is removed by the next pack or rekey.
        let _ = Self::remove_staging(built);
        sync_parent(path)
    }
}
