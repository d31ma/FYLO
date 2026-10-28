use std::net::SocketAddr;
use std::sync::Arc;

use super::server_state::ServerState;

/// Stops a running [`crate::MachineServer`] from another thread.
#[derive(Clone)]
pub struct ServerHandle {
    state: Arc<ServerState>,
    address: SocketAddr,
}

impl ServerHandle {
    pub(crate) const fn new(state: Arc<ServerState>, address: SocketAddr) -> Self {
        Self { state, address }
    }

    /// The address the server is listening on.
    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.address
    }

    /// Stop accepting, let running frames finish, then release the root.
    /// `MachineServer::run` returns once that is done.
    pub fn shutdown(&self) {
        self.state.request_shutdown();
    }
}
