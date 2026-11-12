//! FYLO's C ABI (ADR 0026): the `fylo` crate for iOS and Android apps, and
//! any other host that links a C library.
//!
//! The header is `include/fylo.h`. Every input is a pointer and a length,
//! borrowed for the call. Every output is a [`FyloBytes`] buffer the caller
//! gives back to `fylo_bytes_free`. A root is a handle number, never a
//! pointer. Results and errors are the machine protocol's JSON.

mod c_abi;
mod call_failure;
mod call_outcome;
mod create_mode;
mod create_settings;
mod engine_settings;
mod fylo_bytes;
mod fylo_status;
mod identity_calls;
mod identity_settings;
mod open_settings;
mod root_calls;
mod root_entry;
mod root_table;
mod trust_settings;

pub use c_abi::CAbi;
pub use fylo_bytes::FyloBytes;
pub use fylo_status::FyloStatus;
