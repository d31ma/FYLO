use super::kernel_file_lock::KernelFileLock;
use super::lease_backoff::LeaseBackoff;
use super::lease_claims::LeaseClaims;
use super::lease_heartbeat::LeaseHeartbeat;
use super::lease_observation::LeaseObservation;
use super::lease_takeover::LeaseTakeover;
use super::lease_timing::LeaseTiming;
use crate::{FilesystemProfile, NativeStorageError, NativeStorageErrorCode, RootFilesystem};
use fylo_vfs::{File, OpenOptions};
use std::fs::TryLockError;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// One owner of a folder: the kernel lock on its lock file plus the current
/// generation of its lease record, kept alive by a heartbeat
/// (ADR 0033 section 4).
///
/// The owned folder is a root today. The files that coordinate its owners
/// live beside it, named after it, so the same lease serves one namespace of
/// an app folder once namespaces exist: only the owned path changes.
#[derive(Debug)]
pub(super) struct RootLeaseInner {
    root: PathBuf,
    owner: String,
    generation: u64,
    scope_name: String,
    lock_file: File,
    record_path: PathBuf,
    kernel_enforced: bool,
    filesystem: RootFilesystem,
    heartbeat: LeaseHeartbeat,
}

impl RootLeaseInner {
    const LOCK_SUFFIX: &str = "fylo-root-owner.lock";
    const RECORD_SUFFIX: &str = "fylo-root-owner.lock.json";
    const OWNER_ID_BYTES: usize = 16;

    pub(super) fn acquire(root: impl AsRef<Path>) -> Result<Self, NativeStorageError> {
        Self::acquire_with_profile(root, FilesystemProfile::Native)
    }

    pub(super) fn acquire_with_profile(
        root: impl AsRef<Path>,
        profile: FilesystemProfile,
    ) -> Result<Self, NativeStorageError> {
        let timing = LeaseTiming::from_environment()?;
        Self::acquire_timed(root.as_ref(), profile, timing, None)
    }

    /// Try the root again after growing pauses while another owner holds
    /// it, until `wait_until`.
    pub(super) fn acquire_waiting(
        root: &Path,
        profile: FilesystemProfile,
        wait_until: Instant,
    ) -> Result<Self, NativeStorageError> {
        let timing = LeaseTiming::from_environment()?;
        Self::acquire_waiting_timed(root, profile, timing, wait_until)
    }

    pub(super) fn acquire_waiting_timed(
        root: &Path,
        profile: FilesystemProfile,
        timing: LeaseTiming,
        wait_until: Instant,
    ) -> Result<Self, NativeStorageError> {
        let mut backoff = LeaseBackoff::until(wait_until);
        let mut attempt = Self::acquire_timed(root, profile, timing, Some(wait_until));
        while Self::held_by_another(&attempt) && backoff.wait() {
            attempt = Self::acquire_timed(root, profile, timing, Some(wait_until));
        }
        attempt
    }

    fn held_by_another(attempt: &Result<Self, NativeStorageError>) -> bool {
        attempt
            .as_ref()
            .is_err_and(|error| error.code() == NativeStorageErrorCode::RootLocked)
    }

    /// Take the root: the kernel lock first, then the next generation of
    /// the lease record. Without a deadline, a takeover from an owner that
    /// may live on another machine waits the whole takeover time.
    pub(super) fn acquire_timed(
        root: &Path,
        profile: FilesystemProfile,
        timing: LeaseTiming,
        deadline: Option<Instant>,
    ) -> Result<Self, NativeStorageError> {
        Self::refuse_database(root)?;
        // Refused before the root or its lock file is created (ADR 0029).
        let filesystem = RootFilesystem::admit(root, profile)?;
        Self::prepare_root(root, profile)?;
        let canonical = fylo_vfs::canonicalize(root).map_err(NativeStorageError::io)?;
        crate::profile_manifest::ProfileManifest::admit_selection(&canonical, profile)?;
        let lock_path = Self::sibling_path(&canonical, Self::LOCK_SUFFIX)?;
        let record_path = Self::sibling_path(&canonical, Self::RECORD_SUFFIX)?;
        let lock_file = Self::open_lock_file(&lock_path)?;
        let kernel_enforced = Self::lock_root(
            KernelFileLock::try_lock(&lock_file),
            &record_path,
            &canonical,
            profile,
        )?;
        let owner = Self::new_owner_id()?;
        let owned_name = Self::owned_name(&canonical);
        let claims = LeaseClaims::beside(Self::parent(&canonical)?, &owned_name);
        let record = LeaseTakeover {
            record_path: &record_path,
            claims: &claims,
            canonical: &canonical,
            owned_name: &owned_name,
            owner_id: &owner,
            timing,
            deadline,
        }
        .claim()?;
        let generation = record.generation();
        let heartbeat = LeaseHeartbeat::start(record_path.clone(), record, timing.heartbeat());
        Ok(Self {
            root: canonical,
            scope_name: format!("{owner}:{generation}"),
            owner,
            generation,
            lock_file,
            record_path,
            kernel_enforced,
            filesystem,
            heartbeat,
        })
    }

    /// A `.vdbx` database is owned through the lease of its own file
    /// (ADR 0016 section 7); a root lease, and the queues and machine
    /// sessions held under one, need a directory root. A missing root named
    /// `*.vdbx` names a database, never a directory to create.
    fn refuse_database(root: &Path) -> Result<(), NativeStorageError> {
        if crate::NativeRoot::names_database(root) {
            return Err(crate::NativeRoot::database_unsupported(
                "a .vdbx database takes no root lease; queues and machine sessions need a directory root",
            ));
        }
        Ok(())
    }

    fn prepare_root(root: &Path, profile: FilesystemProfile) -> Result<(), NativeStorageError> {
        if profile == FilesystemProfile::Native {
            return fylo_vfs::create_dir_all(root).map_err(NativeStorageError::io);
        }
        Self::require_existing_directory(root)
    }

    fn require_existing_directory(root: &Path) -> Result<(), NativeStorageError> {
        let metadata = fylo_vfs::metadata(root).map_err(NativeStorageError::io)?;
        if metadata.is_dir() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::WrongType,
            "EFS profile requires an existing directory",
        ))
    }

    fn new_owner_id() -> Result<String, NativeStorageError> {
        let mut bytes = [0_u8; Self::OWNER_ID_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::Io,
                format!("no random owner id for the root lease: {error}"),
            )
        })?;
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub(super) fn owner_generation(&self) -> &str {
        &self.scope_name
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    pub(super) fn kernel_enforced(&self) -> bool {
        self.kernel_enforced
    }

    pub(super) fn filesystem(&self) -> &RootFilesystem {
        &self.filesystem
    }

    /// The fencing check (ADR 0033 section 4): the heartbeat has not seen
    /// a takeover, and the record still names this owner and generation.
    pub(super) fn assert_owned(&self) -> Result<(), NativeStorageError> {
        let recorded = LeaseObservation::read(&self.record_path).record();
        let named = recorded.is_some_and(|record| record.names(&self.owner, self.generation));
        let owned = named && !self.heartbeat.lost();
        owned.then_some(()).ok_or_else(Self::ownership_lost)
    }

    pub(super) fn release_checked(mut self) -> Result<(), NativeStorageError> {
        self.require_kernel_lock()?;
        self.heartbeat.stop();
        self.assert_owned()?;
        self.mark_released()?;
        self.unlock_checked()
    }

    /// Give up the kernel lock but keep the lease, as a router reboot does
    /// to an NFS client's locks.
    #[cfg(feature = "test-support")]
    pub(super) fn drop_kernel_lock_for_test(&self) {
        let _ = KernelFileLock::unlock(&self.lock_file);
    }

    fn require_kernel_lock(&self) -> Result<(), NativeStorageError> {
        if self.kernel_enforced {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            "checked root release requires successful kernel advisory lock admission",
        ))
    }

    /// A clean stop marks the record released instead of removing it, so
    /// the generation keeps growing and the next owner claims at once.
    fn mark_released(&self) -> Result<(), NativeStorageError> {
        let recorded = LeaseObservation::read(&self.record_path).record();
        let mine = recorded.filter(|record| record.names(&self.owner, self.generation));
        let released = mine.map(|record| record.released());
        released.map_or(Ok(()), |record| record.persist(&self.record_path))?;
        crate::write::failpoint("lease-after-release")
    }

    fn unlock_checked(&self) -> Result<(), NativeStorageError> {
        KernelFileLock::unlock(&self.lock_file).map_err(NativeStorageError::io)
    }

    fn ownership_lost() -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::RootLeaseLost,
            "exclusive ownership of this FYLO root was lost",
        )
    }

    fn owned_name(canonical: &Path) -> String {
        canonical.file_name().map_or_else(
            || "root".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    fn parent(canonical: &Path) -> Result<&Path, NativeStorageError> {
        canonical.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "FYLO root has no parent directory",
            )
        })
    }

    /// The file `.<root name>.<suffix>` in the root's parent directory, where
    /// the files that coordinate the root's owners live.
    pub(super) fn sibling_path(
        canonical: &Path,
        suffix: &str,
    ) -> Result<PathBuf, NativeStorageError> {
        let parent = Self::parent(canonical)?;
        Ok(parent.join(format!(".{}.{suffix}", Self::owned_name(canonical))))
    }

    fn open_lock_file(path: &Path) -> Result<File, NativeStorageError> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .map_err(NativeStorageError::io)
    }

    pub(super) fn lock_root(
        attempt: Result<(), TryLockError>,
        record_path: &Path,
        canonical: &Path,
        profile: FilesystemProfile,
    ) -> Result<bool, NativeStorageError> {
        match attempt {
            Ok(()) => Ok(true),
            // The lock file is already open read-write, so a refusal here is
            // another holder: SMB (cifs) reports a held byte-range lock as
            // EACCES rather than EWOULDBLOCK.
            Err(TryLockError::WouldBlock) => Err(Self::root_locked(record_path, canonical)),
            Err(TryLockError::Error(error))
                if error.kind() == std::io::ErrorKind::PermissionDenied =>
            {
                Err(Self::root_locked(record_path, canonical))
            }
            Err(TryLockError::Error(error)) if error.kind() == std::io::ErrorKind::Unsupported => {
                if profile == FilesystemProfile::EfsSidecarV1 {
                    Err(NativeStorageError::new(
                        NativeStorageErrorCode::Unsupported,
                        "EFS profile requires supported kernel advisory locking",
                    ))
                } else {
                    Ok(false)
                }
            }
            Err(TryLockError::Error(error)) => Err(NativeStorageError::io(error)),
        }
    }

    fn root_locked(record_path: &Path, canonical: &Path) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::RootLocked,
            LeaseObservation::read(record_path).owner_message(canonical),
        )
    }
}

impl Drop for RootLeaseInner {
    fn drop(&mut self) {
        self.heartbeat.stop();
        let _ = self.mark_released();
        let _ = KernelFileLock::unlock(&self.lock_file);
    }
}
