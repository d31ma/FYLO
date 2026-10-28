use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use crate::{ApplicationConfig, ServeOptions};

/// What every connection of one server shares.
///
/// The executor runs one frame at a time across all connections. Within one
/// process the engine has a single writer per root, as `exec --loop` does,
/// and two frames writing the same collection would otherwise refuse each
/// other with `EENGINE_CONCURRENT_WRITE`.
pub(crate) struct ServerState {
    options: ServeOptions,
    application: ApplicationConfig,
    wake_address: SocketAddr,
    executor: Mutex<()>,
    stopping: AtomicBool,
    connections: Mutex<usize>,
}

impl ServerState {
    const WAKE_TIMEOUT: Duration = Duration::from_secs(1);

    pub(crate) fn new(
        options: ServeOptions,
        application: ApplicationConfig,
        wake_address: SocketAddr,
    ) -> Self {
        Self {
            options,
            application,
            wake_address,
            executor: Mutex::new(()),
            stopping: AtomicBool::new(false),
            connections: Mutex::new(0),
        }
    }

    pub(crate) const fn options(&self) -> &ServeOptions {
        &self.options
    }

    pub(crate) const fn application(&self) -> &ApplicationConfig {
        &self.application
    }

    /// Take the executor. A panic inside a frame poisons it, and then the
    /// server stops rather than run more frames beside unknown state.
    pub(crate) fn execute(&self) -> std::io::Result<MutexGuard<'_, ()>> {
        self.executor.lock().map_err(|_| {
            self.request_shutdown();
            std::io::Error::other("a frame panicked, so the server is stopping")
        })
    }

    pub(crate) fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::SeqCst)
    }

    /// Stop accepting. Idle connections notice within one poll interval; a
    /// running frame finishes and writes its response first. The accept loop
    /// is woken by a connection to itself.
    pub(crate) fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        drop(TcpStream::connect_timeout(
            &self.wake_address,
            Self::WAKE_TIMEOUT,
        ));
    }

    /// Count a new connection; false at the limit or while stopping.
    pub(crate) fn register(&self) -> bool {
        let mut connections = self.lock_connections();
        let admitted = !self.is_stopping() && *connections < self.options.max_connections();
        *connections += usize::from(admitted);
        admitted
    }

    pub(crate) fn unregister(&self) {
        *self.lock_connections() -= 1;
    }

    fn lock_connections(&self) -> MutexGuard<'_, usize> {
        self.connections
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
