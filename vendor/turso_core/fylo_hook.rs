//! FYLO: let the embedder claim a database path, so a binding built with
//! FYLO opens a FYLO root as its database instead of a file. With no hook
//! installed, or a path the hook declines, opening behaves as upstream.

use crate::sync::Arc;
use crate::{Database, Result};
use std::sync::OnceLock;

/// Returns `None` for a path it does not claim.
pub type OpenHook = fn(&str) -> Option<Result<Arc<Database>>>;

static OPEN_HOOK: OnceLock<OpenHook> = OnceLock::new();

/// Install the hook once; later calls keep the first hook.
pub fn install_open_hook(hook: OpenHook) {
    let _ = OPEN_HOOK.set(hook);
}

/// The database the hook opens for `path`, or `None` when no hook is
/// installed or the hook declines the path.
pub fn open_hooked(path: &str) -> Option<Result<Arc<Database>>> {
    OPEN_HOOK.get()?(path)
}
