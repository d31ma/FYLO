use super::super::CollectionWriteLock;
use super::commit_queue::ExclusiveGuard;

/// Exclusive use of one collection: by this thread within the process, and
/// by this process on disk. The file lock is released before the in-process
/// claim, so a waiting thread never finds the file still locked.
#[derive(Debug)]
pub(crate) struct CollectionGuard {
    _file: CollectionWriteLock,
    _exclusive: ExclusiveGuard,
}

impl CollectionGuard {
    pub(crate) fn new(file: CollectionWriteLock, exclusive: ExclusiveGuard) -> Self {
        Self {
            _file: file,
            _exclusive: exclusive,
        }
    }
}
