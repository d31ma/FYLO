use std::net::SocketAddr;
use std::time::Duration;

use fylo_engine::HotSet;

use crate::{FrameLimits, ServeToken};

/// Where and how a [`crate::MachineServer`] listens.
///
/// There is no default address: the operator names the interface, so the
/// server is never reachable from a network nobody chose.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    listen: SocketAddr,
    token: ServeToken,
    limits: FrameLimits,
    max_connections: usize,
    idle_timeout: Duration,
    hello_timeout: Duration,
    hot_set_bytes: usize,
}

impl ServeOptions {
    /// Connections served at once; more are refused with `ESERVERBUSY`.
    pub const DEFAULT_MAX_CONNECTIONS: usize = 16;
    /// The largest connection limit an operator may configure.
    pub const MAX_CONNECTIONS: usize = 1024;
    /// How long a connection may wait between frames before it is closed.
    pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_mins(5);
    /// How long a new connection has to authenticate.
    pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(5);

    /// Listen on `listen` and admit clients holding `token`.
    #[must_use]
    pub fn new(listen: SocketAddr, token: ServeToken) -> Self {
        Self {
            listen,
            token,
            limits: FrameLimits::default(),
            max_connections: Self::DEFAULT_MAX_CONNECTIONS,
            idle_timeout: Self::DEFAULT_IDLE_TIMEOUT,
            hello_timeout: Self::DEFAULT_HELLO_TIMEOUT,
            hot_set_bytes: HotSet::DEFAULT_BUDGET_BYTES,
        }
    }

    /// Bound every request and response frame.
    #[must_use]
    pub fn with_limits(mut self, limits: FrameLimits) -> Self {
        self.limits = limits.clamped();
        self
    }

    /// Bound the connections served at once, from 1 to [`Self::MAX_CONNECTIONS`].
    #[must_use]
    pub fn with_max_connections(mut self, max_connections: usize) -> Self {
        self.max_connections = max_connections.clamp(1, Self::MAX_CONNECTIONS);
        self
    }

    /// Close a connection that sends nothing for `idle_timeout`.
    #[must_use]
    pub fn with_idle_timeout(mut self, idle_timeout: Duration) -> Self {
        self.idle_timeout = idle_timeout.max(Duration::from_millis(1));
        self
    }

    /// Close a connection that does not authenticate within `hello_timeout`.
    #[must_use]
    pub fn with_hello_timeout(mut self, hello_timeout: Duration) -> Self {
        self.hello_timeout = hello_timeout.max(Duration::from_millis(1));
        self
    }

    /// Keep at most `hot_set_bytes` of parsed documents and index snapshots
    /// in memory for warm reads; 0 turns the hot set off. The default is
    /// [`HotSet::DEFAULT_BUDGET_BYTES`].
    #[must_use]
    pub const fn with_hot_set_budget(mut self, hot_set_bytes: usize) -> Self {
        self.hot_set_bytes = hot_set_bytes;
        self
    }

    pub(crate) const fn listen(&self) -> SocketAddr {
        self.listen
    }

    pub(crate) const fn token(&self) -> &ServeToken {
        &self.token
    }

    pub(crate) const fn limits(&self) -> FrameLimits {
        self.limits
    }

    pub(crate) const fn max_connections(&self) -> usize {
        self.max_connections
    }

    pub(crate) const fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    pub(crate) const fn hello_timeout(&self) -> Duration {
        self.hello_timeout
    }

    pub(crate) const fn hot_set_bytes(&self) -> usize {
        self.hot_set_bytes
    }
}
