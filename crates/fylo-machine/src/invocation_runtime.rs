use std::io;
use std::time::{Duration, Instant};

use fylo_storage_native::{NativeStorageErrorCode, RootLease};
use serde_json::Value;

use crate::{InvocationOptions, StrictValue};

/// Exactly-one-frame native execution with checked teardown before response.
///
/// Only immutable host configuration survives between calls. Advisory root
/// ownership coordinates cooperating clients; it is not distributed fencing.
pub struct InvocationRuntime {
    options: InvocationOptions,
}

impl InvocationRuntime {
    /// Prepare configuration without acquiring a root lease or recovering data.
    ///
    /// # Errors
    /// Rejects a budget already inside its cleanup reserve.
    pub fn new(options: InvocationOptions) -> io::Result<Self> {
        InvocationOptions::check_work_budget(options.budget())?;
        Ok(Self { options })
    }

    /// Execute one bounded, nonempty LF-delimited core frame.
    ///
    /// # Errors
    /// Returns no completed response for invalid admission, exhausted budget,
    /// oversized output, or uncertain checked cleanup/unlock. A mutation may
    /// already have happened; this method never automatically replays it.
    pub fn execute(&self, frame: &[u8]) -> io::Result<Vec<u8>> {
        InvocationOptions::check_work_budget(self.options.budget())?;
        let request = self.prepare_request(frame)?;
        InvocationOptions::check_work_budget(self.options.budget())?;
        let lease = self.acquire_lease()?;
        // Retain every inner result until all private owning handles are gone.
        let result = self.execute_owned(&lease, &request);
        lease
            .release_checked()
            .map_err(|error| Self::storage_error(&error))?;
        self.options.check_completion_deadline()?;
        result
    }

    /// Wait for a root another invocation holds, within this budget.
    ///
    /// Concurrent provider invocations are normal, and refusing them at once
    /// pushes every caller into its own retry loop. Waiting never extends past
    /// the cleanup reserve; when the budget runs out the lock error is returned
    /// and nothing was mutated.
    fn acquire_lease(&self) -> io::Result<RootLease> {
        let root = self.options.root();
        let (deadline, reserve) = self.options.budget();
        let mut pause = Duration::from_millis(10);
        loop {
            match RootLease::acquire_with_profile(root.root(), root.filesystem_profile()) {
                Ok(lease) => return Ok(lease),
                Err(error) if error.code() == NativeStorageErrorCode::RootLocked => {
                    let usable = deadline
                        .checked_duration_since(Instant::now())
                        .and_then(|remaining| remaining.checked_sub(reserve))
                        .filter(|usable| !usable.is_zero())
                        .ok_or_else(|| Self::storage_error(&error))?;
                    std::thread::sleep(pause.min(usable));
                    pause = (pause * 2).min(Duration::from_millis(200));
                }
                Err(error) => return Err(Self::storage_error(&error)),
            }
        }
    }

    fn execute_owned(&self, lease: &RootLease, request: &Value) -> io::Result<Vec<u8>> {
        self.require_kernel_owner(lease)?;
        let root = self.options.root().clone().with_lease(lease.clone());
        crate::execute_invocation_frame(
            request,
            root,
            self.options.application().clone(),
            (self.options.limits(), self.options.budget()),
        )
    }

    fn require_kernel_owner(&self, lease: &RootLease) -> io::Result<()> {
        if !lease.kernel_enforced() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "invocations require actual kernel advisory-lock admission",
            ));
        }
        if lease.root() != self.options.root().root() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "acquired lease differs from the admitted invocation root",
            ));
        }
        lease
            .assert_owned()
            .map_err(|error| Self::storage_error(&error))
    }

    fn prepare_request(&self, frame: &[u8]) -> io::Result<Value> {
        let payload = self.require_single_frame(frame)?;
        let mut request = serde_json::from_slice::<StrictValue>(payload)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
            .into_inner();
        self.require_host_configuration(&request)?;
        Self::require_bounded_validation(&request)?;
        Self::require_supported_scope(&request)?;
        Self::require_retry_safe_create(&request)?;
        // A verified alias uses the canonical private session default. This
        // also avoids acquiring a second native lease for its textual alias.
        if let Some(object) = request.as_object_mut() {
            object.remove("root");
        }
        Ok(request)
    }

    fn require_single_frame<'a>(&self, frame: &'a [u8]) -> io::Result<&'a [u8]> {
        let Some((&delimiter, payload)) = frame.split_last() else {
            return Err(Self::invalid_frame());
        };
        if delimiter != b'\n' || payload.is_empty() || payload.contains(&b'\n') {
            return Err(Self::invalid_frame());
        }
        if payload.len() > self.options.limits().max_request_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invocation request exceeds the configured frame limit",
            ));
        }
        Ok(payload)
    }

    fn require_host_configuration(&self, request: &Value) -> io::Result<()> {
        if request.get("schemaDir").is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invocations cannot override host schema configuration",
            ));
        }
        if let Some(root) = request.get("root") {
            let root = root.as_str().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invocation root override must be a canonical alias string",
                )
            })?;
            if fylo_vfs::canonicalize(root)? != self.options.root().root() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invocations cannot override the admitted physical root",
                ));
            }
        }
        Ok(())
    }

    fn require_supported_scope(request: &Value) -> io::Result<()> {
        if matches!(
            request.get("op").and_then(Value::as_str),
            Some(
                "log"
                    | "commit"
                    | "checkout"
                    | "branch"
                    | "status"
                    | "diff"
                    | "restoreCommit"
                    | "merge"
            )
        ) {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this invocation slice excludes version/repository operations whose additional read sets are not qualified",
            ));
        }
        Ok(())
    }

    /// An invocation can fail after its mutation committed (an exhausted budget
    /// or an oversized response), so callers must be able to retry. A create
    /// with a caller-chosen identifier is safe to retry: a repeat is refused
    /// as already existing and nothing is duplicated. A server-allocated
    /// identifier would create a second record on every retry.
    fn require_retry_safe_create(request: &Value) -> io::Result<()> {
        if request.get("op").and_then(Value::as_str) == Some("putData")
            && request.get("id").is_none()
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invocation creates need a caller-chosen id so a retry cannot create a duplicate",
            ));
        }
        Ok(())
    }

    fn require_bounded_validation(request: &Value) -> io::Result<()> {
        if request.get("op").and_then(Value::as_str) == Some("schemaValidate") {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "this invocation slice excludes external schema validation until helper deadlines and checked descendant cleanup are qualified",
            ));
        }
        Ok(())
    }

    fn invalid_frame() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "invocations require exactly one nonempty LF-delimited frame",
        )
    }

    fn storage_error(error: &fylo_storage_native::NativeStorageError) -> io::Error {
        io::Error::other(error.to_string())
    }
}
