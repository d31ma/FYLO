use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::time::Duration;

use fylo_engine::HotSet;

use crate::{ApplicationConfig, FrameSession, ServeOptions};

/// What every connection of one server shares.
///
/// The executor lets read frames run together and a frame that may write
/// run alone. Within one process the engine has a single writer per root, as
/// `exec --loop` does, and two frames writing the same collection would
/// otherwise refuse each other with `EENGINE_CONCURRENT_WRITE`. Because no
/// read runs beside a write, the hot set cleared around every write never
/// holds anything a later read should not see.
pub(crate) struct ServerState {
    options: ServeOptions,
    application: ApplicationConfig,
    wake_address: SocketAddr,
    executor: RwLock<()>,
    hot_set: Option<Arc<HotSet>>,
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
        let hot_set = Self::hot_set_for(&options);
        Self {
            options,
            application,
            wake_address,
            executor: RwLock::new(()),
            hot_set,
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

    /// Share the executor with other reads. A panic inside a frame poisons
    /// it, and then the server stops rather than run more frames beside
    /// unknown state.
    pub(crate) fn read(&self) -> std::io::Result<RwLockReadGuard<'_, ()>> {
        self.executor.read().map_err(|_| self.poisoned())
    }

    /// Take the executor alone, for a frame that may write.
    pub(crate) fn write(&self) -> std::io::Result<RwLockWriteGuard<'_, ()>> {
        self.executor.write().map_err(|_| self.poisoned())
    }

    /// Forget every resident read; called around each frame that may write.
    pub(crate) fn clear_hot_set(&self) {
        if let Some(hot_set) = &self.hot_set {
            hot_set.clear();
        }
    }

    /// Give a new session the server's hot set, when there is one.
    pub(crate) fn share_hot_set(&self, session: FrameSession) -> FrameSession {
        match &self.hot_set {
            Some(hot_set) => session.with_hot_set(Arc::clone(hot_set)),
            None => session,
        }
    }

    fn poisoned(&self) -> std::io::Error {
        self.request_shutdown();
        std::io::Error::other("a frame panicked, so the server is stopping")
    }

    fn hot_set_for(options: &ServeOptions) -> Option<Arc<HotSet>> {
        let budget = options.hot_set_bytes();
        (budget > 0).then(|| Arc::new(HotSet::new(budget)))
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
