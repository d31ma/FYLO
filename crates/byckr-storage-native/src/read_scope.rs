use crate::read_scope_state::ReadScopeState;
use std::io;
use std::path::{Path, PathBuf};

/// One read-only operation on the current thread.
///
/// Inside a scope, a root's ownership and profile checks run once per root
/// and lease, a path is resolved to its canonical form once, and each
/// collection's catalog descriptor is read once. Each check
/// reads files that only a write or another owner can change, and a read
/// operation changes neither, so repeating them inside one operation found
/// nothing new; on a network filesystem each repeat cost round trips.
/// Writers never reuse a check, scoped or not. The scope ends when the value
/// is dropped; scopes on other threads are separate.
#[derive(Debug)]
#[must_use = "the scope ends when it is dropped"]
pub struct ReadScope {
    active: bool,
}

impl ReadScope {
    /// Enter a scope for a read-only operation. With `read_only` false the
    /// value does nothing, so a caller can scope every operation the same
    /// way.
    pub fn enter(read_only: bool) -> Self {
        let scope = Self { active: read_only };
        scope.begin();
        scope
    }

    /// The canonical form of `path`, resolved once per scope.
    ///
    /// # Errors
    ///
    /// Returns the error of [`std::fs::canonicalize`].
    pub fn canonicalize(path: &Path) -> io::Result<PathBuf> {
        let known = ReadScopeState::with(|state| state.canonical(path));
        known.map_or_else(|| Self::resolve(path), Ok)
    }

    pub(crate) fn is_verified(key: u64) -> bool {
        ReadScopeState::with(|state| state.is_verified(key))
    }

    pub(crate) fn remember_verified(key: u64) {
        ReadScopeState::with(|state| state.remember_verified(key));
    }

    /// The descriptor bytes this scope already read from `path`.
    pub(crate) fn known_descriptor(path: &Path) -> Option<Vec<u8>> {
        ReadScopeState::with(|state| state.descriptor(path))
    }

    pub(crate) fn remember_descriptor(path: &Path, bytes: &[u8]) {
        ReadScopeState::with(|state| state.remember_descriptor(path, bytes));
    }

    fn resolve(path: &Path) -> io::Result<PathBuf> {
        let canonical = byckr_vfs::canonicalize(path)?;
        ReadScopeState::with(|state| state.remember_canonical(path, &canonical));
        Ok(canonical)
    }

    fn begin(&self) {
        if self.active {
            ReadScopeState::with(ReadScopeState::open);
        }
    }

    fn end(&self) {
        if self.active {
            ReadScopeState::with(ReadScopeState::close);
        }
    }
}

impl Drop for ReadScope {
    fn drop(&mut self) {
        self.end();
    }
}
