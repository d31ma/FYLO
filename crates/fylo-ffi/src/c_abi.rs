//! The C entry points (ADR 0026). Everything `unsafe` in `fylo-ffi` lives
//! here: reading the caller's pointers, and handing out and taking back
//! output buffers. Nothing below this file knows a C caller exists.

#![allow(
    unsafe_code,
    reason = "ADR 0026 confines the C boundary to this module"
)]

use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::call_failure::CallFailure;
use crate::call_outcome::CallOutcome;
use crate::fylo_bytes::FyloBytes;
use crate::fylo_status::FyloStatus;
use crate::identity_calls::IdentityCalls;
use crate::root_calls::RootCalls;
use crate::root_table::RootTable;

/// The C ABI. Its functions are exported under the names in `fylo.h`.
pub struct CAbi;

impl CAbi {
    /// The ABI this library implements; `fylo.h`'s `FYLO_ABI_VERSION`.
    pub const ABI_VERSION: u32 = 1;

    /// Report the ABI this library implements.
    #[unsafe(no_mangle)]
    pub extern "C" fn fylo_abi_version() -> u32 {
        Self::ABI_VERSION
    }

    /// Open the root or `.vdbx` database at `path`.
    ///
    /// # Safety
    ///
    /// Each input pointer addresses its length in readable bytes for the
    /// call (or is null with length 0); `out_root` and `out_error` are
    /// writable, or null to ignore them.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_open(
        path: *const u8,
        path_len: usize,
        options: *const u8,
        options_len: usize,
        key: *const u8,
        key_len: usize,
        out_root: *mut u64,
        out_error: *mut FyloBytes,
    ) -> FyloStatus {
        let opened = Self::guarded(|| {
            // SAFETY: the caller's contract above.
            let path = unsafe { Self::text(path, path_len) }?;
            // SAFETY: the caller's contract above.
            let options = unsafe { Self::text(options, options_len) }?;
            // SAFETY: the caller's contract above.
            let key = unsafe { Self::bytes(key, key_len) }?;
            Self::refuse_key(key)?;
            RootCalls::open(path, options)
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver_root(opened, out_root, out_error) }
    }

    /// Close a root. The handle is gone afterwards, even when closing fails.
    ///
    /// # Safety
    ///
    /// `out_error` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_close(root: u64, out_error: *mut FyloBytes) -> FyloStatus {
        let outcome = Self::root_call(root, || Ok(RootCalls::close(root)));
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out_error) }
    }

    /// Create a document collection.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_create_collection(
        root: u64,
        name: *const u8,
        name_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::root_call(root, || {
            // SAFETY: the caller's contract above.
            let name = unsafe { Self::text(name, name_len) }?;
            Ok(RootCalls::create_collection(root, name))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Store a new document; the result is its TTID as a JSON string.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_put(
        root: u64,
        collection: *const u8,
        collection_len: usize,
        document: *const u8,
        document_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::root_call(root, || {
            // SAFETY: the caller's contract above.
            let collection = unsafe { Self::text(collection, collection_len) }?;
            // SAFETY: the caller's contract above.
            let document = unsafe { Self::text(document, document_len) }?;
            Ok(RootCalls::put(root, collection, document))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Read one document: `{"<id>": {...}}`, or `FYLO_NOT_FOUND`.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_get(
        root: u64,
        collection: *const u8,
        collection_len: usize,
        id: *const u8,
        id_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::root_call(root, || {
            // SAFETY: the caller's contract above.
            let collection = unsafe { Self::text(collection, collection_len) }?;
            // SAFETY: the caller's contract above.
            let id = unsafe { Self::text(id, id_len) }?;
            Ok(RootCalls::get(root, collection, id))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Delete one document, keeping its tombstone.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_delete(
        root: u64,
        collection: *const u8,
        collection_len: usize,
        id: *const u8,
        id_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::root_call(root, || {
            // SAFETY: the caller's contract above.
            let collection = unsafe { Self::text(collection, collection_len) }?;
            // SAFETY: the caller's contract above.
            let id = unsafe { Self::text(id, id_len) }?;
            Ok(RootCalls::delete(root, collection, id))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Every document a structured query matches: `{"<id>": {...}, ...}`
    /// in TTID order.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_find(
        root: u64,
        collection: *const u8,
        collection_len: usize,
        query: *const u8,
        query_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::root_call(root, || {
            // SAFETY: the caller's contract above.
            let collection = unsafe { Self::text(collection, collection_len) }?;
            // SAFETY: the caller's contract above.
            let query = unsafe { Self::text(query, query_len) }?;
            Ok(RootCalls::find(root, collection, query))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Make an application identity's key where `reference` names, and
    /// describe it: `{"id": "fid1:...", "publicKey": "fpk1:..."}`.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_identity_create(
        reference: *const u8,
        reference_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::guarded_outcome(|| {
            // SAFETY: the caller's contract above.
            let reference = unsafe { Self::text(reference, reference_len) }?;
            Ok(IdentityCalls::create(reference))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Describe the identity `reference` names.
    ///
    /// # Safety
    ///
    /// As [`CAbi::fylo_open`] for the inputs; `out` is writable, or null.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn fylo_identity_show(
        reference: *const u8,
        reference_len: usize,
        out: *mut FyloBytes,
    ) -> FyloStatus {
        let outcome = Self::guarded_outcome(|| {
            // SAFETY: the caller's contract above.
            let reference = unsafe { Self::text(reference, reference_len) }?;
            Ok(IdentityCalls::show(reference))
        });
        // SAFETY: the caller's contract above.
        unsafe { Self::deliver(outcome, out) }
    }

    /// Give back a buffer FYLO returned. An empty buffer is ignored.
    ///
    /// # Safety
    ///
    /// `bytes` came from a FYLO call, unchanged, and is freed once.
    #[unsafe(no_mangle)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "C hands the buffer back by value, and this call frees it"
    )]
    pub unsafe extern "C" fn fylo_bytes_free(bytes: FyloBytes) {
        if bytes.ptr.is_null() {
            return;
        }
        let slice = std::ptr::slice_from_raw_parts_mut(bytes.ptr, bytes.len);
        // SAFETY: `FyloBytes::from_vec` made `ptr` from a boxed slice of
        // exactly `len` bytes, and the caller frees it once.
        drop(unsafe { Box::from_raw(slice) });
    }

    /// The caller's text, borrowed for the call.
    ///
    /// # Safety
    ///
    /// `ptr` addresses `len` readable bytes, or `len` is 0.
    unsafe fn text<'call>(ptr: *const u8, len: usize) -> Result<&'call str, CallFailure> {
        // SAFETY: the contract of this function.
        let bytes = unsafe { Self::bytes(ptr, len) }?;
        std::str::from_utf8(bytes).map_err(|_| CallFailure::bad_request("text must be UTF-8"))
    }

    /// The caller's bytes, borrowed for the call.
    ///
    /// # Safety
    ///
    /// `ptr` addresses `len` readable bytes, or `len` is 0.
    unsafe fn bytes<'call>(ptr: *const u8, len: usize) -> Result<&'call [u8], CallFailure> {
        if len == 0 {
            return Ok(&[]);
        }
        if ptr.is_null() {
            return Err(CallFailure::bad_request(
                "a null pointer came with a length",
            ));
        }
        // SAFETY: `ptr` is not null and addresses `len` readable bytes that
        // stay live and unchanged for the call (the contract above).
        Ok(unsafe { std::slice::from_raw_parts(ptr, len) })
    }

    /// Keys given as a value arrive in a later slice (ADR 0026 section 9).
    fn refuse_key(key: &[u8]) -> Result<(), CallFailure> {
        if key.is_empty() {
            return Ok(());
        }
        Err(CallFailure::bad_request(
            "this build of fylo_open takes no encryption key yet",
        ))
    }

    /// Run a call on `root`; a panic kills that root's handle.
    fn root_call(
        root: u64,
        call: impl FnOnce() -> Result<CallOutcome, CallFailure>,
    ) -> CallOutcome {
        let outcome = Self::guarded_outcome(call);
        Self::kill_after_panic(root, &outcome);
        outcome
    }

    fn kill_after_panic(root: u64, outcome: &CallOutcome) {
        if !matches!(outcome, CallOutcome::Panicked) {
            return;
        }
        if let Some(entry) = RootTable::shared().find(root) {
            entry.mark_dead();
        }
    }

    fn guarded_outcome(call: impl FnOnce() -> Result<CallOutcome, CallFailure>) -> CallOutcome {
        match Self::guarded(call) {
            Ok(Ok(outcome)) => outcome,
            Ok(Err(failure)) => CallOutcome::failed(failure),
            Err(()) => CallOutcome::Panicked,
        }
    }

    /// Catch a panic so it never unwinds into the caller's frames.
    fn guarded<T>(call: impl FnOnce() -> T) -> Result<T, ()> {
        catch_unwind(AssertUnwindSafe(call)).map_err(|_| ())
    }

    /// # Safety
    ///
    /// `out` is writable, or null.
    unsafe fn deliver(outcome: CallOutcome, out: *mut FyloBytes) -> FyloStatus {
        let status = outcome.status();
        // SAFETY: the contract of this function.
        unsafe { Self::hand_out(outcome, out) };
        status
    }

    /// Give the outcome's bytes to the caller; with no place to put them,
    /// make none, so nothing leaks.
    ///
    /// # Safety
    ///
    /// `out` is writable, or null.
    unsafe fn hand_out(outcome: CallOutcome, out: *mut FyloBytes) {
        if out.is_null() {
            return;
        }
        // SAFETY: `out` is not null and writable (the contract above).
        unsafe { Self::write(out, FyloBytes::from_vec(outcome.into_bytes())) };
    }

    /// # Safety
    ///
    /// `out_root` and `out_error` are writable, or null.
    unsafe fn deliver_root(
        opened: Result<Result<u64, CallFailure>, ()>,
        out_root: *mut u64,
        out_error: *mut FyloBytes,
    ) -> FyloStatus {
        match opened {
            Ok(Ok(handle)) => {
                // SAFETY: the contract of this function.
                unsafe { Self::write(out_root, handle) };
                // SAFETY: the contract of this function.
                unsafe { Self::write(out_error, FyloBytes::empty()) };
                FyloStatus::Ok
            }
            // SAFETY: the contract of this function.
            Ok(Err(failure)) => unsafe { Self::deliver(CallOutcome::failed(failure), out_error) },
            // SAFETY: the contract of this function.
            Err(()) => unsafe { Self::deliver(CallOutcome::Panicked, out_error) },
        }
    }

    /// Write `value` through `out`, or drop it when `out` is null.
    ///
    /// # Safety
    ///
    /// `out` is writable and aligned for `T`, or null.
    unsafe fn write<T>(out: *mut T, value: T) {
        if out.is_null() {
            return;
        }
        // SAFETY: `out` is not null, writable and aligned (the contract
        // above); the old value is the caller's uninitialized slot, so it is
        // overwritten, never dropped.
        unsafe { out.write(value) };
    }
}

#[cfg(test)]
#[allow(unsafe_code, reason = "these tests call the C ABI as a C host does")]
#[path = "c_abi_tests.rs"]
mod tests;
