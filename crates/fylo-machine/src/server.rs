//! `fylo serve`: one process owns a root and answers the machine protocol over
//! TCP, so a remote client pays one network round trip per operation instead
//! of one per file call on a network filesystem.

mod machine_server;
mod serve_options;
mod serve_token;
mod server_connection;
mod server_handle;
mod server_hello;
mod server_state;

pub use machine_server::MachineServer;
pub use serve_options::ServeOptions;
pub use serve_token::ServeToken;
pub use server_handle::ServerHandle;
