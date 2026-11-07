use std::collections::BTreeMap;

use fylo_identity::{IdentityId, PublicIdentity};
use serde::{Deserialize, Serialize};

use crate::{
    AccessAction, AccessGrantRequest, AccessPolicyIdentity, AccessRule, DatabaseAccessError,
};

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

    /// The next version: this one with `request`'s rule added, and its
    /// keys added to the identity the rule names.
    ///
    /// # Errors
    /// `EPOLICY` for a rule id already in use, a rule with no action or no
    /// resource, or a resource this format does not know.
    pub fn with_grant(
        &self,
        request: &AccessGrantRequest,
        previous: String,
    ) -> Result<AccessPolicy, DatabaseAccessError> {
        Self::require_valid_rule(&request.rule)?;
        self.require_new_rule(&request.rule.id)?;
        let mut next = self.next(previous);
        let identity = next
            .identities
            .entry(request.rule.identity.clone())
            .or_insert_with(|| AccessPolicyIdentity {
                keys: Vec::new(),
                label: request.label.clone(),
            });
        let added = request.keys.iter().map(ToString::to_string);
        identity.keys.extend(added);
        identity.keys.sort();
        identity.keys.dedup();
        next.rules.push(request.rule.clone());
        Ok(next)
    }

    /// The next version: this one without the rule `id`, and without the
    /// identities no rule names any more. Returns it and the removed rule.
    ///
    /// # Errors
    /// `EPOLICY` when no rule has that id, or when removing it would leave
    /// the database with no admin.
    pub fn without_rule(
        &self,
        id: &str,
        previous: String,
    ) -> Result<(AccessPolicy, AccessRule), DatabaseAccessError> {
        let mut next = self.next(previous);
        let position = next
            .rules
            .iter()
            .position(|rule| rule.id == id)
            .ok_or_else(|| Self::refused(&format!("the policy has no rule {id:?}")))?;
        let removed = next.rules.remove(position);
        next.drop_unnamed_identities();
        next.require_admin()?;
        Ok((next, removed))
    }

    fn next(&self, previous: String) -> AccessPolicy {
        AccessPolicy {
            version: self.version + 1,
            previous: Some(previous),
            ..self.clone()
        }
    }

    fn drop_unnamed_identities(&mut self) {
        let named: Vec<String> = self
            .rules
            .iter()
            .map(|rule| rule.identity.clone())
            .collect();
        self.identities.retain(|name, _| named.contains(name));
    }

    fn require_admin(&self) -> Result<(), DatabaseAccessError> {
        Some(self.admin_keys())
            .filter(|admins| !admins.is_empty())
            .map(|_| ())
            .ok_or_else(|| Self::refused("a policy keeps at least one admin of the database"))
    }

    fn require_new_rule(&self, id: &str) -> Result<(), DatabaseAccessError> {
        Some(id)
            .filter(|id| self.rules.iter().all(|rule| rule.id != *id))
            .map(|_| ())
            .ok_or_else(|| Self::refused(&format!("the policy already has a rule {id:?}")))
    }

    fn require_valid_rule(rule: &AccessRule) -> Result<(), DatabaseAccessError> {
        Some(rule)
            .filter(|rule| !rule.id.is_empty() && !rule.identity.is_empty())
            .filter(|rule| !rule.actions.is_empty() && !rule.resources.is_empty())
            .filter(|rule| rule.resources.iter().all(|resource| AccessRule::is_resource(resource)))
            .map(|_| ())
            .ok_or_else(|| {
                Self::refused(
                    "a rule needs an id, an identity, actions, and resources among database, collection:* and collection:<name>",
                )
            })
    }

    fn refused(message: &str) -> DatabaseAccessError {
        DatabaseAccessError::Policy(message.to_owned())
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
