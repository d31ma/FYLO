use fylo_identity::PublicIdentity;

use crate::AccessRule;

/// A grant `fylo access grant` asks for (ADR 0021): add `rule`, naming the
/// policy identity `rule.identity`, and give that identity `keys` (added to
/// the keys it already has).
#[derive(Clone, Debug)]
pub struct AccessGrantRequest {
    /// The keys of the identity the rule names.
    pub keys: Vec<PublicIdentity>,
    /// A label for people, kept when the identity is new.
    pub label: Option<String>,
    /// The rule to add; its id must be new.
    pub rule: AccessRule,
}
