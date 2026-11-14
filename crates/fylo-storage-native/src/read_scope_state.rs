use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// What the current thread's open [`crate::ReadScope`] has checked so far.
#[derive(Debug, Default)]
pub(crate) struct ReadScopeState {
    depth: usize,
    verified: Vec<u64>,
    canonical: Vec<(PathBuf, PathBuf)>,
}

impl ReadScopeState {
    pub(crate) fn with<R>(action: impl FnOnce(&mut Self) -> R) -> R {
        thread_local! {
            static STATE: RefCell<ReadScopeState> = RefCell::new(ReadScopeState::default());
        }
        STATE.with(|state| action(&mut state.borrow_mut()))
    }

    pub(crate) fn open(&mut self) {
        self.depth += 1;
    }

    pub(crate) fn close(&mut self) {
        self.depth = self.depth.saturating_sub(1);
        self.forget_when_closed();
    }

    pub(crate) fn is_verified(&self, key: u64) -> bool {
        self.verified.contains(&key)
    }

    pub(crate) fn remember_verified(&mut self, key: u64) {
        if self.is_open() {
            self.verified.push(key);
        }
    }

    pub(crate) fn canonical(&self, path: &Path) -> Option<PathBuf> {
        self.canonical
            .iter()
            .find(|(asked, _)| asked == path)
            .map(|(_, canonical)| canonical.clone())
    }

    /// Remember `canonical` as the answer for `path` and for itself, since a
    /// caller that already holds the canonical form often resolves it again.
    pub(crate) fn remember_canonical(&mut self, path: &Path, canonical: &Path) {
        if self.is_open() {
            self.canonical
                .push((path.to_path_buf(), canonical.to_path_buf()));
            self.canonical
                .push((canonical.to_path_buf(), canonical.to_path_buf()));
        }
    }

    fn is_open(&self) -> bool {
        self.depth > 0
    }

    fn forget_when_closed(&mut self) {
        if !self.is_open() {
            self.verified.clear();
            self.canonical.clear();
        }
    }
}
