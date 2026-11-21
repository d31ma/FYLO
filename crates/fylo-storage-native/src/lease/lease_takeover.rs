use super::lease_claims::LeaseClaims;
use super::lease_observation::LeaseObservation;
use super::lease_record::LeaseRecord;
use super::lease_timing::LeaseTiming;
use crate::{NativeStorageError, NativeStorageErrorCode};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Taking the next generation of a lease once this process holds the kernel
/// lock (ADR 0033 section 4, "Taking a namespace").
///
/// The record is claimed at once when it is missing, released, written by a
/// binary before the layout release, or names a process on this machine
/// that has ended. In every other case the owner may be alive on another
/// machine, whose kernel lock this machine cannot see: the record must then
/// stay exactly the same for the whole takeover time, measured on this
/// process's own clock, before it is claimed.
pub(super) struct LeaseTakeover<'a> {
    pub(super) record_path: &'a Path,
    pub(super) claims: &'a LeaseClaims,
    pub(super) canonical: &'a Path,
    pub(super) owned_name: &'a str,
    pub(super) owner_id: &'a str,
    pub(super) timing: LeaseTiming,
    pub(super) deadline: Option<Instant>,
}

impl LeaseTakeover<'_> {
    const LONGEST_POLL: Duration = Duration::from_millis(250);
    const POLLS_PER_TAKEOVER: u32 = 8;

    /// # Errors
    ///
    /// `RootLocked` when the record changed while this process waited (its
    /// owner is alive), when the deadline came first, or when another
    /// process claimed the generation first; I/O errors otherwise.
    pub(super) fn claim(&self) -> Result<LeaseRecord, NativeStorageError> {
        let first = LeaseObservation::read(self.record_path);
        let stale = self.claims.present();
        let waits = Self::must_wait(&first, &stale);
        self.wait_when(waits, &first)?;
        let generation = LeaseClaims::next_after(Self::base_generation(&first, &stale), &stale);
        self.claims.claim(generation)?;
        crate::write::failpoint("lease-after-claim")?;
        self.require_unchanged(&first)?;
        let record = LeaseRecord::claim(self.owned_name, self.owner_id, generation, self.timing);
        record.persist(self.record_path)?;
        crate::write::failpoint("lease-after-record")?;
        self.claims.remove_before(generation);
        Ok(record)
    }

    fn must_wait(first: &LeaseObservation, stale: &BTreeSet<u64>) -> bool {
        let base = Self::base_generation(first, stale);
        let claim_pending = stale.range(base.saturating_add(1)..).next().is_some();
        let free = first.free_now() || first.owner_died_here();
        claim_pending || !free
    }

    fn base_generation(first: &LeaseObservation, stale: &BTreeSet<u64>) -> u64 {
        first
            .generation()
            .unwrap_or_else(|| LeaseClaims::newest(stale))
    }

    fn wait_when(&self, waits: bool, first: &LeaseObservation) -> Result<(), NativeStorageError> {
        if !waits {
            return Ok(());
        }
        let takeover = self.timing.takeover_against(first.recorded_takeover_millis());
        let ends = Self::first_seen(self.record_path, first) + takeover;
        self.watch_until(ends, first, takeover)
    }

    fn watch_until(
        &self,
        ends: Instant,
        first: &LeaseObservation,
        takeover: Duration,
    ) -> Result<(), NativeStorageError> {
        let poll = (takeover / Self::POLLS_PER_TAKEOVER).min(Self::LONGEST_POLL);
        while let Some(remaining) = ends.checked_duration_since(Instant::now()) {
            self.require_time_left()?;
            std::thread::sleep(poll.min(remaining));
            self.require_unchanged(first)?;
        }
        self.require_unchanged(first)
    }

    fn require_time_left(&self) -> Result<(), NativeStorageError> {
        let passed = self.deadline.is_some_and(|deadline| Instant::now() >= deadline);
        if passed {
            return Err(self.locked(&LeaseObservation::read(self.record_path)));
        }
        Ok(())
    }

    fn require_unchanged(&self, first: &LeaseObservation) -> Result<(), NativeStorageError> {
        let now = LeaseObservation::read(self.record_path);
        if now != *first {
            return Err(self.locked(&now));
        }
        Ok(())
    }

    fn locked(&self, observation: &LeaseObservation) -> NativeStorageError {
        NativeStorageError::new(
            NativeStorageErrorCode::RootLocked,
            observation.owner_message(self.canonical),
        )
    }

    /// When this process first saw exactly these record bytes. A process
    /// that tries again and again with short deadlines (a server taking one
    /// request at a time) still takes over once the record has stood still
    /// for the takeover time across all its tries.
    fn first_seen(path: &Path, observation: &LeaseObservation) -> Instant {
        type SeenRecords = HashMap<PathBuf, (LeaseObservation, Instant)>;
        static SEEN: OnceLock<Mutex<SeenRecords>> = OnceLock::new();
        let mut seen = SEEN
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = seen
            .entry(path.to_path_buf())
            .or_insert_with(|| (observation.clone(), Instant::now()));
        Self::restart_when_changed(entry, observation);
        entry.1
    }

    fn restart_when_changed(entry: &mut (LeaseObservation, Instant), observation: &LeaseObservation) {
        if entry.0 != *observation {
            *entry = (observation.clone(), Instant::now());
        }
    }
}
