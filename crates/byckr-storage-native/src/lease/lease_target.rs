use super::lease_claims::LeaseClaims;
use crate::layout::{NamespaceName, RootLayout};
use crate::{NativeStorageError, NativeStorageErrorCode};
use std::path::{Path, PathBuf};

/// What one lease owns, and the files that coordinate its owners.
///
/// A folder root (the layout before ADR 0033) keeps its files beside it:
/// `.<root>.fylo-root-owner.lock(.json)` in its parent folder. A namespace
/// of an app folder keeps them in the app folder, named after the namespace:
/// `<app>/.<namespace>.fylo-root-owner.lock(.json)` (ADR 0033 section 4),
/// as `RootLayout` names them.
pub(super) struct LeaseTarget {
    pub(super) owned: PathBuf,
    pub(super) owned_name: String,
    pub(super) lock_path: PathBuf,
    pub(super) record_path: PathBuf,
    pub(super) claims: LeaseClaims,
}

impl LeaseTarget {
    const LOCK_SUFFIX: &str = "fylo-root-owner.lock";
    const RECORD_SUFFIX: &str = "fylo-root-owner.lock.json";
    const TURN_SUFFIX: &str = "fylo-root-turn.lock";

    /// The lease of a folder root, given its canonical path.
    pub(super) fn root(canonical: &Path) -> Result<Self, NativeStorageError> {
        let owned_name = Self::folder_name(canonical);
        let parent = Self::parent(canonical)?;
        Ok(Self {
            owned: canonical.to_path_buf(),
            lock_path: parent.join(format!(".{owned_name}.{}", Self::LOCK_SUFFIX)),
            record_path: parent.join(format!(".{owned_name}.{}", Self::RECORD_SUFFIX)),
            claims: LeaseClaims::beside(parent, &owned_name),
            owned_name,
        })
    }

    /// The lease of namespace `namespace` of the app folder `layout` names.
    /// The layout's app folder must be canonical.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` when the namespace folder does not exist.
    pub(super) fn namespace(
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<Self, NativeStorageError> {
        let owned = layout.namespace_folder(namespace);
        Self::require_namespace_folder(&owned, namespace)?;
        Ok(Self {
            owned,
            owned_name: namespace.as_str().to_owned(),
            lock_path: layout.lease_lock_path(namespace),
            record_path: layout.lease_record_path(namespace),
            claims: LeaseClaims::with_prefix(
                layout.app_folder(),
                RootLayout::lease_claim_prefix(namespace),
            ),
        })
    }

    /// The fair-wait turn file of a folder root (see `RootTurn`).
    pub(super) fn root_turn_path(canonical: &Path) -> Result<PathBuf, NativeStorageError> {
        let name = Self::folder_name(canonical);
        Ok(Self::parent(canonical)?.join(format!(".{name}.{}", Self::TURN_SUFFIX)))
    }

    fn require_namespace_folder(
        folder: &Path,
        namespace: &NamespaceName,
    ) -> Result<(), NativeStorageError> {
        let present = byckr_vfs::symlink_metadata(folder).is_ok_and(|metadata| metadata.is_dir());
        if present {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::NotFound,
            format!("namespace {namespace} does not exist: {}", folder.display()),
        ))
    }

    fn folder_name(canonical: &Path) -> String {
        canonical.file_name().map_or_else(
            || "root".to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    }

    fn parent(canonical: &Path) -> Result<&Path, NativeStorageError> {
        canonical.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "BYCKR root has no parent directory",
            )
        })
    }
}
