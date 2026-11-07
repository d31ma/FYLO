use serde::Serialize;

use crate::{AccessKeyWrap, AccessPolicyEnvelope};

/// What a protected database's access slots hold (ADR 0021): every policy
/// version, oldest first, and the keyring. The wraps are readable by
/// anyone; each opens only with its recipient's private key.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct AccessDocument {
    /// Always [`AccessDocument::FORMAT`].
    pub format: String,
    /// The policy versions, oldest first.
    pub policies: Vec<AccessPolicyEnvelope>,
    /// The key wraps.
    pub keyring: Vec<AccessKeyWrap>,
}

impl AccessDocument {
    /// The document an access slot holds, from its JSON bytes.
    ///
    /// # Errors
    /// The first field that is missing, of the wrong type, or unknown.
    pub fn from_json(bytes: &[u8]) -> Result<AccessDocument, String> {
        use crate::access_json::AccessJson;
        let value: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let map = AccessJson::object(&value, "the access document")?;
        AccessJson::known_keys(
            map,
            &["format", "policies", "keyring"],
            "the access document",
        )?;
        let policies = AccessJson::array(map, "policies")?
            .iter()
            .map(Self::envelope)
            .collect::<Result<_, _>>()?;
        let keyring = AccessJson::array(map, "keyring")?
            .iter()
            .map(Self::wrap)
            .collect::<Result<_, _>>()?;
        Ok(AccessDocument {
            format: AccessJson::string(map, "format")?,
            policies,
            keyring,
        })
    }

    fn envelope(value: &serde_json::Value) -> Result<AccessPolicyEnvelope, String> {
        use crate::access_json::AccessJson;
        let map = AccessJson::object(value, "a policy envelope")?;
        AccessJson::known_keys(map, &["sealed", "signatures"], "a policy envelope")?;
        let signatures = AccessJson::array(map, "signatures")?
            .iter()
            .map(|signature| {
                let map = AccessJson::object(signature, "a signature")?;
                AccessJson::known_keys(map, &["key", "signature"], "a signature")?;
                Ok(crate::AccessSignature {
                    key: AccessJson::string(map, "key")?,
                    signature: AccessJson::string(map, "signature")?,
                })
            })
            .collect::<Result<_, String>>()?;
        Ok(AccessPolicyEnvelope {
            sealed: AccessJson::string(map, "sealed")?,
            signatures,
        })
    }

    fn wrap(value: &serde_json::Value) -> Result<AccessKeyWrap, String> {
        use crate::access_json::AccessJson;
        let map = AccessJson::object(value, "a key wrap")?;
        AccessJson::known_keys(
            map,
            &["recipient", "collection", "epoch", "wrap"],
            "a key wrap",
        )?;
        Ok(AccessKeyWrap {
            recipient: AccessJson::string(map, "recipient")?,
            collection: AccessJson::optional_u32(map, "collection")?,
            epoch: AccessJson::optional_u32(map, "epoch")?,
            wrap: AccessJson::string(map, "wrap")?,
        })
    }

    /// The access document format this build reads and writes.
    pub const FORMAT: &'static str = "fylo.access/1";

    /// The wraps addressed to `recipient`.
    pub fn wraps_for<'a>(&'a self, recipient: &'a str) -> impl Iterator<Item = &'a AccessKeyWrap> {
        self.keyring
            .iter()
            .filter(move |wrap| wrap.recipient == recipient)
    }
}
