use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::thread::JoinHandle;

use fylo_storage_native::{FilesystemProfile, NativeWriteRoot, RootLease, RootOpenOptions};

use super::server_connection::ServerConnection;
use super::server_state::ServerState;
use crate::{ApplicationConfig, ServeOptions, ServerHandle};

/// A long-running process that owns one root and serves it over TCP.
///
/// It holds the root lease from [`Self::bind`] until [`Self::run`] returns,
/// so no other process can open the root meanwhile. Each connection gets its
/// own persistent machine session, and frames from all connections run one
/// at a time. If the process dies, the kernel drops the lock and the next
/// owner recovers the root, exactly as after a local process crash.
pub struct MachineServer {
    listener: TcpListener,
    lease: RootLease,
    state: Arc<ServerState>,
    address: SocketAddr,
}

impl MachineServer {
    const ACCEPT_RETRY_PAUSE: std::time::Duration = std::time::Duration::from_millis(50);

    /// Start listening, then take the root and recover it.
    ///
    /// The address is bound first, so a port conflict never touches the root.
    ///
    /// # Errors
    /// Fails when the address cannot be bound, or the root is owned by another
    /// process or cannot be recovered.
    pub fn bind(
        root: &Path,
        application: ApplicationConfig,
        options: ServeOptions,
    ) -> std::io::Result<Self> {
        let listener = TcpListener::bind(options.listen())?;
        let address = listener.local_addr()?;
        let lease = Self::acquire(root)?;
        Self::recover(&lease)?;
        let state = ServerState::new(options, application, Self::wake_address(address));
        Ok(Self {
            listener,
            lease,
            state: Arc::new(state),
            address,
        })
    }

    /// The bound address, with the real port when port 0 was requested.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// The canonical root this server owns.
    #[must_use]
    pub fn root(&self) -> &Path {
        self.lease.root()
    }

    /// A handle that stops this server from another thread.
    #[must_use]
    pub fn handle(&self) -> ServerHandle {
        ServerHandle::new(Arc::clone(&self.state), self.address)
    }

    /// Serve until shut down, then wait for every connection and release the
    /// root.
    ///
    /// # Errors
    /// Returns an accept failure, or a failure to release the root cleanly.
    pub fn run(self) -> std::io::Result<()> {
        let workers = self.accept_until_shutdown();
        Self::join(workers);
        drop(self.listener);
        self.lease
            .release_checked()
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    fn accept_until_shutdown(&self) -> Vec<JoinHandle<()>> {
        let mut workers = Vec::new();
        for stream in self.listener.incoming() {
            if self.state.is_stopping() {
                break;
            }
            workers.retain(|worker: &JoinHandle<()>| !worker.is_finished());
            workers.extend(self.accepted(stream));
        }
        workers
    }

    /// A failed accept (out of file descriptors, say) is retried after a
    /// pause rather than in a busy loop.
    fn accepted(&self, stream: std::io::Result<TcpStream>) -> Option<JoinHandle<()>> {
        let Ok(stream) = stream else {
            std::thread::sleep(Self::ACCEPT_RETRY_PAUSE);
            return None;
        };
        self.start(stream)
    }

    fn start(&self, stream: TcpStream) -> Option<JoinHandle<()>> {
        if !self.state.register() {
            ServerConnection::refuse_busy(stream);
            return None;
        }
        let state = Arc::clone(&self.state);
        let connection = ServerConnection::new(stream, Arc::clone(&state), self.lease.clone());
        Some(std::thread::spawn(move || {
            drop(connection.run());
            state.unregister();
        }))
    }

    fn join(workers: Vec<JoinHandle<()>>) {
        for worker in workers {
            drop(worker.join());
        }
    }

    fn acquire(root: &Path) -> std::io::Result<RootLease> {
        let lease = RootLease::acquire_with_profile(root, FilesystemProfile::Native)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        Self::require_kernel_lock(&lease)?;
        Ok(lease)
    }

    fn require_kernel_lock(lease: &RootLease) -> std::io::Result<()> {
        if lease.kernel_enforced() {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "fylo serve needs a filesystem with a real kernel advisory lock",
        ))
    }

    fn recover(lease: &RootLease) -> std::io::Result<()> {
        let options = RootOpenOptions::new(lease.root()).with_lease(lease.clone());
        NativeWriteRoot::open_profiled(options)
            .and_then(|writer| writer.recover_repository_materialization())
            .map_err(|error| std::io::Error::other(error.to_string()))
    }

    /// A wildcard address cannot be connected to, so shutdown wakes the
    /// accept loop through loopback instead.
    fn wake_address(address: SocketAddr) -> SocketAddr {
        let host = match address.ip() {
            IpAddr::V4(host) if host.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(host) if host.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
            host => host,
        };
        SocketAddr::new(host, address.port())
    }
}
