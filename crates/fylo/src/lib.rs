//! FYLO in-process: open a root in the host's own process and use its
//! documents, collections and queues through typed, synchronous calls.
//!
//! This is layer 1 of the Turso SDK design (ADR 0019, proposed). It wraps the
//! same engine the CLI and the machine protocol use, with no child process
//! and no listener:
//!
//! ```no_run
//! use serde_json::json;
//!
//! let root = fylo::Root::open("/srv/catalog", fylo::OpenOptions::default())?;
//! root.create_collection("products")?;
//! let products = root.collection("products");
//! let id = products.put(json!({ "name": "Ada" }).as_object().unwrap().clone())?;
//! let found = products.find(&fylo::Query::from_json(&json!({ "$ops": [{ "name": { "$eq": "Ada" } }] }))?)?;
//! assert_eq!(found[0].0, id);
//! root.close()?;
//! # Ok::<(), fylo::Error>(())
//! ```
//!
//! A [`Root`] holds the root's exclusive lease from [`Root::open`] until
//! [`Root::close`] or drop, so one process owns the root, exactly as one
//! `fylo exec --loop` session does. It is `Send` and `Sync`: each call opens
//! the engine it needs under that lease, so calls may come from any thread.
//! Every write is durable before its call returns.
//!
//! Phase 1 scope: directory roots on their main worktree, document
//! collections, and the queue. There is no SQL yet.

mod collection;
mod error;
mod queue;

use std::path::{Path, PathBuf};

use fylo_engine::{ReadOnlyEngine, WriteEngine};
use fylo_storage_native::{
    CollectionKind, FilesystemProfile, NativeQueue, NativeRoot, NativeWriteRoot, RootLease,
    RootOpenOptions,
};

pub use collection::{Collection, Document, Query};
pub use error::Error;
pub use fylo_engine::{CollectionInspection, EngineConfig};
pub use fylo_storage_native::{QueueAckResult, QueueDelivery, QueueNackResult, QueuePublishResult};
pub use queue::{ClaimOptions, PublishOptions, Queue};

/// How [`Root::open`] opens a root.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct OpenOptions {
    config: EngineConfig,
}

impl OpenOptions {
    /// Use these engine settings for every write through the root.
    #[must_use]
    pub const fn with_config(mut self, config: EngineConfig) -> Self {
        self.config = config;
        self
    }
}

/// One open FYLO root, owned by this process until it is closed or dropped.
#[derive(Debug)]
pub struct Root {
    path: PathBuf,
    lease: RootLease,
    config: EngineConfig,
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
        if NativeRoot::names_database(&path) {
            return Err(Error::new(
                "ELAYOUTUNSUPPORTED",
                "the fylo crate opens directory roots only in this release",
            ));
        }
        let lease = RootLease::acquire_with_profile(&path, FilesystemProfile::Native)?;
        let root = Self {
            path,
            lease,
            config: options.config,
        };
        root.check_lease()?;
        root.refuse_branch_worktree()?;
        root.writer()?.recover_repository_materialization()?;
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
        self.lease.assert_owned()?;
        let canonical = fylo_vfs::canonicalize(&self.path).map_err(|_| {
            Error::new("EROOTLEASELOST", "cannot resolve the owning root directory")
        })?;
        if canonical != self.lease.root() {
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
        self.lease.release_checked().map_err(Error::from)
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

    fn options(&self) -> RootOpenOptions {
        RootOpenOptions::new(&self.path)
            .with_profile(FilesystemProfile::Native)
            .with_lease(self.lease.clone())
    }

    fn writer(&self) -> Result<NativeWriteRoot, Error> {
        self.lease.assert_owned()?;
        Ok(NativeWriteRoot::open_profiled(self.options())?.with_config(self.config))
    }

    fn write_engine(&self) -> Result<WriteEngine, Error> {
        self.lease.assert_owned()?;
        Ok(WriteEngine::open_profiled(self.options())?.with_config(self.config))
    }

    fn reader(&self) -> Result<ReadOnlyEngine, Error> {
        self.lease.assert_owned()?;
        ReadOnlyEngine::open_profiled(self.options()).map_err(Error::from)
    }

    fn native_queue(&self) -> Result<NativeQueue, Error> {
        NativeQueue::open_with_lease(self.lease.clone()).map_err(Error::from)
    }

    /// The machine protocol resolves a checked-out branch to its
    /// materialized worktree. This release serves the main worktree only, so
    /// a root on another branch is refused rather than read from the wrong
    /// tree.
    fn refuse_branch_worktree(&self) -> Result<(), Error> {
        let head = self.path.join(".fylo-vcs").join("HEAD");
        let Ok(text) = fylo_vfs::read_to_string(&head) else {
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
