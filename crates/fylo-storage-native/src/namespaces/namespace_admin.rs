use super::namespaces_lock::NamespacesLock;
use crate::layout::{AppFolder, GuardFiles, NamespaceMarker, NamespaceName, RootLayout};
use crate::{
    FilesystemProbe, FilesystemProfile, LeaseTiming, NativeStorageError, NativeStorageErrorCode,
    RootLease,
};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Create, list and drop the namespaces of one app folder (ADR 0033
/// section 3). A namespace is a folder `<app>/<namespace>` with its own
/// `.fylo` folder, the guard files for old binaries, and the marker
/// `.fylo/namespace.json`, written last: a folder without its marker is a
/// create that a crash stopped, which the next create finishes.
///
/// A drop takes the namespace's lease, so it fails with `EROOTLOCKED` while
/// another process uses the namespace. It moves the folder into
/// `<app>/.fylo/trash`, gives the lease back, then deletes it; a crash
/// leaves at most a folder in the trash, which the next create, drop, list
/// or open removes.
pub struct NamespaceAdmin;

impl NamespaceAdmin {
    /// Create namespace `namespace` in the app folder `layout` names,
    /// making the app folder first when it does not exist yet. `layout` must
    /// name the canonical app folder; `created_by` is the FYLO version a new
    /// app folder records, and `forced` the metadata profile its probe must
    /// choose, if any (slice L1-4).
    ///
    /// # Errors
    ///
    /// `ENAMESPACEEXISTS` when the namespace exists, the errors of
    /// `AppFolder::prepare` (`ENOTFYLOROOT`, `EFSPROBE`), and `ENATIVE_IO`.
    pub fn create_namespace(
        layout: &RootLayout,
        namespace: &NamespaceName,
        created_by: &str,
        forced: Option<FilesystemProfile>,
    ) -> Result<PathBuf, NativeStorageError> {
        AppFolder::prepare(layout, created_by, |folder| {
            FilesystemProbe::run(folder, forced)
        })?;
        let _lock = NamespacesLock::take(layout)?;
        Self::sweep_trash(layout);
        Self::refuse_existing(layout, namespace)?;
        Self::make_namespace(layout, namespace)?;
        Ok(layout.namespace_folder(namespace))
    }

    /// The namespaces of the app folder `layout` names, sorted by name. A
    /// folder counts only when its marker names it.
    ///
    /// # Errors
    ///
    /// `ENATIVE_IO` when the app folder cannot be read.
    pub fn list_namespaces(layout: &RootLayout) -> Result<Vec<NamespaceName>, NativeStorageError> {
        Self::sweep_trash(layout);
        let entries = fylo_vfs::read_dir(layout.app_folder()).map_err(NativeStorageError::io)?;
        let mut names = entries
            .filter_map(Result::ok)
            .filter_map(|entry| Self::namespace_named(layout, &entry.file_name().to_string_lossy()))
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    /// Drop namespace `namespace`. Without `cascade` only a namespace with
    /// no collections can be dropped.
    ///
    /// # Errors
    ///
    /// `ENATIVE_NOT_FOUND` when the namespace does not exist, `EROOTLOCKED`
    /// while another process owns it, `ENAMESPACENOTEMPTY` when it holds
    /// collections and `cascade` is false, and `ENATIVE_IO`.
    pub fn drop_namespace(
        layout: &RootLayout,
        namespace: &NamespaceName,
        cascade: bool,
        (profile, timing): (FilesystemProfile, LeaseTiming),
    ) -> Result<(), NativeStorageError> {
        Self::require_existing(layout, namespace)?;
        let lease = RootLease::acquire_namespace(layout, namespace, profile, timing)?;
        Self::refuse_collections_unless(cascade, layout, namespace)?;
        let trashed = Self::move_to_trash(layout, namespace)?;
        drop(lease);
        Self::remove_trashed(&trashed);
        Ok(())
    }

    /// Delete every folder a drop left in `<app>/.fylo/trash`. Returns how
    /// many were removed; one that cannot be removed now stays for the next
    /// sweep.
    pub fn sweep_trash(layout: &RootLayout) -> usize {
        let Ok(entries) = fylo_vfs::read_dir(layout.trash_folder()) else {
            return 0;
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| fylo_vfs::remove_dir_all(entry.path()).is_ok())
            .count()
    }

    fn namespace_named(layout: &RootLayout, name: &str) -> Option<NamespaceName> {
        let namespace = NamespaceName::parse(name).ok()?;
        let marker = NamespaceMarker::read(&layout.namespace_marker_path(&namespace)).ok()??;
        (marker.name() == &namespace).then_some(namespace)
    }

    fn refuse_existing(
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<(), NativeStorageError> {
        if Self::namespace_named(layout, namespace.as_str()).is_none() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::NamespaceExists,
            format!(
                "namespace {namespace} already exists in {}",
                layout.app_folder().display()
            ),
        ))
    }

    fn require_existing(
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<(), NativeStorageError> {
        if Self::namespace_named(layout, namespace.as_str()).is_some() {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::NotFound,
            format!(
                "namespace {namespace} does not exist in {}",
                layout.app_folder().display()
            ),
        ))
    }

    fn make_namespace(
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<(), NativeStorageError> {
        let folder = layout.namespace_folder(namespace);
        Self::make_folder(&folder)?;
        Self::make_folder(&layout.namespace_state_folder(namespace))?;
        GuardFiles::write(&folder)?;
        NamespaceMarker::new(namespace.clone()).write(&layout.namespace_marker_path(namespace))?;
        crate::write::sync_directory(layout.app_folder())
    }

    fn make_folder(path: &Path) -> Result<(), NativeStorageError> {
        match fylo_vfs::create_dir(path) {
            Err(error) if error.kind() != ErrorKind::AlreadyExists => {
                Err(NativeStorageError::io(error))
            }
            _ => Ok(()),
        }
    }

    fn refuse_collections_unless(
        cascade: bool,
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<(), NativeStorageError> {
        let held = Self::collection_count(&layout.namespace_folder(namespace))?;
        if cascade || held == 0 {
            return Ok(());
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::NamespaceNotEmpty,
            format!(
                "namespace {namespace} holds {held} collection(s); drop it with cascade to delete them"
            ),
        ))
    }

    /// Entries of the namespace folder other than its `.fylo` folder and
    /// the guard files.
    fn collection_count(folder: &Path) -> Result<usize, NativeStorageError> {
        let entries = fylo_vfs::read_dir(folder).map_err(NativeStorageError::io)?;
        Ok(entries
            .filter_map(Result::ok)
            .filter(|entry| Self::is_collection_entry(&entry.file_name().to_string_lossy()))
            .count())
    }

    fn is_collection_entry(name: &str) -> bool {
        name != RootLayout::STATE_FOLDER && !GuardFiles::is_guard_name(name)
    }

    fn move_to_trash(
        layout: &RootLayout,
        namespace: &NamespaceName,
    ) -> Result<PathBuf, NativeStorageError> {
        let _lock = NamespacesLock::take(layout)?;
        fylo_vfs::create_dir_all(layout.trash_folder()).map_err(NativeStorageError::io)?;
        let trashed = layout
            .trash_folder()
            .join(crate::write::unique_name(namespace.as_str()));
        fylo_vfs::rename(layout.namespace_folder(namespace), &trashed)
            .map_err(NativeStorageError::io)?;
        crate::write::sync_directory(layout.app_folder())?;
        Ok(trashed)
    }

    fn remove_trashed(trashed: &Path) {
        let _ = fylo_vfs::remove_dir_all(trashed);
    }
}
