use fylo_identity::{Identity, IdentityError, KeyReference};
use serde_json::json;

/// `fylo identity create --store <reference> [--json]` and
/// `fylo identity show <reference> [--json]` (ADR 0021): make an application
/// identity's P-256 key in a key store, or print the identifier of one that
/// exists. The identifier, `fid1:p256:...`, is what a database policy
/// grants. `create` never replaces a key.
pub struct IdentityCommand;

type Operation = fn(&KeyReference) -> Result<Identity, IdentityError>;

impl IdentityCommand {
    const JSON_FLAG: &'static str = "--json";
    const USAGE: &'static str = "Usage:\n  fylo identity create --store <reference> [--json]\n  fylo identity show <reference> [--json]\n\nReferences: file:<path>, keychain:<label> (macOS), cng:<name> (Windows), creds:<name> (Linux services), webcrypto:<name> (browser)";

    /// Run `fylo identity` with the arguments after `identity`.
    ///
    /// # Errors
    /// The usage text for unknown arguments, or the identity error as
    /// `<code>: <message>`.
    pub fn run(arguments: &[String]) -> Result<String, String> {
        let json = arguments.iter().any(|argument| argument == Self::JSON_FLAG);
        let (operation, reference) = Self::operation(&Self::positionals(arguments))?;
        let identity = KeyReference::parse(&reference)
            .and_then(|reference| operation(&reference))
            .map_err(|error| error.to_string())?;
        Ok(Self::output(&identity, json))
    }

    fn positionals(arguments: &[String]) -> Vec<&str> {
        arguments
            .iter()
            .map(String::as_str)
            .filter(|argument| *argument != Self::JSON_FLAG)
            .collect()
    }

    fn operation(arguments: &[&str]) -> Result<(Operation, String), String> {
        match arguments {
            ["create", "--store", reference] => Ok((Identity::create, (*reference).to_owned())),
            ["show", reference] => Ok((Identity::open, (*reference).to_owned())),
            _ => Err(Self::USAGE.to_owned()),
        }
    }

    fn output(identity: &Identity, json: bool) -> String {
        if json {
            return json!({
                "id": identity.id().as_str(),
                "reference": identity.reference().to_string(),
            })
            .to_string();
        }
        identity.id().to_string()
    }
}
