use std::cell::RefCell;
use std::thread::LocalKey;

/// How much of a segment collection one thread's storage calls decoded,
/// recorded between [`SegmentReadLog::start`] and [`SegmentReadLog::finish`].
///
/// A commit record holds one manifest entry per document it wrote, so
/// reading its documents one by one must not decode its whole manifest
/// each time. Tests use this log to hold a read to the entries and frames
/// it needs. Nothing is recorded while no log is started, so a process
/// that never starts one only pays one thread-local check per decode.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SegmentReadLog {
    entries: usize,
    frames: usize,
}

impl SegmentReadLog {
    /// Start recording on this thread, dropping anything recorded before.
    pub fn start() {
        Self::slot().with(|slot| slot.replace(Some(Self::default())));
    }

    /// Stop recording on this thread and return what was recorded; an empty
    /// log when recording was not started.
    #[must_use]
    pub fn finish() -> Self {
        Self::slot()
            .with(|slot| slot.replace(None))
            .unwrap_or_default()
    }

    /// Manifest entries decoded, counting every entry of each manifest
    /// decoded whole and one for each single entry picked out of one.
    #[must_use]
    pub fn entries_decoded(&self) -> usize {
        self.entries
    }

    /// Record frames decoded (read, checked and decompressed).
    #[must_use]
    pub fn frames_decoded(&self) -> usize {
        self.frames
    }

    pub(crate) fn record_entries(count: usize) {
        Self::slot().with(|slot| Self::add(&mut slot.borrow_mut(), count, 0));
    }

    pub(crate) fn record_frame() {
        Self::slot().with(|slot| Self::add(&mut slot.borrow_mut(), 0, 1));
    }

    fn add(log: &mut Option<Self>, entries: usize, frames: usize) {
        if let Some(log) = log {
            log.entries += entries;
            log.frames += frames;
        }
    }

    fn slot() -> &'static LocalKey<RefCell<Option<Self>>> {
        thread_local! {
            static SLOT: RefCell<Option<SegmentReadLog>> = const { RefCell::new(None) };
        }
        &SLOT
    }
}
