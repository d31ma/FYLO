use super::lease_observation::LeaseObservation;
use super::lease_record::LeaseRecord;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

/// The thread that proves an owner is alive: every heartbeat interval it
/// reads the record, checks it still names this owner and generation, and
/// writes it again with the counter one higher.
///
/// When the record names someone else, this owner was taken over while it
/// could not beat (a paused process, a lost network). The thread then marks
/// the lease lost, so the next fencing check stops with `EROOTLEASELOST`
/// without reading anything, and never writes again.
///
/// A target that cannot start threads (WebAssembly) has no heartbeat; a
/// process there is never taken over while it lives, because no other
/// machine shares its root.
#[derive(Debug)]
pub(super) struct LeaseHeartbeat {
    lost: Arc<AtomicBool>,
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl LeaseHeartbeat {
    const THREAD_NAME: &str = "fylo-lease-heartbeat";

    pub(super) fn start(path: PathBuf, record: LeaseRecord, interval: Duration) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel::<()>();
        let flag = Arc::clone(&lost);
        let thread = std::thread::Builder::new()
            .name(Self::THREAD_NAME.to_owned())
            .spawn(move || Self::run(&path, record, interval, &stopped, &flag))
            .ok();
        Self {
            lost,
            stop: Some(stop),
            thread,
        }
    }

    pub(super) fn lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Stop beating and wait until the thread has written its last record,
    /// so nothing it writes can land after a clean stop's record.
    pub(super) fn stop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }

    fn run(
        path: &std::path::Path,
        record: LeaseRecord,
        interval: Duration,
        stopped: &mpsc::Receiver<()>,
        lost: &AtomicBool,
    ) {
        let mut current = Some(record);
        while let Some(record) = current.take().filter(|_| Self::keep_beating(stopped, interval)) {
            current = Self::beat(path, record, lost);
        }
    }

    fn keep_beating(stopped: &mpsc::Receiver<()>, interval: Duration) -> bool {
        matches!(stopped.recv_timeout(interval), Err(RecvTimeoutError::Timeout))
    }

    /// One heartbeat. `None` when the lease was lost.
    ///
    /// Only a readable record that names another owner, or a released one,
    /// proves the lease lost. A record that cannot be read now (a network
    /// hiccup) skips this beat without writing, so a beat never replaces a
    /// record this process could not see.
    fn beat(path: &std::path::Path, record: LeaseRecord, lost: &AtomicBool) -> Option<LeaseRecord> {
        let Some(recorded) = LeaseObservation::read(path).record() else {
            return Some(record);
        };
        Self::mark_lost_unless(recorded.same_owner(&record), lost)?;
        let next = record.beat();
        // A failed write is retried at the next beat: a short network
        // hiccup must not end the lease. Fencing keeps writes safe if the
        // hiccup outlasts the takeover time.
        let _ = next.publish(path);
        Some(next)
    }

    fn mark_lost_unless(owned: bool, lost: &AtomicBool) -> Option<()> {
        if owned {
            return Some(());
        }
        lost.store(true, Ordering::Release);
        None
    }
}
