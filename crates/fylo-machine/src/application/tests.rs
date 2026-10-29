use super::ApplicationConfig;
use super::secret_declarations::SecretDeclarations;
use super::secret_reference::SecretReference;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

struct Fixture(PathBuf);

impl Fixture {
    const APPLICATION_FILE: &'static str = "application.json";
    const TOKEN_NAME: &'static str = "TOKEN";
    const MAXIMUM_SHARD_WIDTH: u32 = 4;
    const EMPTY_CONFIG: &'static str = "{}";
    const PADDING: &'static str = " ";

    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "fylo-application-unit-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn load(&self, source: &str) -> Result<ApplicationConfig, String> {
        let path = self.0.join(Self::APPLICATION_FILE);
        std::fs::write(&path, source).unwrap();
        ApplicationConfig::load(Some(&path))
    }

    fn error(&self, source: &str) -> String {
        self.load(source).err().expect("configuration must fail")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn secret_parser_keeps_literal_values() {
    let values = SecretDeclarations::parse(
        "export TOKEN=\"line\\nvalue\" # comment\nSALT='${NOT_EXPANDED}'\n",
    )
    .unwrap();
    assert_eq!(values[Fixture::TOKEN_NAME], "line\nvalue");
    assert_eq!(values["SALT"], "${NOT_EXPANDED}");
}

#[test]
fn duplicate_secret_names_are_rejected() {
    assert!(SecretDeclarations::parse("A=1\nA=2").is_err());
}

#[test]
fn invalid_secret_names_are_rejected() {
    assert!(SecretDeclarations::parse("BAD-NAME=value").is_err());
}

#[test]
fn text_after_double_quoted_secrets_is_rejected() {
    assert!(SecretDeclarations::parse("A=\"secret\" trailing").is_err());
}

#[test]
fn unfinished_single_quoted_secrets_are_rejected() {
    assert!(SecretDeclarations::parse("A='unterminated").is_err());
}

#[test]
fn whole_value_secret_references_are_accepted() {
    assert_eq!(
        SecretReference::parse("${TOKEN}").unwrap(),
        Some(Fixture::TOKEN_NAME)
    );
}

#[test]
fn command_substitution_is_literal_text() {
    assert_eq!(SecretReference::parse("$(command)").unwrap(), None);
}

#[test]
fn partial_value_secret_references_are_rejected() {
    assert!(SecretReference::parse("prefix-${TOKEN}").is_err());
}

#[test]
fn secret_reference_defaults_are_rejected() {
    assert!(SecretReference::parse("${TOKEN:-default}").is_err());
}

#[test]
fn joined_secret_references_are_rejected() {
    assert!(SecretReference::parse("${A}${B}").is_err());
}

#[test]
fn empty_secret_references_are_rejected() {
    assert!(SecretReference::parse("${}").is_err());
}

#[test]
fn unknown_settings_are_rejected_before_invalid_root() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"unknown":true,"root":1}}"#),
        "unknown key under fylo; use documented nested configuration keys"
    );
}

#[test]
fn invalid_root_is_rejected_before_invalid_schema() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"root":1,"schema":false}}"#),
        "fylo.root must be a string"
    );
}

#[test]
fn invalid_schema_is_rejected_before_invalid_strict_flag() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"schema":false,"strict":0}}"#),
        "fylo.schema must be a string or an object with a path"
    );
}

#[test]
fn schema_accepts_a_path_or_an_object_with_auto_push() {
    let fixture = Fixture::new();
    let plain = fixture.load(r#"{"fylo":{"schema":"schemas"}}"#).unwrap();
    assert_eq!(plain.schema, Some(fixture.0.join("schemas")));
    assert!(!plain.storage.schema_auto_push);
    let object = fixture
        .load(r#"{"fylo":{"schema":{"path":"schemas","autoPush":true}}}"#)
        .unwrap();
    assert_eq!(object.schema, Some(fixture.0.join("schemas")));
    assert!(object.storage.schema_auto_push);
    let default = fixture
        .load(r#"{"fylo":{"schema":{"path":"schemas"}}}"#)
        .unwrap();
    assert!(!default.storage.schema_auto_push);
    assert_eq!(
        fixture.error(r#"{"fylo":{"schema":{"autoPush":true}}}"#),
        "fylo.schema.path is required when fylo.schema is an object"
    );
    assert_eq!(
        fixture.error(r#"{"fylo":{"schema":{"path":"schemas","autoPush":"yes"}}}"#),
        "fylo.schema.autoPush must be a boolean"
    );
    assert_eq!(
        fixture.error(r#"{"fylo":{"schema":{"path":"schemas","push":true}}}"#),
        "unknown key under fylo.schema; use documented nested configuration keys"
    );
}

#[test]
fn invalid_strict_flag_is_rejected_before_invalid_shard() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"strict":0,"shard":null}}"#),
        "fylo.strict must be a boolean"
    );
}

#[test]
fn unknown_shard_setting_is_rejected_before_invalid_width() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"shard":{"bad":0,"width":0},"encryption":null}}"#),
        "unknown key under fylo.shard; use documented nested configuration keys"
    );
}

#[test]
fn invalid_shard_width_is_rejected_before_invalid_encryption() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"shard":{"width":0},"encryption":null}}"#),
        "fylo.shard.width must be an integer from 1 to 4"
    );
}

#[test]
fn literal_encryption_key_is_rejected_before_invalid_cipher() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"encryption":{"key":"literal"},"cipher":null}}"#),
        "fylo.encryption.key must use a ${NAME} secret reference, not a literal credential"
    );
}

#[test]
fn invalid_cipher_is_rejected_before_the_removed_chex_setting() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"cipher":null,"chex":null}}"#),
        "fylo.cipher must be an object"
    );
}

#[test]
fn removed_chex_setting_is_rejected_with_its_replacement() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.error(r#"{"fylo":{"chex":{"binary":"./chex"}}}"#),
        "fylo.chex is no longer supported; CHEX schema validation is built into FYLO, so remove the setting"
    );
}

#[test]
fn unused_secret_files_are_not_read_and_paths_remain_relative() {
    let fixture = Fixture::new();
    std::fs::create_dir(fixture.0.join(".env")).unwrap();
    let config = fixture.load(r#"{"other":{"root":1}}"#).unwrap();
    assert!(config.root.is_none());
    let config = fixture.load(r#"{"fylo":{"root":"../data","schema":"schema.json","strict":true,"shard":{"width":4}}}"#).unwrap();
    assert_eq!(config.root, Some(fixture.0.join("../data")));
    assert_eq!(config.schema, Some(fixture.0.join("schema.json")));
    assert!(config.storage.strict_schema);
    assert_eq!(
        config.storage.shard_width,
        Some(Fixture::MAXIMUM_SHARD_WIDTH)
    );
}

#[test]
fn file_limits_bom_utf8_and_regular_file_errors_are_stable() {
    let fixture = Fixture::new();
    assert!(fixture.load("\u{feff}{}").is_ok());
    let padding = Fixture::PADDING.repeat(
        usize::try_from(super::config_file::ConfigFile::MAX_BYTES).unwrap()
            - Fixture::EMPTY_CONFIG.len(),
    );
    let exact_limit = Fixture::EMPTY_CONFIG.to_owned() + &padding;
    assert!(fixture.load(&exact_limit).is_ok());
    assert_eq!(
        fixture.error(&(exact_limit + Fixture::PADDING)),
        "configuration file exceeds 1 MiB"
    );
    let path = fixture.0.join(Fixture::APPLICATION_FILE);
    std::fs::write(&path, [0xff]).unwrap();
    assert_eq!(
        ApplicationConfig::load(Some(&path)).err().unwrap(),
        "configuration file must be UTF-8"
    );
    assert_eq!(
        ApplicationConfig::load(Some(&fixture.0)).err().unwrap(),
        "configuration and secrets must be regular files"
    );
}

#[test]
fn quoted_secret_values_keep_escape_and_comment_rules() {
    let values = SecretDeclarations::parse(
        "A=\"a\\qb\\r\\t\\\"\\\\\" # tail\nB='a\\n${LITERAL}'\nC= a # comment\nD=\"é🙂\"\n",
    )
    .unwrap();
    assert_eq!(values["A"], "a\\qb\r\t\"\\");
    assert_eq!(values["B"], "a\\n${LITERAL}");
    assert_eq!(values["C"], "a");
    assert_eq!(values["D"], "é🙂");
}

#[test]
fn unfinished_escape_is_rejected() {
    assert!(SecretDeclarations::parse("A=\"ending\\").is_err());
}

#[test]
fn text_after_single_quoted_secrets_is_rejected() {
    assert!(SecretDeclarations::parse("A='quoted' trailing").is_err());
}

#[test]
fn duplicate_secret_after_comment_is_rejected() {
    assert!(SecretDeclarations::parse("A=1\n\n# comment\nA=2").is_err());
}

#[test]
fn errors_keep_original_line_numbers() {
    assert_eq!(
        SecretDeclarations::parse("# comment\n\nBAD-NAME=secret").unwrap_err(),
        "invalid .env secret declaration at line 3"
    );
}

#[test]
fn encryption_file_opts_into_encrypted_databases_with_the_key_alone() {
    let fixture = Fixture::new();
    // A key generated for this test, at the minimum length.
    std::fs::write(
        fixture.0.join(".env"),
        format!(
            "FYLO_UNIT_DATABASE_KEY={}\n",
            "generated-unit-key-".repeat(2)
        ),
    )
    .unwrap();
    let plain = fixture.load(r#"{"fylo":{"root":"db.vdbx"}}"#).unwrap();
    assert!(plain.database_key.is_none());
    assert!(!plain.encrypt_databases);
    assert!(plain.creation_key().is_none());

    let config = fixture
        .load(r#"{"fylo":{"encryption":{"key":"${FYLO_UNIT_DATABASE_KEY}","file":true}}}"#)
        .unwrap();
    assert!(config.encrypt_databases);
    assert!(config.creation_key().is_some());
    // A database key alone encrypts files, and no field.
    assert!(config.credentials.is_none());
    let unset =
        fixture.load(r#"{"fylo":{"encryption":{"key":"${FYLO_UNIT_DATABASE_KEY}","file":false}}}"#);
    assert!(unset.is_err(), "a key without file still needs its salt");
}

#[test]
fn encryption_file_errors_are_stable() {
    let fixture = Fixture::new();
    std::fs::write(fixture.0.join(".env"), "FYLO_UNIT_SHORT_KEY=short\n").unwrap();
    assert_eq!(
        fixture.error(r#"{"fylo":{"encryption":{"file":"yes"}}}"#),
        "fylo.encryption.file must be a boolean"
    );
    assert_eq!(
        fixture.error(r#"{"fylo":{"encryption":{"file":true}}}"#),
        "fylo.encryption.file requires fylo.encryption.key, the secret a database's key is derived from"
    );
    assert!(
        fixture
            .error(r#"{"fylo":{"encryption":{"key":"${FYLO_UNIT_SHORT_KEY}","file":true}}}"#)
            .contains("32")
    );
    assert_eq!(
        fixture.error(r#"{"fylo":{"encryption":{"files":true}}}"#),
        "unknown key under fylo.encryption; use documented nested configuration keys"
    );
}

#[test]
fn a_new_database_key_is_named_by_a_secret_reference_as_the_configured_one_is() {
    let fixture = Fixture::new();
    let config = fixture.0.join(Fixture::APPLICATION_FILE);
    // Keys generated for this test: one at the minimum length, one short.
    std::fs::write(
        fixture.0.join(".env"),
        format!(
            "FYLO_UNIT_NEXT_KEY={}\nFYLO_UNIT_SHORT_NEXT_KEY=short\n",
            "generated-next-key-".repeat(2)
        ),
    )
    .unwrap();
    let resolve = |reference: &str| {
        ApplicationConfig::database_key_reference(Some(&config), reference, "--new-key")
    };
    assert!(resolve("${FYLO_UNIT_NEXT_KEY}").is_ok());
    // A literal is refused without echoing it, as fylo.encryption.key is.
    let literal = "generated-literal-secret-of-forty-bytes!";
    let error = resolve(literal).unwrap_err();
    assert_eq!(
        error,
        "--new-key must use a ${NAME} secret reference, not a literal credential"
    );
    assert!(!error.contains(literal));
    assert_eq!(
        resolve("${FYLO_UNIT_SHORT_NEXT_KEY}").unwrap_err(),
        "--new-key must name a secret of at least 32 UTF-8 bytes"
    );
    assert_eq!(
        resolve("${FYLO_UNIT_ABSENT_NEXT_KEY}").unwrap_err(),
        "missing referenced secret FYLO_UNIT_ABSENT_NEXT_KEY"
    );
    assert!(resolve("prefix-${FYLO_UNIT_NEXT_KEY}").is_err());
}
