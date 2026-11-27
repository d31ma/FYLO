use super::super::CollectionWriteLock;
use super::commit_queue::ExclusiveGuard;
use crate::commit_check_scope::CommitCheckScope;

/// Exclusive use of one collection: by this thread within the process, and
/// by this process on disk. The file lock is released before the in-process
/// claim, so a waiting thread never finds the file still locked. While it is
/// held, owner checks are reused until a commit point (ADR 0033).
#[derive(Debug)]
pub(crate) struct CollectionGuard {
    _checks: CommitCheckScope,
    _file: CollectionWriteLock,
    _exclusive: ExclusiveGuard,
}

impl CollectionGuard {
    pub(crate) fn new(file: CollectionWriteLock, exclusive: ExclusiveGuard) -> Self {
        Self {
            _checks: CommitCheckScope::enter(),
            _file: file,
            _exclusive: exclusive,
        }
    }
}
