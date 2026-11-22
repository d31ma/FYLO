use super::lease_observation::LeaseObservation;
use super::lease_record::LeaseRecord;
use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The record a heartbeat thread keeps alive, the flag it raises when the
/// lease was taken over, and the lock it shares with its owner so the
/// owner's own fencing read never sees a half-written beat (records are
/// rewritten in place, see `LeaseRecordFile`).
pub(super) struct HeartbeatTarget {
    pub(super) path: PathBuf,
    pub(super) lost: Arc<AtomicBool>,
    pub(super) record_lock: Arc<Mutex<()>>,
    pub(super) last_write_failed: Cell<bool>,
}

impl HeartbeatTarget {
    /// One heartbeat. `None` when the lease was lost.
    ///
    /// Only a readable record that names another owner, or a released one,
    /// proves the lease lost. A record that cannot be read now (a network
    /// hiccup) skips this beat without writing, so a beat never replaces a
    /// record this process could not see.
    pub(super) fn beat(&self, record: LeaseRecord) -> Option<LeaseRecord> {
        let _turn = Self::hold(&self.record_lock);
        let Some(recorded) = LeaseObservation::read(&self.path).record() else {
            return Some(self.repair_own_write(record));
        };
        self.mark_lost_unless(recorded.same_owner(&record))?;
        let next = record.beat();
        self.write(&next);
        Some(next)
    }

    /// A record that cannot be read now is rewritten only when this
    /// thread's own last write failed (a full disk can leave it short):
    /// then the broken bytes are this owner's. Otherwise someone else wrote
    /// them, or the network failed the read, and nothing is written.
    fn repair_own_write(&self, record: LeaseRecord) -> LeaseRecord {
        if !self.last_write_failed.get() {
            return record;
        }
        let next = record.beat();
        self.write(&next);
        next
    }

    /// A failed write is retried at the next beat: a short network hiccup
    /// must not end the lease. Fencing keeps writes safe if the hiccup
    /// outlasts the takeover time.
    fn write(&self, record: &LeaseRecord) {
        let written = record.publish(&self.path).is_ok();
        self.last_write_failed.set(!written);
    }

    pub(super) fn hold(record_lock: &Mutex<()>) -> MutexGuard<'_, ()> {
        record_lock.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn mark_lost_unless(&self, owned: bool) -> Option<()> {
        if owned {
            return Some(());
        }
        self.lost.store(true, Ordering::Release);
        None
    }
}
