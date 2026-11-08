use std::collections::BTreeMap;

use fylo_identity::IdentityId;

use crate::{AccessAction, AccessPolicy};

/// What one identity may do under a policy: admin of the database or not,
/// and the strongest action on each collection it may touch at all.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AccessGrants {
    admin: bool,
    collections: BTreeMap<String, AccessAction>,
}

impl AccessGrants {
    /// The grants `policy` gives the key `id` on the collections `names`.
    #[must_use]
    pub fn resolve(policy: &AccessPolicy, id: &IdentityId, names: &[&str]) -> AccessGrants {
        let rules = policy.rules_for(id);
        let admin = rules
            .iter()
            .any(|rule| rule.allows_on_database(AccessAction::Admin));
        let collections = Self::strongest_by_name(&rules, names);
        AccessGrants { admin, collections }
    }

    fn strongest_by_name(
        rules: &[&crate::AccessRule],
        names: &[&str],
    ) -> BTreeMap<String, AccessAction> {
        let mut collections = BTreeMap::new();
        for name in names {
            Self::insert_strongest(&mut collections, rules, name);
        }
        collections
    }

    fn insert_strongest(
        collections: &mut BTreeMap<String, AccessAction>,
        rules: &[&crate::AccessRule],
        name: &str,
    ) {
        if let Some(action) = Self::strongest(rules, name) {
            collections.insert(name.to_owned(), action);
        }
    }

    /// Whether any rule names the identity.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        !self.admin && self.collections.is_empty()
    }

    /// Whether the identity is an admin of the database.
    #[must_use]
    pub fn is_admin(&self) -> bool {
        self.admin
    }

    /// The strongest action on the collection `name`, if any.
    #[must_use]
    pub fn on_collection(&self, name: &str) -> Option<AccessAction> {
        self.collections.get(name).copied()
    }

    /// Every collection the identity may touch, with its strongest action.
    #[must_use]
    pub fn collections(&self) -> &BTreeMap<String, AccessAction> {
        &self.collections
    }

    fn strongest(rules: &[&crate::AccessRule], name: &str) -> Option<AccessAction> {
        rules
            .iter()
            .filter_map(|rule| rule.strongest_on_collection(name))
            .max()
    }
}
