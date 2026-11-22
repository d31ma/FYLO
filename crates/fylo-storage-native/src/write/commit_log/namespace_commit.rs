use std::collections::BTreeSet;

use fylo_format::{LogRecord, LogRecordKind};

use super::super::GenerationRecord;
use super::super::{
    CollectionWrites, DocumentCodec, DocumentPolicy, NativeWriteRoot, failpoint, json_error,
};
use super::abort_manifest::AbortManifest;
use super::commit_manifest::CommitManifest;
use super::commit_plan::CommitPlan;
use super::commit_queue::{BusyPeriod, CommitQueue, ExclusiveGuard};
use super::log_origin::LogOrigin;
use super::logged_plan::LoggedPlan;
use super::namespace_entry::NamespaceEntry;
use super::namespace_log::NamespaceLog;
use super::namespace_manifest::NamespaceManifest;
use super::namespace_member::NamespaceMember;
use super::namespace_part::NamespacePart;
use super::placed_record::PlacedRecord;
use crate::{CollectionKind, NativeCollection, NativeStorageError, NativeStorageErrorCode};

/// Transactions over several collections of one namespace (ADR 0035,
/// Amendment on several collections).
///
/// The collections are locked in name order, so two transactions can never
/// wait for each other. The commit is one record in the namespace log and
/// one flush. Each collection then applies its part and gets a copy of it in
/// its own log without a flush: the namespace record keeps it durable until
/// a later flush or checkpoint of that log does.
impl NativeWriteRoot {
    const NAMESPACE_BEFORE_SYNC: &'static str = "before-namespace-log-sync";
    const NAMESPACE_AFTER_SYNC: &'static str = "after-namespace-log-sync";
    const NAMESPACE_APPLIED_PART: &'static str = "after-namespace-apply-part";
    const NAMESPACE_BEFORE_COPIES: &'static str = "before-namespace-collection-append";

    /// Commit writes to several document collections of this namespace as
    /// one transaction: after a crash, recovery applies all of them or
    /// none. Each body is built through `codec`, as a single put or replace
    /// builds it. A transaction that writes one collection is exactly
    /// [`Self::commit_document_writes_with`].
    ///
    /// # Errors
    ///
    /// `InvalidCollection` when a collection appears twice; `Unsupported`
    /// for a raw-file collection, a segment collection, or one without the
    /// commit log; otherwise as [`Self::commit_document_writes_with`].
    /// Nothing is written when it fails.
    pub fn commit_collection_writes_with(
        &self,
        batches: &[CollectionWrites],
        codec: &dyn DocumentCodec,
    ) -> Result<(), NativeStorageError> {
        self.root.assert_profile_owned()?;
        let ordered = Self::ordered_batches(batches)?;
        match ordered.as_slice() {
            [] => Ok(()),
            [(_, only)] => self.commit_document_writes_with(&only.collection, &only.writes, codec),
            _ => self.commit_namespace_batches(&ordered, codec),
        }
    }

    /// The batches that write something, in collection-name order, each
    /// with its position in the request.
    fn ordered_batches(
        batches: &[CollectionWrites],
    ) -> Result<Vec<(usize, &CollectionWrites)>, NativeStorageError> {
        let mut names = BTreeSet::new();
        let mut ordered = Vec::with_capacity(batches.len());
        for (position, batch) in batches.iter().enumerate() {
            if !names.insert(batch.collection.as_str()) {
                return Err(NativeStorageError::new(
                    NativeStorageErrorCode::InvalidCollection,
                    format!(
                        "a transaction names collection {} more than once",
                        batch.collection
                    ),
                ));
            }
            if !batch.writes.is_empty() {
                ordered.push((position, batch));
            }
        }
        ordered.sort_by(|left, right| left.1.collection.cmp(&right.1.collection));
        Ok(ordered)
    }

    fn commit_namespace_batches(
        &self,
        ordered: &[(usize, &CollectionWrites)],
        codec: &dyn DocumentCodec,
    ) -> Result<(), NativeStorageError> {
        let mut collections = Vec::with_capacity(ordered.len());
        for (_, batch) in ordered {
            collections.push(self.namespace_collection(batch)?);
        }
        let policies: Vec<DocumentPolicy<'_>> = collections
            .iter()
            .map(|collection| self.policy(collection, codec))
            .collect();
        let outcome = Self::namespace_members(ordered, collections, &policies)
            .and_then(|members| self.commit_namespace(members));
        Self::finish_policies(policies, outcome)
    }

    fn namespace_members<'a>(
        ordered: &[(usize, &'a CollectionWrites)],
        collections: Vec<NativeCollection>,
        policies: &[DocumentPolicy<'_>],
    ) -> Result<Vec<NamespaceMember<'a>>, NativeStorageError> {
        let mut members = Vec::with_capacity(ordered.len());
        for ((&(position, batch), collection), policy) in
            ordered.iter().zip(collections).zip(policies)
        {
            let bodies = Self::batch_bodies(policy, &batch.collection, &batch.writes)?;
            members.push(NamespaceMember {
                position,
                collection,
                writes: &batch.writes,
                bodies,
            });
        }
        Ok(members)
    }

    fn finish_policies(
        policies: Vec<DocumentPolicy<'_>>,
        outcome: Result<(), NativeStorageError>,
    ) -> Result<(), NativeStorageError> {
        let mut outcome = outcome;
        for policy in policies {
            outcome = policy.finish(outcome);
        }
        outcome
    }

    /// The document collection a batch writes, if it can join a
    /// transaction over several collections.
    fn namespace_collection(
        &self,
        batch: &CollectionWrites,
    ) -> Result<NativeCollection, NativeStorageError> {
        Self::check_batch_identifiers(&batch.writes)?;
        let collection = self.writable_collection(&batch.collection)?;
        let joins = collection.kind == CollectionKind::Document
            && self.uses_log(&collection)
            && !collection.uses_segments();
        if joins {
            return Ok(collection);
        }
        Err(NativeStorageError::new(
            NativeStorageErrorCode::Unsupported,
            format!(
                "collection {} cannot join a transaction over several collections: only document collections with the commit log can",
                batch.collection
            ),
        ))
    }

    /// `members` arrive in collection-name order, which is the lock order.
    /// Everything after the locks runs in request order, so collections are
    /// applied and become `stable` in the order of their first write. A
    /// namespace log past its checkpoint limits is settled afterwards, with
    /// no lock held.
    fn commit_namespace(
        &self,
        members: Vec<NamespaceMember<'_>>,
    ) -> Result<(), NativeStorageError> {
        self.settle_namespace_once()?;
        self.commit_namespace_locked(members)?;
        if NamespaceLog::shared(self.path()).wants_checkpoint() {
            self.recover_namespace()?;
        }
        Ok(())
    }

    fn commit_namespace_locked(
        &self,
        members: Vec<NamespaceMember<'_>>,
    ) -> Result<(), NativeStorageError> {
        let _guards = self.exclusive_in_order(&members)?;
        let mut periods = Vec::with_capacity(members.len());
        for member in &members {
            periods.push(self.begin_busy(&member.collection)?);
        }
        let (members, mut periods) = Self::in_request_order(members, periods);
        let outcome = self.commit_locked(&members, &mut periods);
        let ended = Self::end_periods(&members, periods);
        outcome.and(ended)
    }

    fn in_request_order(
        members: Vec<NamespaceMember<'_>>,
        periods: Vec<BusyPeriod>,
    ) -> (Vec<NamespaceMember<'_>>, Vec<BusyPeriod>) {
        let mut pairs: Vec<_> = members.into_iter().zip(periods).collect();
        pairs.sort_by_key(|(member, _)| member.position);
        pairs.into_iter().unzip()
    }

    /// Take every collection away from other writers of this process, in
    /// name order, and register each for the closing checkpoint.
    fn exclusive_in_order(
        &self,
        members: &[NamespaceMember<'_>],
    ) -> Result<Vec<ExclusiveGuard>, NativeStorageError> {
        let mut guards = Vec::with_capacity(members.len());
        for member in members {
            let queue = CommitQueue::shared(&member.collection.path);
            guards.push(queue.exclusive()?);
            queue.lock().origin = Some(LogOrigin::of(self, member.collection.name()));
        }
        Ok(guards)
    }

    fn commit_locked(
        &self,
        members: &[NamespaceMember<'_>],
        periods: &mut [BusyPeriod],
    ) -> Result<(), NativeStorageError> {
        let plans = self.namespace_plans(members)?;
        for (member, period) in members.iter().zip(periods.iter_mut()) {
            self.publish_namespace_writing(&member.collection, period)?;
        }
        let record = Self::namespace_record(members, periods, &plans)?;
        let log = NamespaceLog::shared(self.path());
        let lsn = log.append_durably(&self.root, record, self.config.durability)?;
        self.apply_namespace(members, periods, (&plans, lsn), &log)?;
        self.copy_into_collection_logs(members, periods, &plans)
    }

    fn namespace_plans(
        &self,
        members: &[NamespaceMember<'_>],
    ) -> Result<Vec<CommitPlan>, NativeStorageError> {
        let mut plans = Vec::with_capacity(members.len());
        let mut blob_bytes = 0;
        for member in members {
            let plan =
                self.plan_document_writes(&member.collection, member.writes, &member.bodies)?;
            plan.index_growth().require_room(&member.collection)?;
            blob_bytes += plan.blob_bytes();
            plans.push(plan);
        }
        Self::require_namespace_budget(blob_bytes)?;
        Ok(plans)
    }

    fn require_namespace_budget(blob_bytes: usize) -> Result<(), NativeStorageError> {
        if blob_bytes > Self::LOG_BLOB_BUDGET {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::FileTooLarge,
                Self::OVER_BUDGET,
            ));
        }
        Ok(())
    }

    /// Open the collection's log segment if it has none, and publish
    /// `writing`, so readers wait and recovery knows to look.
    fn publish_namespace_writing(
        &self,
        collection: &NativeCollection,
        period: &mut BusyPeriod,
    ) -> Result<(), NativeStorageError> {
        if period.segment.is_none() {
            period.segment = Some(Self::create_segment(
                &self.root,
                collection,
                period.generation,
            )?);
        }
        let segment = period.segment.as_ref().expect("segment opened above");
        let writing = GenerationRecord::log_writing(period.generation + 1, segment.position())
            .fenced(period.fence.is_present());
        Self::publish_quietly(collection, &writing)?;
        period.published_writing = true;
        Ok(())
    }

    fn namespace_record(
        members: &[NamespaceMember<'_>],
        periods: &[BusyPeriod],
        plans: &[CommitPlan],
    ) -> Result<NamespaceEntry<'static>, NativeStorageError> {
        let mut blobs = Vec::new();
        let mut parts = Vec::with_capacity(plans.len());
        for ((member, period), plan) in members.iter().zip(periods).zip(plans) {
            parts.push(NamespacePart {
                collection: member.collection.name().to_owned(),
                generation: period.generation + 2,
                blob_offset: blobs.len() as u64,
                blob_length: plan.blobs().len() as u64,
                commit: Self::commit_manifest_of(plan)?,
            });
            blobs.extend_from_slice(plan.blobs());
        }
        let manifest = serde_json::to_vec(&NamespaceManifest::new(parts))
            .map_err(|error| json_error(&error))?;
        Ok(NamespaceEntry {
            kind: LogRecordKind::Commit,
            manifest,
            blobs,
            failpoints: Some((Self::NAMESPACE_BEFORE_SYNC, Self::NAMESPACE_AFTER_SYNC)),
        })
    }

    fn commit_manifest_of(plan: &CommitPlan) -> Result<CommitManifest, NativeStorageError> {
        serde_json::from_slice(&plan.manifest()?).map_err(|error| json_error(&error))
    }

    /// Apply every part. When one fails, undo them all from their
    /// pre-images and abort the record; when that fails too, leave every
    /// collection `writing` so recovery completes the committed write.
    fn apply_namespace(
        &self,
        members: &[NamespaceMember<'_>],
        periods: &mut [BusyPeriod],
        (plans, lsn): (&[CommitPlan], u64),
        log: &NamespaceLog,
    ) -> Result<(), NativeStorageError> {
        let Err((failed, error, index_applied)) = self.apply_parts(members, plans, lsn) else {
            return Ok(());
        };
        let undone = self
            .undo_parts(members, &plans[..=failed], index_applied)
            .and_then(|()| self.abort_namespace_record(log, lsn));
        if let Err(rollback) = undone {
            Self::strand(periods);
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::Io,
                format!(
                    "write failed ({error}) and rollback failed ({rollback}); recovery will complete the committed write"
                ),
            ));
        }
        Err(error)
    }

    fn apply_parts(
        &self,
        members: &[NamespaceMember<'_>],
        plans: &[CommitPlan],
        lsn: u64,
    ) -> Result<(), (usize, NativeStorageError, bool)> {
        let placed = PlacedRecord {
            segment: 0,
            offset: 0,
            length: 0,
            lsn,
        };
        for (index, (member, plan)) in members.iter().zip(plans).enumerate() {
            plan.apply(&self.root, &member.collection, &placed)
                .map_err(|(error, index_applied)| (index, error, index_applied))?;
            failpoint(Self::NAMESPACE_APPLIED_PART).map_err(|error| (index, error, true))?;
        }
        Ok(())
    }

    /// Undo the applied parts in reverse order. Every part before the last
    /// one was applied in full; the last one says whether its index delta
    /// was appended.
    fn undo_parts(
        &self,
        members: &[NamespaceMember<'_>],
        plans: &[CommitPlan],
        last_index_applied: bool,
    ) -> Result<(), NativeStorageError> {
        let last = plans.len() - 1;
        for (index, (member, plan)) in members.iter().zip(plans).enumerate().rev() {
            let index_applied = index != last || last_index_applied;
            plan.undo(&self.root, &member.collection, index_applied)?;
        }
        Ok(())
    }

    fn abort_namespace_record(
        &self,
        log: &NamespaceLog,
        lsn: u64,
    ) -> Result<(), NativeStorageError> {
        let manifest =
            serde_json::to_vec(&AbortManifest::new(lsn)).map_err(|error| json_error(&error))?;
        let entry = NamespaceEntry {
            kind: LogRecordKind::Abort,
            manifest,
            blobs: Vec::new(),
            failpoints: None,
        };
        log.append_durably(&self.root, entry, self.config.durability)
            .map(drop)
    }

    fn strand(periods: &mut [BusyPeriod]) {
        for period in periods {
            period.stranded = true;
        }
    }

    /// Give each collection's log a copy of its part, so its checkpoint
    /// flushes the files the part wrote and its readers find the length
    /// `stable` records. A segment this transaction created is flushed
    /// once, so its name is durable before a later commit relies on it.
    fn copy_into_collection_logs(
        &self,
        members: &[NamespaceMember<'_>],
        periods: &mut [BusyPeriod],
        plans: &[CommitPlan],
    ) -> Result<(), NativeStorageError> {
        let copied = failpoint(Self::NAMESPACE_BEFORE_COPIES).and_then(|()| {
            for ((member, period), plan) in members.iter().zip(periods.iter_mut()).zip(plans) {
                self.copy_part(&member.collection, period, plan)?;
            }
            Ok(())
        });
        if copied.is_err() {
            Self::strand(periods);
        }
        copied
    }

    fn copy_part(
        &self,
        collection: &NativeCollection,
        period: &mut BusyPeriod,
        plan: &CommitPlan,
    ) -> Result<(), NativeStorageError> {
        let segment = period
            .segment
            .as_mut()
            .expect("writing was published with a segment");
        let record = LogRecord {
            lsn: segment.next_lsn(),
            kind: LogRecordKind::Commit,
            generation: period.generation + 2,
            manifest: plan.manifest()?,
            blobs: plan.blobs().to_vec(),
        };
        let encoded = segment.encode(&record)?;
        segment.append(&encoded, 1)?;
        if segment.is_fresh() {
            segment.flush_handle().run(self.config.durability)?;
            segment.mark_flushed();
        }
        Self::merge_schema_delta(collection, period, plan);
        Ok(())
    }

    fn merge_schema_delta(
        collection: &NativeCollection,
        period: &mut BusyPeriod,
        plan: &CommitPlan,
    ) {
        if !collection.tracks_schema_versions() {
            return;
        }
        match plan.schema_count_delta(collection) {
            Ok(delta) => period.schema_delta.merge(&delta),
            Err(_) => period.schema_counts = None,
        }
    }

    /// Publish `stable` in every collection (unless stranded), in request
    /// order: an application that writes its pointer (an outbox entry, a
    /// head row) last can rely on every collection it points into being
    /// `stable` or still `writing` once the pointer is readable. Returns the
    /// first failure.
    fn end_periods(
        members: &[NamespaceMember<'_>],
        periods: Vec<BusyPeriod>,
    ) -> Result<(), NativeStorageError> {
        let mut outcome = Ok(());
        for (member, period) in members.iter().zip(periods) {
            let ended = Self::end_busy(&member.collection, period);
            outcome = outcome.and(ended);
        }
        outcome
    }
}
