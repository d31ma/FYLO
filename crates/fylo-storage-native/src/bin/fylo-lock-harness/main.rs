//! Multi-process qualification harness for `.vdbx` byte-range locks
//! (ADR 0016 section 7).
//!
//! ```text
//! fylo-lock-harness qualify --dir DIR [--procs 8] [--threads 2]
//!     [--seconds 10] [--writers 4] [--sync] [--label NAME] [--json FILE]
//! ```
//!
//! creates a scratch `.vdbx`-sized file in DIR and runs, in order:
//!
//! 1. the lock bytes leave the file's length and data alone;
//! 2. a conflict matrix: a child process holds a byte, the harness checks
//!    which requests conflict with it;
//! 3. a holder that exits releases, and a killed holder releases, timed;
//! 4. closing a second handle in the holder keeps its locks; closing a
//!    plain `File` opened by path is reported, since macOS drops them;
//! 5. load: `procs` processes of `threads` threads contend for the lease
//!    (mostly shared, sometimes exclusive), the allocation byte and
//!    `writers` collection writer bytes for `seconds`. Each holder checks an
//!    owner word in the file's data on entry and exit, and counts its entries
//!    there, so an overlap or a lost update fails the run.
//!
//! `--sync` flushes the data before each release, for runs across machines:
//!
//! ```text
//! fylo-lock-harness prepare --file SHARED                 # once
//! fylo-lock-harness share --file SHARED --machines 2 --machine I \
//!     --start-at UNIX_SECONDS --sync --json I.json         # on each machine
//! fylo-lock-harness verify --file SHARED 0.json 1.json
//! ```
//!
//! Before the load run, a short control run with the locks switched off must
//! show overlaps, which proves the witness can see one on this filesystem.
//! The exit status is 1 when any check fails.

#[cfg(not(target_family = "wasm"))]
mod harness;

#[cfg(not(target_family = "wasm"))]
fn main() -> std::process::ExitCode {
    harness::main()
}

// A WebAssembly host has no processes or byte-range locks to qualify.
#[cfg(target_family = "wasm")]
fn main() {}
