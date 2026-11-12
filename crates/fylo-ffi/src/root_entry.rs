use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{PoisonError, RwLock};

use fylo::Root;

use crate::call_outcome::CallOutcome;

/// One open root behind a handle number. Calls share it; closing waits for
/// the calls already running, then takes the root out.
#[derive(Debug)]
pub(crate) struct RootEntry {
    root: RwLock<Option<Root>>,
    dead: AtomicBool,
}

impl RootEntry {
    pub(crate) fn new(root: Root) -> RootEntry {
        RootEntry {
            root: RwLock::new(Some(root)),
            dead: AtomicBool::new(false),
        }
    }

    /// Run `call` on the root, unless it was closed or a panic killed it.
    pub(crate) fn with_root(&self, call: impl FnOnce(&Root) -> CallOutcome) -> CallOutcome {
        let guard = self.root.read().unwrap_or_else(PoisonError::into_inner);
        Self::call_live(guard.as_ref(), self.dead.load(Ordering::Acquire), call)
    }

    /// A panic left the root in an unknown state: refuse every later call.
    pub(crate) fn mark_dead(&self) {
        self.dead.store(true, Ordering::Release);
    }

    /// Take the root out once no call is using it.
    pub(crate) fn take(&self) -> Option<Root> {
        self.root
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    fn call_live(
        root: Option<&Root>,
        dead: bool,
        call: impl FnOnce(&Root) -> CallOutcome,
    ) -> CallOutcome {
        match root {
            Some(_) if dead => CallOutcome::Panicked,
            Some(root) => call(root),
            None => CallOutcome::Closed,
        }
    }
}
