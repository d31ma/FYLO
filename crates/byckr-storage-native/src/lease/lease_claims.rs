use crate::{NativeStorageError, NativeStorageErrorCode};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The claim files `.<name>.fylo-root-owner.claim.<generation>` that decide
/// which process takes a generation (ADR 0033 section 4, step 4).
///
/// A claim is made with exclusive create, so only one process can make the
/// file for a generation, on any machine that shares the folder. The winner
/// keeps its own claim file and removes older ones, so the newest
/// generation can be found even when the record itself is unreadable.
pub(super) struct LeaseClaims {
    folder: PathBuf,
    prefix: String,
}

impl LeaseClaims {
    const SUFFIX: &str = "fylo-root-owner.claim.";

    pub(super) fn beside(folder: &Path, owned_name: &str) -> Self {
        Self {
            folder: folder.to_path_buf(),
            prefix: format!(".{owned_name}.{}", Self::SUFFIX),
        }
    }

    /// Claim files in `folder` whose names start with `prefix` (a
    /// namespace's, from `RootLayout::lease_claim_prefix`).
    pub(super) fn with_prefix(folder: &Path, prefix: String) -> Self {
        Self {
            folder: folder.to_path_buf(),
            prefix,
        }
    }

    /// The generations with a claim file now.
    pub(super) fn present(&self) -> BTreeSet<u64> {
        let Ok(entries) = byckr_vfs::read_dir(&self.folder) else {
            return BTreeSet::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| self.generation_of(&entry.file_name().to_string_lossy()))
            .collect()
    }

    /// The generation the next claim takes: one past `base`, skipping every
    /// generation whose claim file was already there when the waiting
    /// started. Such a claim never became the record during the whole
    /// takeover time, so its process is gone.
    pub(super) fn next_after(base: u64, stale: &BTreeSet<u64>) -> u64 {
        let mut generation = base.saturating_add(1);
        while stale.contains(&generation) {
            generation = generation.saturating_add(1);
        }
        generation
    }

    /// The last generation any claim file records, for an unreadable
    /// record.
    pub(super) fn newest(present: &BTreeSet<u64>) -> u64 {
        present.last().copied().unwrap_or(0)
    }

    /// Make the claim file for `generation`. `RootLocked` when another
    /// process made it first.
    pub(super) fn claim(&self, generation: u64) -> Result<(), NativeStorageError> {
        let created = byckr_vfs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(self.path(generation));
        created.map(drop).map_err(Self::claim_refused)
    }

    /// Remove the claim files of every generation before `generation`.
    pub(super) fn remove_before(&self, generation: u64) {
        for older in self.present().range(..generation) {
            let _ = byckr_vfs::remove_file(self.path(*older));
        }
    }

    fn generation_of(&self, file_name: &str) -> Option<u64> {
        file_name.strip_prefix(&self.prefix)?.parse::<u64>().ok()
    }

    fn path(&self, generation: u64) -> PathBuf {
        self.folder.join(format!("{}{generation}", self.prefix))
    }

    fn claim_refused(error: std::io::Error) -> NativeStorageError {
        if error.kind() != std::io::ErrorKind::AlreadyExists {
            return NativeStorageError::io(error);
        }
        NativeStorageError::new(
            NativeStorageErrorCode::RootLocked,
            "another process claimed this BYCKR root first",
        )
    }
}
