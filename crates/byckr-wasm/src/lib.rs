//! FYLO as a WebAssembly module for a host that owns storage.
//!
//! The WASI build already runs the whole engine wherever there are files and
//! stdio. This crate is for the one environment that has neither: a browser,
//! or any embedder that wants to supply storage itself. It exports the same
//! NDJSON machine protocol the binary speaks over stdin and stdout, as a
//! buffer in and a buffer out.
//!
//! # Contract
//!
//! The host provides twelve filesystem functions as imports from the
//! `byckr_host` module — the `byckr_vfs::HostVfs` table — and calls:
//!
//! 1. `byckr_abi_version` and refuses an unrecognized answer;
//! 2. `byckr_alloc` to obtain a buffer, writing the NDJSON request into it;
//! 3. `byckr_exec`, which returns the response buffer packed as
//!    `(pointer << 32) | length`;
//! 4. `byckr_free` on both buffers.
//!
//! `byckr_exec` takes a batch of newline-delimited frames and answers all of
//! them, which is `exec --loop` in one call. The module keeps one session
//! across calls, so a query cursor from one batch pages on in the next, as it
//! does for the life of an `exec --loop` process.

#![cfg(target_arch = "wasm32")]

mod boundary;
mod browser_key;
mod browser_settings;

pub use boundary::{
    byckr_abi_version, byckr_alloc, byckr_configure, byckr_exec, byckr_free, byckr_set_database_key,
};
