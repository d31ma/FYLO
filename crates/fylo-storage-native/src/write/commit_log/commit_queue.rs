use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::ThreadId;

use super::log_segment::LogSegment;
use crate::{NativeStorageError, NativeStorageErrorCode};

/// The in-process meeting point for every writer of one collection.
///
/// Log writers share a busy period: they append in turn, one of them flushes
/// for everyone appended so far, and each applies its own commit. Journal
/// operations and recovery take the queue exclusively instead.
#[derive(Debug, Default)]
pub(crate) struct CommitQueue {
    state: Mutex<QueueState>,
    changed: Condvar,
}

#[derive(Debug, Default)]
pub(crate) struct QueueState {
    pub(crate) exclusive: Option<ThreadId>,
    pub(crate) busy: Option<BusyPeriod>,
    /// Root path and collection name, so a closing process can checkpoint
    /// without this registry holding a root (and its lease) open.
    pub(crate) origin: Option<(PathBuf, String)>,
}

/// Shared state while at least one log writer is between its plan and its
/// finish.
#[derive(Debug)]
pub(crate) struct BusyPeriod {
    pub(crate) _lock: Option<super::super::CollectionWriteLock>,
    pub(crate) segment: Option<LogSegment>,
    pub(crate) generation: u64,
    pub(crate) published_writing: bool,
    pub(crate) members: usize,
    pub(crate) pending: BTreeSet<PathBuf>,
    pub(crate) appended_lsn: u64,
    pub(crate) durable_lsn: u64,
    pub(crate) durable_length: u64,
    pub(crate) durable_records: u64,
    pub(crate) flushing: bool,
    pub(crate) failure: Option<(NativeStorageErrorCode, String)>,
    pub(crate) stranded: bool,
}

impl CommitQueue {
    /// The queue for the collection whose state file is `key`.
    pub(crate) fn shared(key: &Path) -> Arc<Self> {
        let mut queues = Self::registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        Arc::clone(queues.entry(key.to_owned()).or_default())
    }

    /// Every queue this process has used, for a closing checkpoint.
    pub(crate) fn all() -> Vec<Arc<Self>> {
        Self::registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .cloned()
            .collect()
    }

    fn registry() -> &'static Mutex<BTreeMap<PathBuf, Arc<Self>>> {
        static QUEUES: OnceLock<Mutex<BTreeMap<PathBuf, Arc<CommitQueue>>>> = OnceLock::new();
        QUEUES.get_or_init(Mutex::default)
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, QueueState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn wait<'a>(&self, guard: MutexGuard<'a, QueueState>) -> MutexGuard<'a, QueueState> {
        self.changed
            .wait(guard)
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn notify(&self) {
        self.changed.notify_all();
    }

    /// Take the collection away from log writers until the guard drops.
    pub(crate) fn exclusive(self: &Arc<Self>) -> Result<ExclusiveGuard, NativeStorageError> {
        let current = std::thread::current().id();
        let mut state = self.lock();
        if state.exclusive == Some(current) {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::ConcurrentWrite,
                "collection write lock is already held by this thread",
            ));
        }
        while state.exclusive.is_some() || state.busy.is_some() {
            state = self.wait(state);
        }
        state.exclusive = Some(current);
        Ok(ExclusiveGuard {
            queue: Arc::clone(self),
        })
    }
}

/// Exclusive use of one collection by a journal operation or recovery.
#[derive(Debug)]
pub(crate) struct ExclusiveGuard {
    queue: Arc<CommitQueue>,
}

impl Drop for ExclusiveGuard {
    fn drop(&mut self) {
        self.queue.lock().exclusive = None;
        self.queue.notify();
    }
}
