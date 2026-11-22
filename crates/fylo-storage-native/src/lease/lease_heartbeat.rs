use super::heartbeat_target::HeartbeatTarget;
use super::lease_record::LeaseRecord;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
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

    /// `record_lock` is held for every read and write of the record, by
    /// this thread and by the owner.
    pub(super) fn start(
        path: PathBuf,
        record: LeaseRecord,
        interval: Duration,
        record_lock: Arc<Mutex<()>>,
    ) -> Self {
        let lost = Arc::new(AtomicBool::new(false));
        let (stop, stopped) = mpsc::channel::<()>();
        let target = HeartbeatTarget {
            path,
            lost: Arc::clone(&lost),
            record_lock,
        };
        let thread = std::thread::Builder::new()
            .name(Self::THREAD_NAME.to_owned())
            .spawn(move || Self::run(&target, record, interval, &stopped))
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
        target: &HeartbeatTarget,
        record: LeaseRecord,
        interval: Duration,
        stopped: &mpsc::Receiver<()>,
    ) {
        let mut current = Self::first_beat(target, record, stopped);
        while let Some(record) = current
            .take()
            .filter(|_| Self::keep_beating(stopped, interval))
        {
            current = target.beat(record);
        }
    }

    /// Add the machine id and process start the claim left out (Windows),
    /// and publish them at once, unless the owner is already stopping.
    fn first_beat(
        target: &HeartbeatTarget,
        record: LeaseRecord,
        stopped: &mpsc::Receiver<()>,
    ) -> Option<LeaseRecord> {
        let identify = record.needs_identity() && Self::still_running(stopped);
        if !identify {
            return Some(record);
        }
        target.beat(record.identified())
    }

    fn still_running(stopped: &mpsc::Receiver<()>) -> bool {
        matches!(stopped.try_recv(), Err(mpsc::TryRecvError::Empty))
    }

    fn keep_beating(stopped: &mpsc::Receiver<()>, interval: Duration) -> bool {
        matches!(
            stopped.recv_timeout(interval),
            Err(RecvTimeoutError::Timeout)
        )
    }
}
