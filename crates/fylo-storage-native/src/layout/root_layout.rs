use std::path::{Path, PathBuf};

use super::{AppName, CollectionName, NamespaceName, RecordAddress};
use crate::NativeStorageError;

/// Every path of one app folder in the layout of ADR 0033:
/// `<drive>/<app>/<namespace>/<collection>/<id>.<extension>`, with the
/// app's and each namespace's own files beside them.
///
/// The app folder is the root: its last folder name is the app name. Pass
/// the canonical root path; this type only builds paths and touches no file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootLayout {
    app_folder: PathBuf,
    app_name: Option<AppName>,
}

impl RootLayout {
    /// The hidden folder of FYLO's own files, in an app folder, a namespace
    /// folder and a collection folder.
    pub const STATE_FOLDER: &'static str = ".fylo";
    const FORMAT_FILE: &'static str = "format.json";
    const NAMESPACES_LOCK_FILE: &'static str = "namespaces.lock";
    const TRASH_FOLDER: &'static str = "trash";
    const NAMESPACE_MARKER_FILE: &'static str = "namespace.json";
    const LEASE_LOCK_SUFFIX: &'static str = "fylo-root-owner.lock";
    const LEASE_RECORD_SUFFIX: &'static str = "fylo-root-owner.lock.json";
    const LEASE_CLAIM_SUFFIX: &'static str = "fylo-root-owner.claim";
    const TURN_LOCK_SUFFIX: &'static str = "fylo-root-turn.lock";
    const VDBX_EXTENSION: &'static str = "vdbx";

    /// The layout of a root whose folder name must be a valid app name.
    ///
    /// # Errors
    ///
    /// Returns `EAPPNAME` when the folder name breaks the name rule.
    pub fn checked(app_folder: PathBuf) -> Result<Self, NativeStorageError> {
        let app_name = AppName::from_root_folder(&app_folder)?;
        Ok(Self {
            app_folder,
            app_name: Some(app_name),
        })
    }

    /// The layout of a root admitted by the system-disk opt-out (ADR 0033
    /// section 5.3): a folder name that breaks the rule is accepted, and the
    /// app then has no name.
    #[must_use]
    pub fn waived(app_folder: PathBuf) -> Self {
        let app_name = AppName::from_root_folder(&app_folder).ok();
        Self {
            app_folder,
            app_name,
        }
    }

    /// The layout of an app's own OS data folder (app mode, ADR 0032 section
    /// 3.2): the folder plays the part of `<drive>/<app>` and has no app name.
    #[must_use]
    pub fn app_owned(app_folder: PathBuf) -> Self {
        Self {
            app_folder,
            app_name: None,
        }
    }

    /// The app folder: the root.
    #[must_use]
    pub fn app_folder(&self) -> &Path {
        &self.app_folder
    }

    /// The app name, or `None` when the opt-out or app mode admitted a folder
    /// without a valid name (the handshake's `storage.appName: null`).
    #[must_use]
    pub fn app_name(&self) -> Option<&AppName> {
        self.app_name.as_ref()
    }

    /// The folder that holds the app folder: the drive's mount point when the
    /// drive rule admitted the root.
    #[must_use]
    pub fn drive(&self) -> Option<&Path> {
        self.app_folder.parent()
    }

    /// `<app>/.fylo`: app-wide state.
    #[must_use]
    pub fn app_state_folder(&self) -> PathBuf {
        self.app_folder.join(Self::STATE_FOLDER)
    }

    /// `<app>/.fylo/format.json`: the layout marker.
    #[must_use]
    pub fn format_marker_path(&self) -> PathBuf {
        self.app_state_folder().join(Self::FORMAT_FILE)
    }

    /// `<app>/.fylo/namespaces.lock`: held only while a namespace is created
    /// or dropped.
    #[must_use]
    pub fn namespaces_lock_path(&self) -> PathBuf {
        self.app_state_folder().join(Self::NAMESPACES_LOCK_FILE)
    }

    /// `<app>/.fylo/trash`: dropped namespaces, deleted after the drop.
    #[must_use]
    pub fn trash_folder(&self) -> PathBuf {
        self.app_state_folder().join(Self::TRASH_FOLDER)
    }

    /// `<app>/<namespace>`.
    #[must_use]
    pub fn namespace_folder(&self, namespace: &NamespaceName) -> PathBuf {
        self.app_folder.join(namespace.as_str())
    }

    /// `<app>/<namespace>/.fylo`: the namespace's catalog, journal, schema
    /// registry, queue and version control.
    #[must_use]
    pub fn namespace_state_folder(&self, namespace: &NamespaceName) -> PathBuf {
        self.namespace_folder(namespace).join(Self::STATE_FOLDER)
    }

    /// `<app>/<namespace>/.fylo/namespace.json`: the namespace marker.
    #[must_use]
    pub fn namespace_marker_path(&self, namespace: &NamespaceName) -> PathBuf {
        self.namespace_state_folder(namespace)
            .join(Self::NAMESPACE_MARKER_FILE)
    }

    /// `<app>/<namespace>.vdbx`: a namespace kept as one file.
    #[must_use]
    pub fn vdbx_path(&self, namespace: &NamespaceName) -> PathBuf {
        self.app_folder
            .join(format!("{namespace}.{}", Self::VDBX_EXTENSION))
    }

    /// `<app>/.<namespace>.fylo-root-owner.lock`: the namespace's kernel lock
    /// file.
    #[must_use]
    pub fn lease_lock_path(&self, namespace: &NamespaceName) -> PathBuf {
        self.hidden_namespace_file(namespace, Self::LEASE_LOCK_SUFFIX)
    }

    /// `<app>/.<namespace>.fylo-root-owner.lock.json`: the namespace's lease
    /// record.
    #[must_use]
    pub fn lease_record_path(&self, namespace: &NamespaceName) -> PathBuf {
        self.hidden_namespace_file(namespace, Self::LEASE_RECORD_SUFFIX)
    }

    /// `<app>/.<namespace>.fylo-root-owner.claim.<generation>`: the short
    /// claim of a lease generation.
    #[must_use]
    pub fn lease_claim_path(&self, namespace: &NamespaceName, generation: u64) -> PathBuf {
        let suffix = format!("{}.{generation}", Self::LEASE_CLAIM_SUFFIX);
        self.hidden_namespace_file(namespace, &suffix)
    }

    /// `<app>/.<namespace>.fylo-root-turn.lock`: the namespace's fair-wait
    /// turn lock.
    #[must_use]
    pub fn turn_lock_path(&self, namespace: &NamespaceName) -> PathBuf {
        self.hidden_namespace_file(namespace, Self::TURN_LOCK_SUFFIX)
    }

    /// `<app>/<namespace>/<collection>`.
    #[must_use]
    pub fn collection_folder(
        &self,
        namespace: &NamespaceName,
        collection: &CollectionName,
    ) -> PathBuf {
        self.namespace_folder(namespace).join(collection.as_str())
    }

    /// `<app>/<namespace>/<collection>/.fylo`: the collection's derived
    /// files.
    #[must_use]
    pub fn collection_state_folder(
        &self,
        namespace: &NamespaceName,
        collection: &CollectionName,
    ) -> PathBuf {
        self.collection_folder(namespace, collection)
            .join(Self::STATE_FOLDER)
    }

    /// The record's file: `<collection>/<id>.<extension>` while the
    /// collection is flat (`shard_width` 0), else
    /// `<collection>/<shard>/<id>.<extension>`.
    #[must_use]
    pub fn record_path(&self, address: &RecordAddress, shard_width: u32) -> PathBuf {
        let mut path = self.collection_folder(address.namespace(), address.collection());
        path.extend(Self::shard_folder(address.id(), shard_width));
        path.join(address.file_name())
    }

    fn hidden_namespace_file(&self, namespace: &NamespaceName, suffix: &str) -> PathBuf {
        self.app_folder.join(format!(".{namespace}.{suffix}"))
    }

    fn shard_folder(id: &str, shard_width: u32) -> Option<String> {
        (shard_width > 0).then(|| crate::shard_of(id, shard_width))
    }
}

#[cfg(test)]
mod tests {
    use super::RootLayout;
    use crate::layout::{CollectionName, NamespaceName, RecordAddress, RecordKind};
    use std::path::{Path, PathBuf};

    const ID: &str = "4UUB32VGUDW";

    fn layout() -> RootLayout {
        RootLayout::checked(PathBuf::from("/mnt/data/chat")).unwrap()
    }

    fn billing() -> NamespaceName {
        NamespaceName::parse("billing").unwrap()
    }

    fn invoices() -> CollectionName {
        CollectionName::parse("invoices").unwrap()
    }

    fn document() -> RecordAddress {
        RecordAddress::new(billing(), invoices(), ID, RecordKind::Document).unwrap()
    }

    #[test]
    fn the_app_name_and_drive_come_from_the_root() {
        let layout = layout();
        assert_eq!(layout.app_name().unwrap().as_str(), "chat");
        assert_eq!(layout.app_folder(), Path::new("/mnt/data/chat"));
        assert_eq!(layout.drive(), Some(Path::new("/mnt/data")));
    }

    #[test]
    fn a_checked_layout_refuses_a_bad_folder_name() {
        let error = RootLayout::checked(PathBuf::from("/tmp/.tmpAbC123")).unwrap_err();
        assert_eq!(error.code().as_str(), "EAPPNAME");
    }

    #[test]
    fn the_opt_out_admits_a_bad_folder_name_without_an_app_name() {
        let layout = RootLayout::waived(PathBuf::from("/tmp/.tmpAbC123"));
        assert_eq!(layout.app_name(), None);
        assert_eq!(layout.app_folder(), Path::new("/tmp/.tmpAbC123"));
    }

    #[test]
    fn the_opt_out_keeps_a_valid_app_name() {
        let layout = RootLayout::waived(PathBuf::from("/tmp/chat"));
        assert_eq!(layout.app_name().unwrap().as_str(), "chat");
    }

    #[test]
    fn app_mode_has_no_app_name() {
        let layout = RootLayout::app_owned(PathBuf::from("/data/com.example.App"));
        assert_eq!(layout.app_name(), None);
    }

    #[test]
    fn app_wide_files_are_in_the_hidden_state_folder() {
        let layout = layout();
        assert_eq!(layout.app_state_folder(), Path::new("/mnt/data/chat/.fylo"));
        assert_eq!(
            layout.format_marker_path(),
            Path::new("/mnt/data/chat/.fylo/format.json")
        );
        assert_eq!(
            layout.namespaces_lock_path(),
            Path::new("/mnt/data/chat/.fylo/namespaces.lock")
        );
        assert_eq!(
            layout.trash_folder(),
            Path::new("/mnt/data/chat/.fylo/trash")
        );
    }

    #[test]
    fn namespace_paths_follow_the_adr() {
        let layout = layout();
        let billing = billing();
        assert_eq!(
            layout.namespace_folder(&billing),
            Path::new("/mnt/data/chat/billing")
        );
        assert_eq!(
            layout.namespace_state_folder(&billing),
            Path::new("/mnt/data/chat/billing/.fylo")
        );
        assert_eq!(
            layout.namespace_marker_path(&billing),
            Path::new("/mnt/data/chat/billing/.fylo/namespace.json")
        );
        assert_eq!(
            layout.vdbx_path(&billing),
            Path::new("/mnt/data/chat/billing.vdbx")
        );
    }

    #[test]
    fn lease_files_sit_beside_the_namespace_folder() {
        let layout = layout();
        let billing = billing();
        assert_eq!(
            layout.lease_lock_path(&billing),
            Path::new("/mnt/data/chat/.billing.fylo-root-owner.lock")
        );
        assert_eq!(
            layout.lease_record_path(&billing),
            Path::new("/mnt/data/chat/.billing.fylo-root-owner.lock.json")
        );
        assert_eq!(
            layout.lease_claim_path(&billing, 7),
            Path::new("/mnt/data/chat/.billing.fylo-root-owner.claim.7")
        );
        assert_eq!(
            layout.turn_lock_path(&billing),
            Path::new("/mnt/data/chat/.billing.fylo-root-turn.lock")
        );
    }

    #[test]
    fn collection_paths_follow_the_adr() {
        let layout = layout();
        assert_eq!(
            layout.collection_folder(&billing(), &invoices()),
            Path::new("/mnt/data/chat/billing/invoices")
        );
        assert_eq!(
            layout.collection_state_folder(&billing(), &invoices()),
            Path::new("/mnt/data/chat/billing/invoices/.fylo")
        );
    }

    #[test]
    fn a_flat_collection_keeps_records_in_its_folder() {
        assert_eq!(
            layout().record_path(&document(), 0),
            Path::new("/mnt/data/chat/billing/invoices/4UUB32VGUDW.json")
        );
    }

    #[test]
    fn a_sharded_collection_adds_the_shard_folder() {
        let shard = crate::shard_of(ID, 1);
        let expected = PathBuf::from("/mnt/data/chat/billing/invoices")
            .join(&shard)
            .join("4UUB32VGUDW.json");
        assert_eq!(layout().record_path(&document(), 1), expected);
        assert_eq!(shard.len(), 1);
    }

    #[test]
    fn a_file_sits_beside_the_documents() {
        let file = RecordAddress::new(billing(), invoices(), ID, RecordKind::file("png").unwrap())
            .unwrap();
        assert_eq!(
            layout().record_path(&file, 0),
            Path::new("/mnt/data/chat/billing/invoices/4UUB32VGUDW.png")
        );
    }

    #[test]
    fn the_default_namespace_is_a_plain_folder() {
        assert_eq!(
            layout().namespace_folder(&NamespaceName::public()),
            Path::new("/mnt/data/chat/public")
        );
    }
}
