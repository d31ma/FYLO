use serde::Serialize;

use crate::{AccessPolicy, AccessPolicyChange};

/// One policy version as `fylo access audit` lists it (ADR 0021 section 7):
/// who signed it, what it changed and when, as the signer stated it, and
/// the rules it added and removed against the version before.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AccessAuditEntry {
    /// The policy version.
    pub version: u64,
    /// The `fid1:` identifier of the admin whose signature it carries.
    pub signer: String,
    /// `create` for version 1, the change's kind (`grant`, `revoke`,
    /// `rekey`, `rekeyCollection`, `settle`), or `unrecorded` for a version written
    /// before changes were recorded.
    pub kind: String,
    /// The rule a grant added or a revocation removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The collection a rekey of one collection rewrote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Milliseconds since the Unix epoch, by the signer's clock.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub at: Option<u64>,
    /// Rule ids this version added.
    pub added: Vec<String>,
    /// Rule ids this version removed.
    pub removed: Vec<String>,
}

impl AccessAuditEntry {
    /// The kind of version 1.
    pub const CREATE: &'static str = "create";
    /// The kind of a later version that records no change.
    pub const UNRECORDED: &'static str = "unrecorded";

    /// The entry of `policy`, signed by `signer`, after `before`.
    pub(crate) fn of(
        policy: &AccessPolicy,
        signer: String,
        before: Option<&AccessPolicy>,
    ) -> AccessAuditEntry {
        let change = policy.change.as_ref();
        AccessAuditEntry {
            version: policy.version,
            signer,
            kind: Self::kind(change, before.is_none()),
            rule: change.and_then(|change| change.rule.clone()),
            collection: change.and_then(|change| change.collection.clone()),
            at: change.map(|change| change.at),
            added: Self::rules_missing_from(policy, before),
            removed: before.map_or_else(Vec::new, |before| {
                Self::rules_missing_from(before, Some(policy))
            }),
        }
    }

    fn kind(change: Option<&AccessPolicyChange>, first: bool) -> String {
        if first {
            return Self::CREATE.to_owned();
        }
        change.map_or_else(|| Self::UNRECORDED.to_owned(), |change| change.kind.clone())
    }

    /// The ids of the rules of `policy` that `other` does not have.
    fn rules_missing_from(policy: &AccessPolicy, other: Option<&AccessPolicy>) -> Vec<String> {
        let mut missing = Vec::new();
        for rule in &policy.rules {
            let held = other.is_some_and(|other| other.rules.iter().any(|kept| kept.id == rule.id));
            if !held {
                missing.push(rule.id.clone());
            }
        }
        missing
    }
}
