use std::fmt::Write;

use serde_json::Value;

use super::store_position::StorePosition;
use super::store_sort_field::StoreSortField;
use super::store_sort_key::StoreSortKey;
use super::store_sort_value::StoreSortValue;
use crate::index::sha256_hex;
use crate::{QueryError, QueryErrorCode};

/// The opaque cursor of a Store scroll: `fyc1.<digest>.<hex>`, where the
/// digest ties it to one query and scope and the hex holds the JSON
/// `[[sort values...], id]` of the last document of the page.
pub(crate) struct StoreCursor;

impl StoreCursor {
    const FORMAT: &'static str = "fyc1";
    const PARTS: usize = 3;
    const DIGEST_HEX_DIGITS: usize = 16;
    const HEX_RADIX: u32 = 16;

    pub(crate) fn digest(scope: &Value) -> String {
        let encoded = serde_json::to_vec(scope).unwrap_or_default();
        let mut hex = sha256_hex(&encoded);
        hex.truncate(Self::DIGEST_HEX_DIGITS);
        hex
    }

    pub(crate) fn encode(position: &StorePosition, digest: &str) -> String {
        let values: Vec<Value> = position
            .keys()
            .iter()
            .map(|key| key.value().to_json())
            .collect();
        let body =
            serde_json::to_vec(&serde_json::json!([values, position.id()])).unwrap_or_default();
        format!("{}.{digest}.{}", Self::FORMAT, Self::hex(&body))
    }

    pub(crate) fn decode(
        cursor: &str,
        digest: &str,
        order: &[StoreSortField],
    ) -> Result<StorePosition, QueryError> {
        let body = Self::body(cursor, digest)?;
        let (values, id) = Self::parts(&body)?;
        let keys = Self::keys(values, order)?;
        Ok(StorePosition::new(keys, id.to_owned()))
    }

    fn body(cursor: &str, digest: &str) -> Result<Value, QueryError> {
        let parts: Vec<&str> = cursor.split('.').collect();
        Self::require_scope(&parts, digest)?;
        let bytes =
            Self::unhex(parts[2]).ok_or_else(|| Self::invalid("the cursor does not parse"))?;
        serde_json::from_slice(&bytes).map_err(|_| Self::invalid("the cursor does not parse"))
    }

    fn require_scope(parts: &[&str], digest: &str) -> Result<(), QueryError> {
        let shaped = parts.len() == Self::PARTS && parts[0] == Self::FORMAT;
        if !shaped {
            return Err(Self::invalid("the cursor does not parse"));
        }
        if parts[1] != digest {
            return Err(Self::invalid(
                "the cursor belongs to another query, collection or namespace; start again from the first page",
            ));
        }
        Ok(())
    }

    fn parts(body: &Value) -> Result<(&[Value], &str), QueryError> {
        match body.as_array().map(Vec::as_slice) {
            Some([Value::Array(values), Value::String(id)]) => Ok((values, id)),
            _ => Err(Self::invalid("the cursor does not parse")),
        }
    }

    fn keys(values: &[Value], order: &[StoreSortField]) -> Result<Vec<StoreSortKey>, QueryError> {
        if values.len() != order.len() {
            return Err(Self::invalid("the cursor does not match the query's order"));
        }
        values
            .iter()
            .zip(order)
            .map(|(value, entry)| Self::key(value, entry))
            .collect()
    }

    fn key(value: &Value, entry: &StoreSortField) -> Result<StoreSortKey, QueryError> {
        let sort_value = StoreSortValue::from_json(value)
            .ok_or_else(|| Self::invalid("the cursor holds a value that cannot be a sort key"))?;
        Ok(StoreSortKey::new(sort_value, entry.descending()))
    }

    fn hex(bytes: &[u8]) -> String {
        let mut text = String::with_capacity(bytes.len() * 2);
        for byte in bytes {
            let _ = write!(text, "{byte:02x}");
        }
        text
    }

    fn unhex(text: &str) -> Option<Vec<u8>> {
        let digits: Vec<char> = text.chars().collect();
        if !digits.len().is_multiple_of(2) {
            return None;
        }
        digits.chunks(2).map(Self::hex_byte).collect()
    }

    fn hex_byte(pair: &[char]) -> Option<u8> {
        let high = pair[0].to_digit(Self::HEX_RADIX)?;
        let low = pair[1].to_digit(Self::HEX_RADIX)?;
        u8::try_from(high * Self::HEX_RADIX + low).ok()
    }

    fn invalid(message: &str) -> QueryError {
        QueryError::new(QueryErrorCode::Cursor, message)
    }
}
