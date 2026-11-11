use std::collections::BTreeSet;
use std::string::FromUtf8Error;

use fylo_query::{
    IndexKeyRange, IndexSnapshot, NumericRange, QueryLimits, equality_candidate_prefixes,
    match_terms, normalize_field_path, word_prefix,
};
use serde_json::{Map, Value};

use crate::encryption::is_encrypted_field;
use crate::{EngineError, EngineErrorCode};

/// One `$ops` entry's candidates: for each field the index can answer, the
/// union of its key ranges, intersected across those fields.
///
/// `$eq` reads the field's equality keys and every numeric key its value
/// coerces to. `$gt`, `$gte`, `$lt` and `$lte` read one run of the field's
/// sortable numeric keys. Other operators, and fields the index cannot
/// answer, are left to the predicate.
/// What the planner knows about a collection's fields: which are encrypted,
/// and which have complete word keys.
pub(super) struct FieldRules<'a> {
    pub(super) encrypted: &'a [String],
    pub(super) words: &'a [String],
}

pub(super) struct IndexOperation {
    fields: Vec<Vec<IndexKeyRange>>,
    /// The top-level field of every path the index is asked about.
    names: Vec<String>,
}

impl IndexOperation {
    pub(super) fn new(operation: &Map<String, Value>, rules: &FieldRules<'_>) -> Option<Self> {
        let fields = Self::fields(operation, rules);
        let names = operation
            .keys()
            .filter_map(|field| {
                Self::normalize_path(field)
                    .split('/')
                    .next()
                    .map(str::to_owned)
            })
            .collect();
        Self::nonempty(fields).map(|operation| Self { names, ..operation })
    }

    /// The top-level fields this operation reads from the index.
    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.names.iter().map(String::as_str)
    }

    /// Every key range the operation reads.
    pub(super) fn key_ranges(&self) -> impl Iterator<Item = &IndexKeyRange> {
        self.fields.iter().flatten()
    }

    pub(super) fn scan(&self, snapshot: &IndexSnapshot) -> Result<BTreeSet<String>, EngineError> {
        let candidates = self.intersect_fields(snapshot)?;
        Ok(candidates.unwrap_or_default())
    }

    fn fields(operation: &Map<String, Value>, rules: &FieldRules<'_>) -> Vec<Vec<IndexKeyRange>> {
        let mut fields = Vec::new();
        for (field, operand) in operation {
            fields.extend(Self::field_ranges(field, operand, rules.encrypted));
            fields.extend(Self::word_ranges(field, operand, rules.words));
        }
        fields
    }

    /// One run of word keys per `$match` term, each intersected with the
    /// others, for a field whose word keys are complete.
    fn word_ranges(field: &str, operand: &Value, words: &[String]) -> Vec<Vec<IndexKeyRange>> {
        let path = Self::normalize_path(field);
        let Some(expected) = Self::word_operand(&path, operand, words) else {
            return Vec::new();
        };
        let terms = match_terms(expected).unwrap_or_default();
        terms
            .iter()
            .map(|term| vec![IndexKeyRange::prefix(&word_prefix(&path, term))])
            .collect()
    }

    fn word_operand<'v>(path: &str, operand: &'v Value, words: &[String]) -> Option<&'v Value> {
        if !words.iter().any(|word| word == path) {
            return None;
        }
        operand.as_object()?.get("$match")
    }

    fn field_ranges(
        field: &str,
        operand: &Value,
        encrypted: &[String],
    ) -> Option<Vec<IndexKeyRange>> {
        let path = Self::normalize_path(field);
        Self::require_plaintext(&path, encrypted)?;
        let operators = operand.as_object()?;
        Self::equality_ranges(&path, operators).or_else(|| Self::numeric_ranges(&path, operators))
    }

    fn equality_ranges(path: &str, operators: &Map<String, Value>) -> Option<Vec<IndexKeyRange>> {
        let expected = operators.get("$eq")?;
        let prefixes = equality_candidate_prefixes(path, expected)?;
        Some(
            prefixes
                .iter()
                .map(|prefix| IndexKeyRange::prefix(prefix))
                .collect(),
        )
    }

    fn numeric_ranges(path: &str, operators: &Map<String, Value>) -> Option<Vec<IndexKeyRange>> {
        let range = NumericRange::from_operators(operators)?;
        Some(vec![range.key_range(path)])
    }

    fn normalize_path(field: &str) -> String {
        normalize_field_path(field)
    }

    fn require_plaintext(path: &str, encrypted: &[String]) -> Option<()> {
        if is_encrypted_field(path, encrypted) {
            return None;
        }
        Some(())
    }

    fn nonempty(fields: Vec<Vec<IndexKeyRange>>) -> Option<Self> {
        if fields.is_empty() {
            return None;
        }
        Some(Self {
            fields,
            names: Vec::new(),
        })
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
        ranges: &[IndexKeyRange],
    ) -> Result<BTreeSet<String>, EngineError> {
        let mut matched = BTreeSet::new();
        for range in ranges {
            Self::scan_range(snapshot, range, &mut matched)?;
        }
        Ok(matched)
    }

    fn scan_range(
        snapshot: &IndexSnapshot,
        range: &IndexKeyRange,
        matched: &mut BTreeSet<String>,
    ) -> Result<(), EngineError> {
        let identifiers = snapshot
            .scan_range(range, QueryLimits::default())
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
