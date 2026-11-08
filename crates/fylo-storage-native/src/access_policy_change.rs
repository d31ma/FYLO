use serde::Serialize;

/// What changed in a policy version, and when, as the admin who signed the
/// version states it (ADR 0021 section 7): the audit record of grants,
/// revocations, rekeys, catalog-key rotations and their settling. Version 1 has none.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AccessPolicyChange {
    /// One of the kinds below, such as [`Self::GRANT`].
    pub kind: String,
    /// The rule a grant added or a revocation removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    /// The collection a rekey of one collection rewrote.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub collection: Option<String>,
    /// Milliseconds since the Unix epoch, by the signer's clock.
    pub at: u64,
}

impl AccessPolicyChange {
    /// `fylo access grant` added a rule.
    pub const GRANT: &'static str = "grant";
    /// `fylo access revoke` removed a rule.
    pub const REVOKE: &'static str = "revoke";
    /// `fylo rekey` rewrote the database under fresh keys.
    pub const REKEY: &'static str = "rekey";
    /// `fylo rekey --collection` rewrote one collection under a new key.
    pub const REKEY_COLLECTION: &'static str = "rekeyCollection";
    /// `fylo access settle`, or an admin's first write, finished a
    /// catalog-key rotation a revocation left pending.
    pub const SETTLE: &'static str = "settle";
    const KEYS: [&'static str; 4] = ["kind", "rule", "collection", "at"];

    /// A change of `kind` made now.
    #[must_use]
    pub fn now(kind: &str) -> AccessPolicyChange {
        let at = crate::wall_clock()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            });
        AccessPolicyChange {
            kind: kind.to_owned(),
            rule: None,
            collection: None,
            at,
        }
    }

    /// This change, naming the rule `rule`.
    #[must_use]
    pub fn with_rule(mut self, rule: &str) -> AccessPolicyChange {
        self.rule = Some(rule.to_owned());
        self
    }

    /// This change, naming the collection `collection`.
    #[must_use]
    pub fn with_collection(mut self, collection: &str) -> AccessPolicyChange {
        self.collection = Some(collection.to_owned());
        self
    }

    /// Whether this change rewrote every record of the database, so that
    /// each record is signed again under this version.
    #[must_use]
    pub fn rewrote_records(&self) -> bool {
        self.kind == Self::REKEY || self.kind == Self::REKEY_COLLECTION
    }

    /// A change from the policy's JSON.
    ///
    /// # Errors
    /// The first field that is missing, of the wrong type, or unknown.
    pub fn from_value(value: &serde_json::Value) -> Result<AccessPolicyChange, String> {
        use crate::access_json::AccessJson;
        let map = AccessJson::object(value, "a policy change")?;
        AccessJson::known_keys(map, &Self::KEYS, "a policy change")?;
        Ok(AccessPolicyChange {
            kind: AccessJson::string(map, "kind")?,
            rule: AccessJson::optional_string(map, "rule")?,
            collection: AccessJson::optional_string(map, "collection")?,
            at: AccessJson::number(map, "at")?,
        })
    }
}
