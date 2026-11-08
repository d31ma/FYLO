use serde::{Deserialize, Serialize};

use crate::{FormatError, FormatErrorCode};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TtidTimestamps {
    pub created_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<u64>,
}

impl TtidTimestamps {
    const PRECISION: u64 = 10_000;
    const MINIMUM_TIMESTAMP_MILLISECONDS: u64 = 1_577_836_800_000;
    const MAXIMUM_TIMESTAMP_MILLISECONDS: u64 = 7_258_118_400_000;
    const MINIMUM_LENGTH: usize = 1;
    const MAXIMUM_LENGTH: usize = 36;
    const MINIMUM_SEGMENTS: usize = 1;
    const MAXIMUM_SEGMENTS: usize = 3;
    const MINIMUM_SEGMENT_LENGTH: usize = 1;
    const MAXIMUM_SEGMENT_LENGTH: usize = 11;
    const CREATION_POSITION: usize = 0;
    const UPDATE_POSITION: usize = 1;
    const DELETION_POSITION: usize = 2;
    const TIMESTAMP_BASE: u32 = 36;
    const ROUNDING_DIVISOR: u64 = 2;
    const PLACEHOLDER: &str = "x";
    const INVALID_SEGMENT: &str = "identifier contains an invalid segment";

    /// # Errors
    /// Returns `InvalidDocumentId` for invalid syntax, lifecycle, or timestamp range.
    pub fn decode(identifier: &str) -> Result<Self, FormatError> {
        Self::check_length(identifier)?;
        let segments: Vec<&str> = identifier.split('-').collect();
        Self::check_lifecycle(&segments)?;
        Self::check_segments(&segments)?;
        Self::reject_placeholder(
            segments[Self::CREATION_POSITION],
            "creation timestamp cannot be a placeholder",
        )?;
        let created_at = Self::decode_segment(segments[Self::CREATION_POSITION])?;
        let updated_at = Self::decode_update(segments.get(Self::UPDATE_POSITION).copied())?;
        let deleted_at = segments
            .get(Self::DELETION_POSITION)
            .copied()
            .map(Self::decode_deletion)
            .transpose()?;
        Ok(Self {
            created_at,
            updated_at,
            deleted_at,
        })
    }

    fn check_length(identifier: &str) -> Result<(), FormatError> {
        if !(Self::MINIMUM_LENGTH..=Self::MAXIMUM_LENGTH).contains(&identifier.len()) {
            return Err(Self::invalid("identifier length is outside FYLO bounds"));
        }
        Ok(())
    }

    fn check_lifecycle(segments: &[&str]) -> Result<(), FormatError> {
        if !(Self::MINIMUM_SEGMENTS..=Self::MAXIMUM_SEGMENTS).contains(&segments.len()) {
            return Err(Self::invalid("identifier has an invalid lifecycle shape"));
        }
        Ok(())
    }

    fn check_segments(segments: &[&str]) -> Result<(), FormatError> {
        for segment in segments {
            Self::check_segment_length(segment)?;
            Self::check_segment_characters(segment)?;
        }
        Ok(())
    }

    fn check_segment_length(segment: &str) -> Result<(), FormatError> {
        if !(Self::MINIMUM_SEGMENT_LENGTH..=Self::MAXIMUM_SEGMENT_LENGTH).contains(&segment.len()) {
            return Err(Self::invalid(Self::INVALID_SEGMENT));
        }
        Ok(())
    }

    fn check_segment_characters(segment: &str) -> Result<(), FormatError> {
        Self::require_alphanumeric(segment.bytes().all(Self::is_alphanumeric))
    }

    fn is_alphanumeric(byte: u8) -> bool {
        byte.is_ascii_alphanumeric()
    }

    fn require_alphanumeric(valid: bool) -> Result<(), FormatError> {
        if !valid {
            return Err(Self::invalid(Self::INVALID_SEGMENT));
        }
        Ok(())
    }

    fn reject_placeholder(segment: &str, message: &str) -> Result<(), FormatError> {
        if segment.eq_ignore_ascii_case(Self::PLACEHOLDER) {
            return Err(Self::invalid(message));
        }
        Ok(())
    }

    fn decode_update(segment: Option<&str>) -> Result<Option<u64>, FormatError> {
        segment
            .filter(Self::is_timestamp)
            .map(Self::decode_segment)
            .transpose()
    }

    fn is_timestamp(segment: &&str) -> bool {
        !segment.eq_ignore_ascii_case(Self::PLACEHOLDER)
    }

    fn decode_deletion(segment: &str) -> Result<u64, FormatError> {
        Self::reject_placeholder(segment, "deletion timestamp cannot be a placeholder")?;
        Self::decode_segment(segment)
    }

    fn decode_segment(segment: &str) -> Result<u64, FormatError> {
        let encoded = u64::from_str_radix(segment, Self::TIMESTAMP_BASE)
            .map_err(|_| Self::invalid("identifier timestamp is not valid base-36"))?;
        let rounded = encoded
            .checked_add(Self::PRECISION / Self::ROUNDING_DIVISOR)
            .ok_or_else(|| Self::invalid("identifier timestamp overflows"))?;
        let milliseconds = rounded / Self::PRECISION;
        Self::check_timestamp(milliseconds)?;
        Ok(milliseconds)
    }

    fn check_timestamp(milliseconds: u64) -> Result<(), FormatError> {
        if !(Self::MINIMUM_TIMESTAMP_MILLISECONDS..=Self::MAXIMUM_TIMESTAMP_MILLISECONDS)
            .contains(&milliseconds)
        {
            return Err(Self::invalid(
                "identifier timestamp is outside the supported range",
            ));
        }
        Ok(())
    }

    fn invalid(message: &str) -> FormatError {
        FormatError::new(FormatErrorCode::InvalidDocumentId, message)
    }
}
