//! The schema registry: a database's own record of its schema versions
//! (ADR 0017 section 3), which replaces reading `fylo.schema` on every use
//! once a collection has one. The folder stays the authoring layout, moved in
//! by `push` and back out by `pull`.

mod loaded_version;
mod registry_store;
mod registry_version;
mod schema_import;
mod schema_pull;
mod schema_push;
mod schema_registry;
mod schema_rules;

pub(crate) use loaded_version::LoadedVersion;
pub(crate) use schema_import::SchemaImport;
pub(crate) use schema_pull::SchemaPull;
pub(crate) use schema_push::{SchemaPush, SchemaPushReport};
pub(crate) use schema_registry::SchemaRegistry;
