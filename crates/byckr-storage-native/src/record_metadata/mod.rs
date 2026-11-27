//! The one writer of record metadata (ADR 0033 section 6, BYCKR-L1-4b with
//! BYCKR-PERF1). [`MetadataPlan`] decides, without writing, the final
//! extended attributes and sidecar file of one record; [`MetadataWriter`]
//! writes that final state and is the only code that writes metadata;
//! [`MetadataReader`] reads both forms back.

mod attribute_plan;
mod metadata_changes;
mod metadata_plan;
mod metadata_plan_input;
mod metadata_reader;
mod metadata_writer;
mod sidecar_image;
mod sidecar_plan;

pub(crate) use crate::write::CapturedAttribute;
pub(crate) use metadata_changes::MetadataChanges;
pub(crate) use metadata_plan::MetadataPlan;
pub(crate) use metadata_plan_input::MetadataPlanInput;
pub(crate) use metadata_reader::MetadataReader;
pub(crate) use metadata_writer::MetadataWriter;
pub(crate) use sidecar_image::SidecarImage;
