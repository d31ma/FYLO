use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use p256::SecretKey;
use zeroize::Zeroizing;

use crate::identity_error::IdentityError;
use crate::key_store::KeyStore;
use crate::private_key_pem::PrivateKeyPem;

/// `creds:<name>`: a `systemd-creds` credential.
///
/// `save` encrypts the PKCS#8 PEM key with `systemd-creds encrypt` (TPM2 or
/// the host key) into `/etc/credstore.encrypted/<name>`, which needs root. A
/// unit then names it with `LoadCredentialEncrypted=<name>`, and systemd
/// hands the decrypted key to that unit alone in `$CREDENTIALS_DIRECTORY`,
/// where `load` reads it.
pub(crate) struct CredentialKeyStore {
    name: String,
}

impl CredentialKeyStore {
    const DIRECTORY_VARIABLE: &'static str = "CREDENTIALS_DIRECTORY";
    const ENCRYPTED_STORE: &'static str = "/etc/credstore.encrypted";
    const TOOL: &'static str = "systemd-creds";

    pub(crate) fn new(name: String) -> CredentialKeyStore {
        CredentialKeyStore { name }
    }

    fn reference(&self) -> String {
        format!("creds:{}", self.name)
    }

    fn credential_path(&self) -> Result<PathBuf, IdentityError> {
        env::var_os(Self::DIRECTORY_VARIABLE)
            .map(|directory| Path::new(&directory).join(&self.name))
            .ok_or_else(|| {
                IdentityError::NotFound(format!(
                    "{}: {} is not set; run under a systemd unit with LoadCredentialEncrypted={}",
                    self.reference(),
                    Self::DIRECTORY_VARIABLE,
                    self.name
                ))
            })
    }

    fn encrypted_path(&self) -> PathBuf {
        Path::new(Self::ENCRYPTED_STORE).join(&self.name)
    }

    fn refuse_existing(&self, path: &Path) -> Result<(), IdentityError> {
        Some(path)
            .filter(|path| !path.exists())
            .map(|_| ())
            .ok_or_else(|| {
                IdentityError::AlreadyExists(format!(
                    "{} already exists at {}; FYLO never replaces a key",
                    self.reference(),
                    path.display()
                ))
            })
    }

    fn encrypt(&self, text: &str, output: &Path) -> io::Result<bool> {
        let mut child = Command::new(Self::TOOL)
            .arg("encrypt")
            .arg(format!("--name={}", self.name))
            .arg("-")
            .arg(output)
            .stdin(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("systemd-creds has no standard input"))?
            .write_all(text.as_bytes())?;
        Ok(child.wait()?.success())
    }

    fn io_error(&self, error: &io::Error) -> IdentityError {
        match error.kind() {
            io::ErrorKind::NotFound => {
                IdentityError::NotFound(format!("{}: {error}", self.reference()))
            }
            _ => IdentityError::Store(format!("{}: {error}", self.reference())),
        }
    }

    fn require_success(&self, succeeded: bool) -> Result<(), IdentityError> {
        Some(succeeded)
            .filter(|succeeded| *succeeded)
            .map(|_| ())
            .ok_or_else(|| {
                IdentityError::Store(format!(
                    "{}: systemd-creds encrypt failed",
                    self.reference()
                ))
            })
    }
}

impl KeyStore for CredentialKeyStore {
    fn save(&self, secret_key: &SecretKey) -> Result<(), IdentityError> {
        let output = self.encrypted_path();
        self.refuse_existing(&output)?;
        let text = PrivateKeyPem::encode(secret_key)?;
        let succeeded = self
            .encrypt(&text, &output)
            .map_err(|error| self.io_error(&error))?;
        self.require_success(succeeded)
    }

    fn load(&self) -> Result<SecretKey, IdentityError> {
        let path = self.credential_path()?;
        let text = fs::read_to_string(&path)
            .map(Zeroizing::new)
            .map_err(|error| self.io_error(&error))?;
        PrivateKeyPem::decode(&text, &self.reference())
    }
}
