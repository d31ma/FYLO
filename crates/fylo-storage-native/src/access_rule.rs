use serde::{Deserialize, Serialize};

use crate::AccessAction;

/// One allow rule of a policy: `identity` may take `actions` on
/// `resources` (`database`, `collection:*` or `collection:<name>`).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessRule {
    /// The rule's name, which `fylo access revoke` takes.
    pub id: String,
    /// The policy identity the rule grants to.
    pub identity: String,
    /// What it allows.
    pub actions: Vec<AccessAction>,
    /// What it allows it on.
    pub resources: Vec<String>,
}

impl AccessRule {
    const DATABASE: &'static str = "database";
    const EVERY_COLLECTION: &'static str = "collection:*";
    const COLLECTION_PREFIX: &'static str = "collection:";

    /// The resource that names the whole database.
    #[must_use]
    pub fn database_resource() -> String {
        Self::DATABASE.to_owned()
    }

    /// The resource that names the collection `name`.
    #[must_use]
    pub fn collection_resource(name: &str) -> String {
        format!("{}{name}", Self::COLLECTION_PREFIX)
    }

    /// Whether the rule allows `action` on the whole database.
    #[must_use]
    pub fn allows_on_database(&self, action: AccessAction) -> bool {
        self.actions.contains(&action) && self.names_resource(Self::DATABASE)
    }

    /// The strongest action the rule allows on the collection `name`, if
    /// it covers it at all.
    #[must_use]
    pub fn strongest_on_collection(&self, name: &str) -> Option<AccessAction> {
        self.covers_collection(name)
            .then(|| self.strongest_action())
    }

    fn covers_collection(&self, name: &str) -> bool {
        self.resources.iter().any(|resource| {
            resource == Self::DATABASE
                || resource == Self::EVERY_COLLECTION
                || resource.strip_prefix(Self::COLLECTION_PREFIX) == Some(name)
        })
    }

    fn names_resource(&self, wanted: &str) -> bool {
        self.resources.iter().any(|resource| resource == wanted)
    }

    fn strongest_action(&self) -> AccessAction {
        self.actions
            .iter()
            .copied()
            .max()
            .unwrap_or(AccessAction::Read)
    }
}
