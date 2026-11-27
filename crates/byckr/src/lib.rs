//! BYCKR in-process: open a root in the host's own process and use its
//! documents, collections and queues through typed, synchronous calls.
//!
//! It wraps the same engine the CLI and the machine protocol use, with no
//! child process and no listener:
//!
//! ```no_run
//! use serde_json::json;
//!
//! let root = byckr::Root::open("/srv/catalog", byckr::OpenOptions::default())?;
//! root.create_collection("products")?;
//! let products = root.collection("products");
//! let id = products.put(json!({ "name": "Ada" }).as_object().unwrap().clone())?;
//! let found = products.find(&byckr::Query::from_json(&json!({ "$ops": [{ "name": { "$eq": "Ada" } }] }))?)?;
//! assert_eq!(found[0].0, id);
//! root.close()?;
//! # Ok::<(), byckr::Error>(())
//! ```
//!
//! A [`Root`] holds the root's exclusive lease from [`Root::open`] until
//! [`Root::close`] or drop, so one process owns the root, exactly as one
//! `fylo exec --loop` session does. It is `Send` and `Sync`: each call opens
//! the engine it needs under that lease, so calls may come from any thread.
//! Every write is durable before its call returns.
//!
//! Scope: directory roots on their main worktree, and `.vdbx` databases,
//! document collections and, in a directory root, the queue. A protected
//! `.vdbx` database (ADR 0021) opens with an application identity and a
//! trusted anchor ([`OpenOptions::with_identity`]).

mod collection;
mod collection_recovery;
mod database_creation;
mod error;
mod page;
mod queue;
pub mod store;

use std::path::{Path, PathBuf};

use collection_recovery::CollectionRecovery;
use byckr_engine::{ReadOnlyEngine, WriteEngine};
use byckr_storage_native::{
    CollectionKind, DatabaseIdentity, FilesystemProfile, NativeQueue, NativeRoot, NativeWriteRoot,
    RootLease, RootOpenOptions,
};

pub use collection::{Collection, Document, Query};
pub use database_creation::DatabaseCreation;
pub use error::Error;
pub use byckr_engine::{CollectionInspection, EngineConfig};
pub use byckr_identity::{
    AndroidKeystore, AndroidKeystoreHost, Identity, IdentityError, IdentityId, KeyReference,
};
pub use byckr_storage_native::{AccessTrust, DatabaseKey, Durability};
pub use byckr_storage_native::{QueueAckResult, QueueDelivery, QueueNackResult, QueuePublishResult};
pub use page::Page;
pub use queue::{ClaimOptions, PublishOptions, Queue};
pub use store::Store;

/// An open database: the Store-shaped name of [`Root`] (ADR 0035).
pub type Database = Root;

/// A failed call: the Store-shaped name of [`Error`] (ADR 0035).
pub type FyloError = Error;

/// How [`Root::open`] opens a root.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
pub struct OpenOptions {
    config: EngineConfig,
    identity: Option<DatabaseIdentity>,
    database_key: Option<DatabaseKey>,
}

impl OpenOptions {
    /// Use these engine settings for every write through the root.
    #[must_use]
    pub const fn with_config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }

    /// Open a protected `.vdbx` database as `identity`, trusting its policy
    /// as `trust` says: [`AccessTrust::Pinned`] with the admin that created
    /// it, or [`AccessTrust::FirstUse`] (ADR 0021). A directory root and a
    /// database that is not protected ignore it; a protected database opened
    /// without one is locked (`ENOIDENTITY`).
    #[must_use]
    pub fn with_identity(mut self, identity: Identity, trust: AccessTrust) -> Self {
        self.identity = Some(DatabaseIdentity::new(identity, trust));
        self
    }

    /// Unlock an encrypted `.vdbx` database with `key`, the secret given as
    /// bytes (ADR 0026 section 9). A directory root and a plain database
    /// ignore it; an encrypted database opened without it, or with another
    /// key, refuses reads and writes with `EKEY`.
    #[must_use]
    pub fn with_database_key(mut self, key: DatabaseKey) -> Self {
        self.database_key = Some(key);
        self
    }
}

/// One open BYCKR root, owned by this process until it is closed or dropped.
#[derive(Debug)]
pub struct Root {
    path: PathBuf,
    /// A directory root's exclusive lease; a `.vdbx` database coordinates
    /// its writers through its own file (ADR 0016 section 7) and has none.
    lease: Option<RootLease>,
    config: EngineConfig,
    identity: Option<DatabaseIdentity>,
    database_key: Option<DatabaseKey>,
}

impl Root {
    /// Open a directory root: take its exclusive lease, then finish any
    /// interrupted commit before the first call.
    ///
    /// # Errors
    ///
    /// Fails when another handle owns the root, when the path is unsafe or
    /// names a `.vdbx` database, when a version control branch other than
    /// `main` is checked out, or when recovery fails. A path through a
    /// symlink opens and leases its target, as the machine protocol does;
    /// hosts that refuse symlinked roots canonicalize first.
    pub fn open(path: impl AsRef<Path>, options: OpenOptions) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let lease = Self::lease_for(&path)?;
        let root = Self {
            path,
            lease,
            config: options.config,
            identity: options.identity,
            database_key: options.database_key,
        };
        root.check_lease()?;
        root.refuse_branch_worktree()?;
        root.recover()?;
        Ok(root)
    }

    /// The root path as it was opened.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Confirm this process still owns the root's lease and that the path
    /// still resolves to the leased directory.
    ///
    /// # Errors
    ///
    /// `EROOTLEASELOST` when ownership was lost or the path moved.
    pub fn check_lease(&self) -> Result<(), Error> {
        let Some(lease) = &self.lease else {
            return Ok(());
        };
        lease.assert_owned()?;
        let canonical = byckr_vfs::canonicalize(&self.path).map_err(|_| {
            Error::new("EROOTLEASELOST", "cannot resolve the owning root directory")
        })?;
        if canonical != lease.root() {
            return Err(Error::new(
                "EROOTLEASELOST",
                "the root path no longer names the leased directory",
            ));
        }
        Ok(())
    }

    /// Release the root's lease, checking that the release succeeded.
    ///
    /// # Errors
    ///
    /// Fails when ownership was already lost or the lease metadata could not
    /// be removed.
    pub fn close(self) -> Result<(), Error> {
        match self.lease {
            Some(lease) => lease.release_checked().map_err(Error::from),
            None => NativeRoot::close_database(&self.path).map_err(Error::from),
        }
    }

    /// A handle on one document collection. The name is checked by each call.
    #[must_use]
    pub fn collection<'root>(&'root self, name: &'root str) -> Collection<'root> {
        Collection::new(self, name)
    }

    /// A handle on one queue topic. The name is checked by each call.
    #[must_use]
    pub fn queue<'root>(&'root self, topic: &'root str) -> Queue<'root> {
        Queue::new(self, topic)
    }

    /// The collection `name` as a Store of entities `T` (`api/store/v1`).
    #[must_use]
    pub fn store<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        name: &str,
    ) -> Store<'_, T> {
        Store::new(self, name)
    }

    /// The root's collections: exists, create, drop and inspect.
    #[must_use]
    pub const fn collections(&self) -> store::Collections<'_> {
        store::Collections::new(self)
    }

    /// Create a document collection, or finish one whose creation was
    /// interrupted. Returns `true` when the collection did not exist before.
    ///
    /// # Errors
    ///
    /// Fails for an invalid name, an existing file collection of that name,
    /// or a failed durable write.
    pub fn create_collection(&self, name: &str) -> Result<bool, Error> {
        self.writer()?
            .create_collection_with_layout(name, CollectionKind::Document, None, None)
            .map_err(Error::from)
    }

    /// Drop a collection and its documents.
    ///
    /// # Errors
    ///
    /// `ECOLLECTION_NOT_FOUND` for a missing collection, or a failed write.
    pub fn drop_collection(&self, name: &str) -> Result<(), Error> {
        self.writer()?.drop_collection(name).map_err(Error::from)
    }

    /// Describe a collection from its stable generation.
    ///
    /// # Errors
    ///
    /// `ECOLLECTION_NOT_FOUND` for a missing collection, or a read failure.
    pub fn inspect_collection(&self, name: &str) -> Result<CollectionInspection, Error> {
        self.reader()?.inspect(name).map_err(Error::from)
    }

    /// Finish what a stopped writer left: the repository materialization,
    /// then every collection's interrupted commit, as `fylo exec` does
    /// before its first request.
    fn recover(&self) -> Result<(), Error> {
        let writer = self.writer()?;
        writer.recover_repository_materialization()?;
        let root = NativeRoot::open_profiled(self.options())?;
        CollectionRecovery::run(&root, &writer)
    }

    /// A directory root's lease; none for a `.vdbx` database.
    fn lease_for(path: &Path) -> Result<Option<RootLease>, Error> {
        if NativeRoot::names_database(path) {
            return Ok(None);
        }
        Ok(Some(RootLease::acquire_with_profile(
            path,
            FilesystemProfile::Native,
        )?))
    }

    fn options(&self) -> RootOpenOptions {
        let options = RootOpenOptions::new(&self.path).with_profile(FilesystemProfile::Native);
        let options = self.add_lease(options);
        let options = self.add_identity(options);
        self.add_database_key(options)
    }

    fn add_lease(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.lease {
            Some(lease) => options.with_lease(lease.clone()),
            None => options,
        }
    }

    fn add_identity(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.identity {
            Some(identity) => options.with_identity(identity.clone()),
            None => options,
        }
    }

    fn add_database_key(&self, options: RootOpenOptions) -> RootOpenOptions {
        match &self.database_key {
            Some(key) => options.with_database_key(key.clone()),
            None => options,
        }
    }

    fn assert_owned(&self) -> Result<(), Error> {
        match &self.lease {
            Some(lease) => lease.assert_owned().map_err(Error::from),
            None => Ok(()),
        }
    }

    fn writer(&self) -> Result<NativeWriteRoot, Error> {
        self.assert_owned()?;
        Ok(NativeWriteRoot::open_profiled(self.options())?.with_config(self.config))
    }

    fn write_engine(&self) -> Result<WriteEngine, Error> {
        self.assert_owned()?;
        Ok(WriteEngine::open_profiled(self.options())?.with_config(self.config))
    }

    fn reader(&self) -> Result<ReadOnlyEngine, Error> {
        self.assert_owned()?;
        ReadOnlyEngine::open_profiled(self.options()).map_err(Error::from)
    }

    /// The queue lives in a directory root; a `.vdbx` database holds none
    /// (ADR 0016 version 1, and ADR 0021 Q11 for a protected one).
    fn native_queue(&self) -> Result<NativeQueue, Error> {
        let lease = self.lease.clone().ok_or_else(|| {
            Error::new(
                "ELAYOUTUNSUPPORTED",
                "a .vdbx database holds no queue; the queue lives in a directory root",
            )
        })?;
        NativeQueue::open_with_lease(lease).map_err(Error::from)
    }

    /// The machine protocol resolves a checked-out branch to its
    /// materialized worktree. This release serves the main worktree only, so
    /// a root on another branch is refused rather than read from the wrong
    /// tree.
    fn refuse_branch_worktree(&self) -> Result<(), Error> {
        if self.lease.is_none() {
            return Ok(());
        }
        let head = self.path.join(".fylo-vcs").join("HEAD");
        let Ok(text) = byckr_vfs::read_to_string(&head) else {
            return Ok(());
        };
        if text.trim() == "ref: refs/heads/main" {
            return Ok(());
        }
        Err(Error::new(
            "ELAYOUTUNSUPPORTED",
            "the fylo crate opens a root's main worktree only in this release",
        ))
    }
}
