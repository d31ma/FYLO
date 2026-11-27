use std::cell::RefCell;
use std::marker::PhantomData;

use crate::read_scope_state::ReadScopeState;

/// One commit on the current thread, while its writer holds the collection
/// lock.
///
/// Inside the scope a root's owner and profile checks run once and are
/// reused, and a commit point reads the owner record again (ADR 0033: one
/// read of the owner record per commit). Without it every step of a commit
/// read the owner record again, about 55 reads per put, and on a network
/// share each read is a round trip. The scope ends when the value is
/// dropped, and it stays on the thread that entered it.
#[derive(Debug)]
#[must_use = "the scope ends when it is dropped"]
pub(crate) struct CommitCheckScope {
    _thread: PhantomData<*const ()>,
}

impl CommitCheckScope {
    pub(crate) fn enter() -> Self {
        Self::with(ReadScopeState::open);
        Self {
            _thread: PhantomData,
        }
    }

    pub(crate) fn is_verified(key: u64) -> bool {
        Self::with(|state| state.is_verified(key))
    }

    pub(crate) fn remember_verified(key: u64) {
        Self::with(|state| state.remember_verified(key));
    }

    pub(crate) fn forget_verified() {
        Self::with(ReadScopeState::forget_verified);
    }

    fn with<R>(action: impl FnOnce(&mut ReadScopeState) -> R) -> R {
        thread_local! {
            static STATE: RefCell<ReadScopeState> = RefCell::new(ReadScopeState::default());
        }
        STATE.with(|state| action(&mut state.borrow_mut()))
    }
}

impl Drop for CommitCheckScope {
    fn drop(&mut self) {
        Self::with(ReadScopeState::close);
    }
}
