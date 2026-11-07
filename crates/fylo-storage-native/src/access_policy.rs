use std::collections::BTreeMap;

use fylo_identity::{IdentityId, PublicIdentity};
use serde::{Deserialize, Serialize};

use crate::{AccessAction, AccessPolicyIdentity, AccessRule};

/// A protected database's policy (ADR 0021): allow rules only, so nothing
/// is allowed without a rule. Each change is a new version that names the
/// one before by hash and is signed by an admin of it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessPolicy {
    /// Always [`AccessPolicy::FORMAT`].
    pub format: String,
    /// 1 for the policy a creation writes, then one more per change.
    pub version: u64,
    /// `sha256:<hex>` of the previous version's bytes; none for version 1.
    pub previous: Option<String>,
    /// The identities rules name, by policy name.
    pub identities: BTreeMap<String, AccessPolicyIdentity>,
    /// The allow rules.
    pub rules: Vec<AccessRule>,
}

impl AccessPolicy {
    /// The policy format this build reads and writes.
    pub const FORMAT: &'static str = "fylo.policy/1";
    /// The policy name of the identity a database is created with.
    pub const CREATOR: &'static str = "admin";
    /// The rule that makes the creator an admin of the database.
    pub const CREATOR_RULE: &'static str = "root-admin";

    /// Version 1: `admin` holds `admin` on the database.
    #[must_use]
    pub fn first(admin: &PublicIdentity) -> AccessPolicy {
        let identity = AccessPolicyIdentity {
            keys: vec![admin.to_string()],
            label: None,
        };
        AccessPolicy {
            format: Self::FORMAT.to_owned(),
            version: 1,
            previous: None,
            identities: BTreeMap::from([(Self::CREATOR.to_owned(), identity)]),
            rules: vec![AccessRule {
                id: Self::CREATOR_RULE.to_owned(),
                identity: Self::CREATOR.to_owned(),
                actions: vec![AccessAction::Admin],
                resources: vec![AccessRule::database_resource()],
            }],
        }
    }

    /// The public keys of every identity a rule names: everyone who gets
    /// the catalog key.
    #[must_use]
    pub fn granted_keys(&self) -> Vec<PublicIdentity> {
        self.keys_of_rules(|_| true)
    }

    /// The public keys of every identity with an admin rule on the
    /// database.
    #[must_use]
    pub fn admin_keys(&self) -> Vec<PublicIdentity> {
        self.keys_of_rules(|rule| rule.allows_on_database(AccessAction::Admin))
    }

    /// The public keys of every identity a rule covering the collection
    /// `name` names: everyone who gets its key.
    #[must_use]
    pub fn collection_keys(&self, name: &str) -> Vec<PublicIdentity> {
        self.keys_of_rules(|rule| rule.strongest_on_collection(name).is_some())
    }

    /// The rules that name an identity holding the key `id`.
    #[must_use]
    pub fn rules_for(&self, id: &IdentityId) -> Vec<&AccessRule> {
        let names = self.names_holding(id);
        self.rules
            .iter()
            .filter(|rule| names.contains(&rule.identity.as_str()))
            .collect()
    }

    fn names_holding(&self, id: &IdentityId) -> Vec<&str> {
        self.identities
            .iter()
            .filter(|(_, identity)| Self::parsed_keys(identity).iter().any(|key| key.id() == id))
            .map(|(name, _)| name.as_str())
            .collect()
    }

    fn keys_of_rules(&self, selected: impl Fn(&AccessRule) -> bool) -> Vec<PublicIdentity> {
        let mut keys: Vec<PublicIdentity> = self
            .rules
            .iter()
            .filter(|rule| selected(rule))
            .filter_map(|rule| self.identities.get(&rule.identity))
            .flat_map(Self::parsed_keys)
            .collect();
        keys.sort_by(|left, right| left.id().cmp(right.id()));
        keys.dedup_by(|left, right| left.id() == right.id());
        keys
    }

    fn parsed_keys(identity: &AccessPolicyIdentity) -> Vec<PublicIdentity> {
        identity
            .keys
            .iter()
            .filter_map(|key| PublicIdentity::parse(key).ok())
            .collect()
    }
}
