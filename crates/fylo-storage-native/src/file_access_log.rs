use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::thread::LocalKey;

/// The files one thread's storage calls opened and the folders they listed,
/// in order, recorded between [`FileAccessLog::start`] and
/// [`FileAccessLog::finish`].
///
/// A cold page cache makes every distinct file a request opens a disk read,
/// so tests use this log to hold a request to the files it needs. Nothing is
/// recorded while no log is started, so a process that never starts one only
/// pays one thread-local check per open.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FileAccessLog {
    opened: Vec<PathBuf>,
    listed: Vec<PathBuf>,
}

impl FileAccessLog {
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

    /// Every file opened for reading, once per open.
    #[must_use]
    pub fn opened(&self) -> &[PathBuf] {
        &self.opened
    }

    /// Every folder listed, once per listing.
    #[must_use]
    pub fn listed(&self) -> &[PathBuf] {
        &self.listed
    }

    /// How many opens were of a file whose path ends with `suffix`.
    #[must_use]
    pub fn opens_ending_with(&self, suffix: &str) -> usize {
        self.opened
            .iter()
            .filter(|path| path.ends_with(suffix))
            .count()
    }

    /// How many opens were of a file inside a folder named `folder`.
    #[must_use]
    pub fn opens_inside(&self, folder: &str) -> usize {
        self.opened
            .iter()
            .filter(|path| Self::has_folder(path, folder))
            .count()
    }

    pub(crate) fn record_open(path: &Path) {
        Self::slot().with(|slot| Self::push_opened(&mut slot.borrow_mut(), path));
    }

    pub(crate) fn record_listing(path: &Path) {
        Self::slot().with(|slot| Self::push_listed(&mut slot.borrow_mut(), path));
    }

    fn push_opened(log: &mut Option<Self>, path: &Path) {
        if let Some(log) = log {
            log.opened.push(path.to_path_buf());
        }
    }

    fn push_listed(log: &mut Option<Self>, path: &Path) {
        if let Some(log) = log {
            log.listed.push(path.to_path_buf());
        }
    }

    fn has_folder(path: &Path, folder: &str) -> bool {
        path.parent()
            .is_some_and(|parent| parent.components().any(|part| part.as_os_str() == folder))
    }

    fn slot() -> &'static LocalKey<RefCell<Option<Self>>> {
        thread_local! {
            static SLOT: RefCell<Option<FileAccessLog>> = const { RefCell::new(None) };
        }
        &SLOT
    }
}
