use std::io;
use std::time::Instant;

use fylo_storage_native::{NativeWriteRoot, RootLease};
use serde_json::{Value, json};

use crate::{InvocationOptions, StrictValue};

/// Exactly-one-frame native execution with checked teardown before response.
///
/// Only immutable host configuration survives between calls. Advisory root
/// ownership coordinates cooperating clients; it is not distributed fencing.
pub struct InvocationRuntime {
    options: InvocationOptions,
}

impl InvocationRuntime {
    const CAPABILITY_VERSION: u32 = 1;
    const COMMAND: &'static str = "exec --invocation";
    const OPTIONS: [&'static str; 3] = ["--request", "--deadline-ms", "--cleanup-reserve-ms"];
    const EXCLUDED_OPERATIONS: [&'static str; 8] = [
        "log",
        "commit",
        "checkout",
        "branch",
        "status",
        "diff",
        "restoreCommit",
        "merge",
    ];

    /// The handshake's `capabilities.invocation`: what a host needs to know
    /// before it starts `fylo exec --invocation` (ADR 0028).
    #[must_use]
    pub fn capability() -> Value {
        json!({
            "version": Self::CAPABILITY_VERSION,
            "command": Self::COMMAND,
            "options": Self::OPTIONS,
            "excludedOperations": Self::EXCLUDED_OPERATIONS,
            "createRequiresId": true,
            "waitsForRoot": true
        })
    }

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
        let settled = self.settle_logs();
        lease
            .release_checked()
            .map_err(|error| Self::storage_error(&error))?;
        self.options.check_completion_deadline()?;
        result.and_then(|response| settled.map(|()| response))
    }

    /// Checkpoint the write-ahead logs this process wrote under the root
    /// while the lease still owns it. A checkpoint takes the collection lock,
    /// so one taken after the release would meet the next owner's writes.
    fn settle_logs(&self) -> io::Result<()> {
        NativeWriteRoot::close_logs_under(self.options.root().root())
            .map_err(|error| Self::storage_error(&error))
    }

    /// Wait for a root another invocation holds, within this budget.
    ///
    /// Concurrent provider invocations are normal, and refusing them at once
    /// pushes every caller into its own retry loop. Waiting invocations take
    /// turns, so a stream of later invocations cannot keep one out. Waiting
    /// never extends past the cleanup reserve; when the budget runs out the
    /// lock error is returned and nothing was mutated.
    fn acquire_lease(&self) -> io::Result<RootLease> {
        let root = self.options.root();
        RootLease::acquire_waiting(root.root(), root.filesystem_profile(), self.wait_until())
            .map_err(|error| Self::storage_error(&error))
    }

    /// The moment the cleanup reserve starts. A budget already inside its
    /// reserve still gets one try of the root.
    fn wait_until(&self) -> Instant {
        let (deadline, reserve) = self.options.budget();
        deadline.checked_sub(reserve).unwrap_or_else(Instant::now)
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
        let operation = request
            .get("op")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if Self::EXCLUDED_OPERATIONS.contains(&operation) {
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
