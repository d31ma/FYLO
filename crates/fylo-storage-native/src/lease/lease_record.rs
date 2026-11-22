use super::lease_state::LeaseState;
use super::lease_timing::LeaseTiming;
use super::machine_identity::MachineIdentity;
use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_vfs::OpenOptions;
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;

/// The lease record `.<name>.fylo-root-owner.lock.json` beside an owned
/// folder (ADR 0033 section 4, `api/storage/v2/namespace-lease.schema.json`).
///
/// A process owns the folder while it holds the kernel lock on the lock file
/// and this record names its owner id and generation in the `held` state.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct LeaseRecord {
    format: String,
    namespace: String,
    owner_id: String,
    generation: u64,
    heartbeat: u64,
    state: LeaseState,
    heartbeat_millis: u64,
    takeover_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    process_start: Option<String>,
    host: String,
    pid: u32,
}

impl LeaseRecord {
    pub(super) const FORMAT: &str = "fylo.namespace-lease.v1";
    pub(super) const MAXIMUM_BYTES: u64 = 4096;

    pub(super) fn claim(
        namespace: &str,
        owner_id: &str,
        generation: u64,
        timing: LeaseTiming,
    ) -> Self {
        Self {
            format: Self::FORMAT.to_owned(),
            namespace: namespace.to_owned(),
            owner_id: owner_id.to_owned(),
            generation,
            heartbeat: 0,
            state: LeaseState::Held,
            heartbeat_millis: Self::millis(timing.heartbeat()),
            takeover_millis: Self::millis(timing.takeover()),
            machine: Self::eager_machine(),
            process_start: Self::eager_process_start(),
            host: Self::host_name(),
            pid: crate::process_id(),
        }
    }

    /// Whether the machine id and the process start are still missing:
    /// on Windows the claim leaves them to the heartbeat thread.
    pub(super) fn needs_identity(&self) -> bool {
        self.machine.is_none() || self.process_start.is_none()
    }

    /// The record with this machine's id and this process's start time.
    pub(super) fn identified(&self) -> Self {
        let mut next = self.clone();
        next.machine = MachineIdentity::current();
        next.process_start = crate::write::own_process_identity();
        next
    }

    /// Reading the process start takes a PowerShell call on Windows, which
    /// can take seconds on a busy machine. A claim must stay fast, so there
    /// the heartbeat thread adds both values right after the claim
    /// ([`LeaseRecord::identified`]). Until it has, a crash waits the
    /// takeover time.
    #[cfg(not(windows))]
    fn eager_machine() -> Option<String> {
        MachineIdentity::current()
    }

    #[cfg(windows)]
    fn eager_machine() -> Option<String> {
        None
    }

    #[cfg(not(windows))]
    fn eager_process_start() -> Option<String> {
        crate::write::own_process_identity()
    }

    #[cfg(windows)]
    fn eager_process_start() -> Option<String> {
        None
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn takeover_millis(&self) -> u64 {
        self.takeover_millis
    }

    pub(super) fn is_released(&self) -> bool {
        self.state == LeaseState::Released
    }

    pub(super) fn names(&self, owner_id: &str, generation: u64) -> bool {
        self.state == LeaseState::Held && self.owner_id == owner_id && self.generation == generation
    }

    /// This record, read back, still names the owner and generation of
    /// `mine` and is held.
    pub(super) fn same_owner(&self, mine: &Self) -> bool {
        self.names(&mine.owner_id, mine.generation)
    }

    /// The record of the next heartbeat.
    pub(super) fn beat(&self) -> Self {
        let mut next = self.clone();
        next.heartbeat = next.heartbeat.saturating_add(1);
        next
    }

    /// The record of a clean stop: the generation stays, so fencing stays
    /// sound, and the next owner claims at once.
    pub(super) fn released(&self) -> Self {
        let mut next = self.clone();
        next.state = LeaseState::Released;
        next
    }

    /// Whether the owner ran on this machine and its process is gone:
    /// its pid now names no process, or a process that started at another
    /// time. Unknown machines and unknown start times are never dead.
    pub(super) fn owner_died_here(&self) -> bool {
        let here = MachineIdentity::current();
        let same_machine = here.is_some() && here == self.machine;
        same_machine && self.process_ended()
    }

    pub(super) fn owner_message(&self, canonical: &Path) -> String {
        format!(
            "FYLO root already has a live exclusive owner (pid {} on {}): {}",
            self.pid,
            self.host,
            canonical.to_string_lossy()
        )
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        let record = serde_json::from_slice::<Self>(bytes).ok()?;
        (record.format == Self::FORMAT).then_some(record)
    }

    /// Write the record for other processes to see. Used by heartbeats,
    /// which need to be seen but not to survive a power cut.
    pub(super) fn publish(&self, path: &Path) -> Result<(), NativeStorageError> {
        let scratch = crate::write::sibling_scratch(path);
        let written = self.write_file(&scratch, false);
        Self::rename_or_clean(written, &scratch, path)
    }

    /// Write the record and flush it and its folder. Used for claims and
    /// clean stops, which change the generation or the state.
    pub(super) fn persist(&self, path: &Path) -> Result<(), NativeStorageError> {
        let scratch = crate::write::sibling_scratch(path);
        let written = self.write_file(&scratch, true);
        Self::rename_or_clean(written, &scratch, path)?;
        crate::write::sync_directory(Self::parent(path)?)
    }

    fn process_ended(&self) -> bool {
        let Some(expected) = &self.process_start else {
            return false;
        };
        crate::write::process_identity(self.pid).as_ref() != Some(expected)
    }

    fn write_file(&self, path: &Path, flush: bool) -> Result<(), NativeStorageError> {
        let encoded = self.encode()?;
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .map_err(NativeStorageError::io)?;
        file.write_all(&encoded).map_err(NativeStorageError::io)?;
        Self::flush_when(flush, &file)
    }

    fn flush_when(flush: bool, file: &fylo_vfs::File) -> Result<(), NativeStorageError> {
        if !flush {
            return Ok(());
        }
        crate::sync_handle(file).map_err(NativeStorageError::io)
    }

    fn rename_or_clean(
        written: Result<(), NativeStorageError>,
        scratch: &Path,
        path: &Path,
    ) -> Result<(), NativeStorageError> {
        let renamed =
            written.and_then(|()| fylo_vfs::rename(scratch, path).map_err(NativeStorageError::io));
        if renamed.is_err() {
            let _ = fylo_vfs::remove_file(scratch);
        }
        renamed
    }

    fn parent(path: &Path) -> Result<&Path, NativeStorageError> {
        path.parent().ok_or_else(|| {
            NativeStorageError::new(
                NativeStorageErrorCode::UnsafePath,
                "the lease record has no parent folder",
            )
        })
    }

    fn encode(&self) -> Result<Vec<u8>, NativeStorageError> {
        serde_json::to_vec(self).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::CorruptMetadata,
                format!("lease record cannot be encoded: {error}"),
            )
        })
    }

    fn millis(duration: std::time::Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    fn host_name() -> String {
        std::env::var("HOSTNAME")
            .ok()
            .or_else(|| std::env::var("COMPUTERNAME").ok())
            .unwrap_or_else(|| "unknown".into())
    }
}
