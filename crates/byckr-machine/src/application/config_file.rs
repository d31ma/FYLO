use std::fs::{File, Metadata};
use std::io::{Error, ErrorKind, Read};
use std::path::Path;

pub(super) struct ConfigFile;

impl ConfigFile {
    pub(super) const MAX_BYTES: u64 = 1_048_576;

    pub(super) fn read(path: &Path, optional: bool) -> Result<Option<String>, String> {
        let metadata = std::fs::metadata(path).map(Some).or_else(|error| {
            Self::missing(
                &error,
                optional,
                "cannot inspect configuration or secrets file",
            )
        })?;
        metadata.as_ref().map(Self::require_file).transpose()?;
        let file = metadata
            .map(|_| Self::open(path, optional))
            .transpose()?
            .flatten();
        file.map(Self::read_open_file).transpose()
    }

    fn missing<T>(error: &Error, optional: bool, message: &str) -> Result<Option<T>, String> {
        match (optional, error.kind()) {
            (true, ErrorKind::NotFound) => Ok(None),
            _ => Err(message.into()),
        }
    }

    fn open(path: &Path, optional: bool) -> Result<Option<File>, String> {
        File::open(path).map(Some).or_else(|error| {
            Self::missing(
                &error,
                optional,
                "cannot read configuration or secrets file",
            )
        })
    }

    fn require_file(metadata: &Metadata) -> Result<(), String> {
        if !metadata.is_file() {
            return Err("configuration and secrets must be regular files".into());
        }
        Ok(())
    }

    fn read_open_file(file: File) -> Result<String, String> {
        let metadata = file
            .metadata()
            .map_err(|_| "cannot inspect configuration file")?;
        Self::require_file(&metadata)?;
        let mut bytes = Vec::new();
        file.take(Self::MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read configuration file")?;
        Self::require_size(bytes.len())?;
        let source = String::from_utf8(bytes).map_err(|_| "configuration file must be UTF-8")?;
        Ok(source
            .strip_prefix('\u{feff}')
            .unwrap_or(&source)
            .to_owned())
    }

    fn require_size(length: usize) -> Result<(), String> {
        if length as u64 > Self::MAX_BYTES {
            return Err("configuration file exceeds 1 MiB".into());
        }
        Ok(())
    }
}
