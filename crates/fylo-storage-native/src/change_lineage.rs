use serde_json::{Map, Value};

use crate::{NativeStorageError, NativeStorageErrorCode};

/// The random value a collection's descriptor records each time its log
/// sequence numbers start again, so a change-feed cursor taken before that
/// is refused instead of skipping changes (ADR 0024).
pub(crate) struct ChangeLineage;

impl ChangeLineage {
    pub(crate) const DESCRIPTOR_KEY: &'static str = "changeLineage";
    const RANDOM_BYTES: usize = 16;

    pub(crate) fn insert_fresh(descriptor: &mut Map<String, Value>) -> Result<(), NativeStorageError> {
        descriptor.insert(Self::DESCRIPTOR_KEY.into(), Value::from(Self::fresh()?));
        Ok(())
    }

    fn fresh() -> Result<String, NativeStorageError> {
        let mut bytes = [0_u8; Self::RANDOM_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| {
            NativeStorageError::new(
                NativeStorageErrorCode::Io,
                format!("cannot draw a change lineage: {error}"),
            )
        })?;
        Ok(format!("{:032x}", u128::from_le_bytes(bytes)))
    }
}
