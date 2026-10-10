use std::collections::BTreeSet;
use std::string::FromUtf8Error;

use fylo_query::{IndexSnapshot, QueryLimits, ScanQuery, equality_candidate_prefixes};
use serde_json::{Map, Value};

use crate::encryption::is_encrypted_field;
use crate::{EngineError, EngineErrorCode};

pub(super) struct EqualityOperation {
    fields: Vec<Vec<String>>,
}

impl EqualityOperation {
    pub(super) fn new(operation: &Map<String, Value>, encrypted: &[String]) -> Option<Self> {
        let fields = Self::fields(operation, encrypted);
        Self::nonempty(fields)
    }

    pub(super) fn scan(&self, snapshot: &IndexSnapshot) -> Result<BTreeSet<String>, EngineError> {
        let candidates = self.intersect_fields(snapshot)?;
        Ok(candidates.unwrap_or_default())
    }

    fn fields(operation: &Map<String, Value>, encrypted: &[String]) -> Vec<Vec<String>> {
        let mut fields = Vec::new();
        for (field, operand) in operation {
            fields.extend(Self::prefixes(field, operand, encrypted));
        }
        fields
    }

    fn prefixes(field: &str, operand: &Value, encrypted: &[String]) -> Option<Vec<String>> {
        let path = Self::normalize_path(field);
        Self::require_plaintext(&path, encrypted)?;
        let expected = operand.as_object()?.get("$eq")?;
        equality_candidate_prefixes(&path, expected)
    }

    fn normalize_path(field: &str) -> String {
        field
            .split(['.', '/'])
            .filter(|segment| !segment.is_empty())
            .collect::<Vec<_>>()
            .join("/")
    }

    fn require_plaintext(path: &str, encrypted: &[String]) -> Option<()> {
        if is_encrypted_field(path, encrypted) {
            return None;
        }
        Some(())
    }

    fn nonempty(fields: Vec<Vec<String>>) -> Option<Self> {
        if fields.is_empty() {
            return None;
        }
        Some(Self { fields })
    }

    fn intersect_fields(
        &self,
        snapshot: &IndexSnapshot,
    ) -> Result<Option<BTreeSet<String>>, EngineError> {
        let mut candidates = None;
        for prefixes in &self.fields {
            let matched = Self::scan_field(snapshot, prefixes)?;
            candidates = Some(Self::intersection(candidates, matched));
        }
        Ok(candidates)
    }

    fn intersection(
        current: Option<BTreeSet<String>>,
        matched: BTreeSet<String>,
    ) -> BTreeSet<String> {
        match current {
            Some(current) => current.intersection(&matched).cloned().collect(),
            None => matched,
        }
    }

    fn scan_field(
        snapshot: &IndexSnapshot,
        prefixes: &[String],
    ) -> Result<BTreeSet<String>, EngineError> {
        let mut matched = BTreeSet::new();
        for prefix in prefixes {
            Self::scan_prefix(snapshot, prefix, &mut matched)?;
        }
        Ok(matched)
    }

    fn scan_prefix(
        snapshot: &IndexSnapshot,
        prefix: &str,
        matched: &mut BTreeSet<String>,
    ) -> Result<(), EngineError> {
        let identifiers = snapshot
            .scan(
                &[ScanQuery {
                    prefix: prefix.to_owned(),
                    range: None,
                }],
                QueryLimits::default(),
            )
            .map_err(EngineError::query)?;
        Self::insert_identifiers(matched, identifiers)
    }

    fn insert_identifiers(
        matched: &mut BTreeSet<String>,
        identifiers: Vec<Vec<u8>>,
    ) -> Result<(), EngineError> {
        for identifier in identifiers {
            matched.insert(Self::decode_identifier(identifier)?);
        }
        Ok(())
    }

    fn decode_identifier(identifier: Vec<u8>) -> Result<String, EngineError> {
        String::from_utf8(identifier).map_err(|error| Self::invalid_identifier(&error))
    }

    fn invalid_identifier(error: &FromUtf8Error) -> EngineError {
        EngineError::new(
            EngineErrorCode::CorruptData,
            format!("index document identifier is not UTF-8: {error}"),
        )
    }
}
