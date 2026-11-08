//! Validation in a build without a regex compiler (ADR 0017 section 4).
//!
//! The WebAssembly builds link only `regex-automata`'s DFA search, so they
//! cannot compile a CHEX schema. They validate a collection with the DFAs a
//! native push stored in its schema registry, loaded through `fylo-chex`'s
//! validating `from_bytes`, and give the same answers and messages as a
//! native build. Where there is nothing they can load, they fail closed: a
//! write under `strict` or `warn` is refused with `ESCHEMAUNAVAILABLE` rather
//! than stored unvalidated. Under `off` nothing is checked, so nothing is
//! refused.

use fylo_storage_native::ValidationMode;

use crate::{EngineError, WriteEngine};

impl WriteEngine {
    /// Refuse to validate `collection` when this build cannot.
    ///
    /// It can when the collection's registry head is portable and its stored
    /// validator loads. It cannot when the head is not portable, has no
    /// stored validator, or its stored bytes are refused.
    ///
    /// A collection without a registry can only have a schema in a
    /// `fylo.schema` folder, which would have to be compiled, so it is
    /// refused when the attached folder may hold one (a folder this build
    /// cannot inspect, as in a browser, counts as holding one) and when its
    /// descriptor records `strict` or `warn`, which only a collection meant
    /// to have a schema does. Without either it has no schema to validate,
    /// as natively.
    ///
    /// # Errors
    ///
    /// Returns `ESCHEMAUNAVAILABLE` naming the reason, or the error reading
    /// the registry's head.
    pub(crate) fn require_stored_validator(&self, collection: &str) -> Result<(), EngineError> {
        if let Some(head) = self.registry_head(collection)? {
            return head
                .stored_validator(collection)
                .map(drop)
                .map_err(EngineError::schema_unavailable);
        }
        let in_folder = self
            .schema
            .as_deref()
            .is_some_and(|schema| schema.may_have_schema(collection));
        // A collection that does not exist yet records nothing; a write to it
        // fails, or creates it, in storage.
        let recorded = self
            .writer
            .collection(collection)
            .ok()
            .and_then(|collection| collection.validation());
        if in_folder
            || matches!(
                recorded,
                Some(ValidationMode::Strict | ValidationMode::Warn)
            )
        {
            return Err(EngineError::schema_unavailable(format!(
                "collection {collection} has no schema registry, and a build without a regex \
                 compiler cannot compile a fylo.schema folder; push the schema into the \
                 registry with a native build (fylo schema push)"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "stored_validation_tests.rs"]
mod tests;
