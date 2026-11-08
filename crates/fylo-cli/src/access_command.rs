use std::path::Path;
use std::sync::Arc;

use fylo_identity::PublicIdentity;
use fylo_machine::ApplicationConfig;
use fylo_storage_native::{
    AccessAction, AccessGrantRequest, AccessPolicy, AccessRule, DatabaseAccess, NativeRoot,
    NativeWriteRoot, RootAccessReport, RootOpenOptions,
};
use serde_json::{Value, json};

/// `fylo access report <root|db.vdbx>` and `fylo access list|grant|revoke
/// <db.vdbx>` (ADR 0021):
/// - `report` lists what a directory root or a database uses today (the
///   POSTIX access descriptors its records carry, its encryption) and what
///   `fylo pack --protect` would change; it reads every record and writes
///   nothing, and opens the root with the configured key and identity, if
///   any.
///
/// The other three run as the configured identity:
/// - `list` prints what the identity may do, the trust anchor that created
///   the database, and the newest policy;
/// - `audit` lists every policy version, oldest first: the admin who signed
///   it, what it changed (a grant, a revocation, a rekey) and when, and the
///   rules it added and removed (ADR 0021 section 7). It writes nothing,
///   and any identity the policy names may run it;
/// - `grant --grantee <name> --key <fpk1:...> --actions <read,write,admin>`
///   with `--collection <name>`, `--all-collections` or `--database` adds a
///   rule (`--rule <id>`, default `<grantee>-<resource>`) as the next policy
///   version, and wraps the keys it covers for the grantee;
/// - `revoke --rule <id>` removes a rule as the next policy version, and
///   starts a new key epoch for every collection it covered;
/// - `settle` finishes a catalog-key rotation a revocation left pending
///   (ADR 0021 section 7): it reseals the directory under the current
///   catalog key, drops the previous key, and records a signed `settle`
///   version. With nothing pending it says so and writes nothing; it is
///   safe to run again after a stop.
///
/// `grant`, `revoke` and `settle` need an admin identity.
pub struct AccessCommand;

impl AccessCommand {
    const COMMANDS: [&'static str; 6] = ["report", "list", "grant", "revoke", "audit", "settle"];

    const USAGE: &'static str = "Usage:\n  fylo access report <root|db.vdbx>\n  fylo access list <db.vdbx>\n  fylo access audit <db.vdbx>\n  fylo access grant <db.vdbx> --grantee <name> --key <fpk1:...> --actions <read|write|admin>[,...] (--collection <name> | --all-collections | --database) [--rule <id>] [--label <text>]\n  fylo access revoke <db.vdbx> --rule <id>\n  fylo access settle <db.vdbx>\nEach runs as --identity <reference> with --trust-anchor <fid1:...> or --trust-on-first-use.";

    /// Run `fylo access` with all its arguments and its positional
    /// arguments after `access`.
    ///
    /// # Errors
    /// The usage text for unknown arguments, `ENOIDENTITY` without an
    /// identity, the open's refusal, `EACCES` for a change by an identity
    /// that is not an admin, `EPOLICY` for a refused rule, or a database
    /// that is not protected.
    pub fn run(
        arguments: &[String],
        positionals: &[String],
        application: &ApplicationConfig,
    ) -> Result<String, String> {
        let (command, path) = Self::command(positionals)?;
        let options = application.with_database_access(RootOpenOptions::new(path));
        let rendered = match command {
            "report" => Self::report(options)?,
            "list" => Self::list(options, path)?,
            "audit" => Self::audit(options, path)?,
            "settle" => Self::settle(&Self::writer(options)?.settle_access(), path)?,
            "grant" => {
                Self::policy(&Self::writer(options)?.grant_access(&Self::request(arguments)?))?
            }
            _ => Self::policy(
                &Self::writer(options)?.revoke_access(Self::required(arguments, "--rule")?),
            )?,
        };
        serde_json::to_string_pretty(&rendered).map_err(|error| error.to_string())
    }

    fn command(positionals: &[String]) -> Result<(&str, &Path), String> {
        match positionals {
            [command, path] if Self::COMMANDS.contains(&command.as_str()) => {
                Ok((command.as_str(), Path::new(path)))
            }
            _ => Err(Self::USAGE.to_owned()),
        }
    }

    fn list(options: RootOpenOptions, path: &Path) -> Result<Value, String> {
        let root = NativeRoot::open_profiled(options).map_err(|error| error.to_string())?;
        let access = Self::access(&root, path)?;
        Ok(Self::render(&access))
    }

    fn audit(options: RootOpenOptions, path: &Path) -> Result<Value, String> {
        let root = NativeRoot::open_profiled(options).map_err(|error| error.to_string())?;
        let access = Self::access(&root, path)?;
        Ok(json!({
            "database": path.display().to_string(),
            "trustAnchor": access.anchor().as_str(),
            "entries": access.audit(),
        }))
    }

    fn settle(
        outcome: &Result<Option<u64>, fylo_storage_native::NativeStorageError>,
        path: &Path,
    ) -> Result<Value, String> {
        let since = outcome.as_ref().map_err(ToString::to_string)?;
        Ok(json!({
            "database": path.display().to_string(),
            "settled": since.is_some(),
            "pendingSince": since,
            "message": Self::settle_message(*since),
        }))
    }

    fn settle_message(since: Option<u64>) -> String {
        since.map_or_else(
            || "no catalog rotation is pending: nothing was written".to_owned(),
            |version| {
                format!(
                    "settled the catalog rotation pending since policy version {version}: the \
                     directory is sealed under the current catalog key only"
                )
            },
        )
    }

    fn report(options: RootOpenOptions) -> Result<Value, String> {
        let root = NativeRoot::open_profiled(options).map_err(|error| error.to_string())?;
        let report = RootAccessReport::of(&root).map_err(|error| error.to_string())?;
        serde_json::to_value(report).map_err(|error| error.to_string())
    }

    fn writer(options: RootOpenOptions) -> Result<NativeWriteRoot, String> {
        NativeWriteRoot::open_profiled(options).map_err(|error| error.to_string())
    }

    fn policy(
        outcome: &Result<AccessPolicy, fylo_storage_native::NativeStorageError>,
    ) -> Result<Value, String> {
        match outcome {
            Ok(policy) => Ok(json!({ "policy": policy })),
            Err(error) => Err(error.to_string()),
        }
    }

    fn request(arguments: &[String]) -> Result<AccessGrantRequest, String> {
        let grantee = Self::required(arguments, "--grantee")?;
        let keys = Self::keys(Self::required(arguments, "--key")?)?;
        let actions = Self::actions(Self::required(arguments, "--actions")?)?;
        let resources = Self::resources(arguments)?;
        let id = crate::option(arguments, "--rule").map_or_else(
            || format!("{grantee}-{}", resources.join(",")),
            ToOwned::to_owned,
        );
        Ok(AccessGrantRequest {
            keys,
            label: crate::option(arguments, "--label").map(ToOwned::to_owned),
            rule: AccessRule {
                id,
                identity: grantee.to_owned(),
                actions,
                resources,
            },
        })
    }

    pub(crate) fn keys(text: &str) -> Result<Vec<PublicIdentity>, String> {
        text.split(',')
            .map(|key| PublicIdentity::parse(key.trim()).map_err(|error| error.to_string()))
            .collect()
    }

    pub(crate) fn actions(text: &str) -> Result<Vec<AccessAction>, String> {
        text.split(',')
            .map(|action| Self::action(action.trim()))
            .collect()
    }

    fn action(text: &str) -> Result<AccessAction, String> {
        match text {
            "read" => Ok(AccessAction::Read),
            "write" => Ok(AccessAction::Write),
            "admin" => Ok(AccessAction::Admin),
            _ => Err(format!(
                "{text:?} is not an action: use read, write or admin"
            )),
        }
    }

    fn resources(arguments: &[String]) -> Result<Vec<String>, String> {
        let collection =
            crate::option(arguments, "--collection").map(AccessRule::collection_resource);
        let every = Self::flag(arguments, "--all-collections")
            .then(|| AccessRule::collection_resource("*"));
        let database = Self::flag(arguments, "--database").then(AccessRule::database_resource);
        let resources: Vec<String> = [collection, every, database]
            .into_iter()
            .flatten()
            .collect();
        match resources.len() {
            1 => Ok(resources),
            _ => Err(
                "grant takes exactly one of --collection <name>, --all-collections and --database"
                    .into(),
            ),
        }
    }

    fn flag(arguments: &[String], name: &str) -> bool {
        arguments.iter().any(|argument| argument == name)
    }

    fn required<'a>(arguments: &'a [String], name: &str) -> Result<&'a str, String> {
        crate::option(arguments, name).ok_or_else(|| format!("{name} is required\n{}", Self::USAGE))
    }

    fn access(root: &NativeRoot, path: &Path) -> Result<Arc<DatabaseAccess>, String> {
        match root.database_access() {
            Some(Some(access)) => Ok(access),
            Some(None) => Err(fylo_storage_native::DatabaseAccessError::NoIdentity.to_string()),
            None => Err(format!(
                "ENATIVE_UNSUPPORTED: {} is not a protected .vdbx database",
                path.display()
            )),
        }
    }

    fn render(access: &DatabaseAccess) -> Value {
        let grants = access.grants();
        json!({
            "identity": access.identity().as_str(),
            "trustAnchor": access.anchor().as_str(),
            "trustedOnFirstUse": access.trusted_on_first_use(),
            "admin": grants.is_admin(),
            "grants": grants.collections(),
            "policy": access.policy(),
        })
    }
}
