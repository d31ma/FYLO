use std::path::{Path, PathBuf};

use fylo_identity::{Identity, IdentityId};
use serde_json::Value;

use crate::{AccessTrust, Error, OpenOptions};

/// The identity a protected `.vdbx` database opens with when a Turso SDK
/// opens it through `fylo link` (ADR 0021, Q9): Turso's open call carries
/// only a path, so it comes from `FYLO_IDENTITY` and `FYLO_TRUST_ANCHOR`,
/// or else from `fylo.identity` and `fylo.trustAnchor` in the
/// `application.json` beside the database.
pub struct LinkIdentity;

impl LinkIdentity {
    const IDENTITY_VARIABLE: &'static str = "FYLO_IDENTITY";
    const TRUST_ANCHOR_VARIABLE: &'static str = "FYLO_TRUST_ANCHOR";
    const CONFIG_FILE: &'static str = "application.json";

    /// The options a hook opens the root at `path` with: the identity and
    /// the pinned anchor named, or none when no identity is named. An
    /// identity without an anchor is refused by the database (`EPOLICY`).
    ///
    /// # Errors
    ///
    /// Fails for an unreadable or malformed `application.json`, a key
    /// reference that does not open, or an anchor that is not an
    /// identifier.
    pub fn open_options(path: &Path) -> Result<OpenOptions, Error> {
        let file = Self::settings_file(path)?;
        let reference = Self::setting(Self::IDENTITY_VARIABLE, file.as_ref(), "identity")?;
        let anchor = Self::setting(Self::TRUST_ANCHOR_VARIABLE, file.as_ref(), "trustAnchor")?;
        let Some(reference) = reference else {
            return Ok(OpenOptions::default());
        };
        let identity = Identity::from_reference(&reference).map_err(Self::identity_error)?;
        let trust = Self::trust(anchor.as_deref())?;
        Ok(OpenOptions::default().with_identity(identity, trust))
    }

    fn settings_file(path: &Path) -> Result<Option<Value>, Error> {
        let file = Self::config_path(path);
        match fylo_vfs::read(&file) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| Error::new("EBADREQUEST", format!("{}: {error}", file.display()))),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::new(
                "ENATIVE_IO",
                format!("{}: {error}", file.display()),
            )),
        }
    }

    fn config_path(path: &Path) -> PathBuf {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .join(Self::CONFIG_FILE)
    }

    fn setting(variable: &str, file: Option<&Value>, key: &str) -> Result<Option<String>, Error> {
        if let Ok(value) = std::env::var(variable) {
            return Ok(Some(value));
        }
        let Some(value) = file.and_then(|file| file.pointer(&format!("/fylo/{key}"))) else {
            return Ok(None);
        };
        value
            .as_str()
            .map(|text| Some(text.to_owned()))
            .ok_or_else(|| Error::new("EBADREQUEST", format!("fylo.{key} must be a string")))
    }

    fn trust(anchor: Option<&str>) -> Result<AccessTrust, Error> {
        let Some(anchor) = anchor else {
            return Ok(AccessTrust::Unpinned);
        };
        IdentityId::parse(anchor)
            .map(AccessTrust::Pinned)
            .map_err(Self::identity_error)
    }

    #[allow(clippy::needless_pass_by_value)]
    fn identity_error(error: fylo_identity::IdentityError) -> Error {
        Error::new(error.code(), error.to_string())
    }
}
