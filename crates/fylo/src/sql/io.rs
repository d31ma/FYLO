use std::sync::Arc;

use turso_core::{
    Clock, File, IO, MemoryIO, MonotonicInstant, OpenFlags, Result, WallClockInstant,
};

/// The IO under the SQL layer's catalog: memory only, because no SQL engine
/// file is ever written; documents stay in FYLO. Wall-clock time is FYLO's
/// own clock, so `datetime('now')` and FYLO's timestamps agree.
pub(crate) struct FyloIo {
    memory: MemoryIO,
}

impl FyloIo {
    pub(crate) fn new() -> Self {
        Self {
            memory: MemoryIO::new(),
        }
    }
}

impl Clock for FyloIo {
    fn current_time_monotonic(&self) -> MonotonicInstant {
        MonotonicInstant::now()
    }

    fn current_time_wall_clock(&self) -> WallClockInstant {
        let millis = fylo_storage_native::unix_millis().unwrap_or(0);
        WallClockInstant {
            secs: i64::try_from(millis / 1_000).unwrap_or(i64::MAX),
            micros: u32::try_from(millis % 1_000).unwrap_or(0) * 1_000,
        }
    }
}

impl IO for FyloIo {
    fn open_file(&self, path: &str, flags: OpenFlags, direct: bool) -> Result<Arc<dyn File>> {
        self.memory.open_file(path, flags, direct)
    }

    fn remove_file(&self, path: &str) -> Result<()> {
        self.memory.remove_file(path)
    }

    fn file_id(&self, path: &str) -> Result<turso_core::io::FileId> {
        self.memory.file_id(path)
    }
}
