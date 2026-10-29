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

use crate::{EngineError, WriteEngine};

impl WriteEngine {
    /// Refuse to validate `collection` when this build cannot.
    ///
    /// It can when the collection's registry head is portable and its stored
    /// validator loads, or when the collection has no schema at all. It
    /// cannot when the head is not portable, has no stored validator, or its
    /// stored bytes are refused, and it cannot when the collection's only
    /// schema is in a `fylo.schema` folder, which would have to be compiled.
    /// A folder this build cannot inspect, such as any folder in a browser,
    /// counts as holding a schema.
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
        match self.schema.as_deref() {
            Some(schema) if schema.may_have_schema(collection) => {
                Err(EngineError::schema_unavailable(format!(
                    "collection {collection} has no schema registry, and a build without a \
                     regex compiler cannot compile its fylo.schema folder; push the schema \
                     into the registry with a native build (fylo schema push)"
                )))
            }
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
#[path = "stored_validation_tests.rs"]
mod tests;
