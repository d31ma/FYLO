use fylo_machine::DatabaseKey;

/// The key a page hands the engine for an encrypted `.vdbx` file (ADR 0025),
/// as the bytes `fylo_set_database_key` receives: one flags byte, then
/// either the secret itself or, for a key the page derived with `WebCrypto`,
/// the file's salt (32 bytes), its file key (32) and its key-check value
/// (16).
pub struct BrowserKey;

impl BrowserKey {
    const DERIVED: u8 = 1;
    const CREATE: u8 = 2;
    const KNOWN_FLAGS: u8 = Self::DERIVED | Self::CREATE;
    const MINIMUM_SECRET_BYTES: usize = 32;
    const SALT_BYTES: usize = 32;
    const FILE_KEY_BYTES: usize = 32;
    const CHECK_BYTES: usize = 16;
    const DERIVED_BYTES: usize = Self::SALT_BYTES + Self::FILE_KEY_BYTES + Self::CHECK_BYTES;

    /// The key `bytes` describe, and whether the page asked for a missing
    /// database to be created with it.
    ///
    /// # Errors
    /// Text saying why the bytes are not a key; it never quotes them.
    pub fn parse(bytes: &[u8]) -> Result<(DatabaseKey, bool), String> {
        let (flags, material) = bytes
            .split_first()
            .ok_or_else(|| "the encryption key is empty".to_owned())?;
        let flags = Self::known(*flags)?;
        let key = Self::key(flags & Self::DERIVED != 0, material)?;
        Ok((key, flags & Self::CREATE != 0))
    }

    fn known(flags: u8) -> Result<u8, String> {
        if flags & !Self::KNOWN_FLAGS == 0 {
            return Ok(flags);
        }
        Err("the encryption key has unknown flags".to_owned())
    }

    fn key(derived: bool, material: &[u8]) -> Result<DatabaseKey, String> {
        if derived {
            return Self::derived(material);
        }
        Self::secret(material)
    }

    fn secret(material: &[u8]) -> Result<DatabaseKey, String> {
        if material.len() < Self::MINIMUM_SECRET_BYTES {
            return Err(format!(
                "the encryption key must be at least {} bytes",
                Self::MINIMUM_SECRET_BYTES
            ));
        }
        Ok(DatabaseKey::new(material))
    }

    fn derived(material: &[u8]) -> Result<DatabaseKey, String> {
        if material.len() != Self::DERIVED_BYTES {
            return Err(
                "a derived encryption key must be a salt, a file key and a check".to_owned(),
            );
        }
        let (salt, rest) = material.split_at(Self::SALT_BYTES);
        let (file_key, check) = rest.split_at(Self::FILE_KEY_BYTES);
        Ok(DatabaseKey::derived(
            Self::array(salt),
            Self::array(file_key),
            Self::array(check),
        ))
    }

    fn array<const N: usize>(bytes: &[u8]) -> [u8; N] {
        let mut array = [0_u8; N];
        array.copy_from_slice(bytes);
        array
    }
}
