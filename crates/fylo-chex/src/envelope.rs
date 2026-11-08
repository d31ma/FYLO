//! The serialized form of a [`CompiledSchema`]: what a schema version record
//! stores as `compiled` (ADR 0017 section 4).
//!
//! Layout, all integers little-endian:
//!
//! ```text
//! magic     8 bytes  "CHEXDFA\0"
//! version   u32      1
//! flags     u32      bit 0: portable (every leaf has a DFA)
//! label     u32 length, UTF-8
//! schema    u32 length, the schema source as JSON
//! entries   u32 count, then per distinct pattern with a DFA:
//!             pattern  u32 length, UTF-8
//!             dfa      u32 length, a little-endian sparse DFA
//! checksum  u64      FNV-1a 64 of every byte before it
//! ```
//!
//! The loader never trusts the bytes: every length is bounds-checked, the
//! checksum catches a corrupted record, and each DFA goes through
//! `regex-automata`'s validating `from_bytes`.

use regex_automata::dfa::sparse;
use serde_json::Value;

use crate::compiled::{CompiledSchema, Matcher};
use crate::{ChexError, Result, truncate};

const MAGIC: &[u8; 8] = b"CHEXDFA\0";
const VERSION: u32 = 1;
const FLAG_PORTABLE: u32 = 1;

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn fnv1a(bytes: &[u8]) -> u64 {
    bytes.iter().fold(FNV_OFFSET, |hash, &byte| {
        (hash ^ u64::from(byte)).wrapping_mul(FNV_PRIME)
    })
}

fn corrupt(what: &str) -> ChexError {
    ChexError::schema_load(format!("Compiled schema is corrupt: {what}"))
}

#[cfg(feature = "compile")]
fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    let len = u32::try_from(bytes.len()).expect("a compiled schema section exceeds 4 GiB");
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(bytes);
}

#[cfg(feature = "compile")]
impl CompiledSchema {
    /// Serialize this schema: the envelope described in the module docs.
    ///
    /// A non-portable schema serializes too, flagged so, and
    /// [`from_bytes`](Self::from_bytes) refuses it.
    ///
    /// # Panics
    /// Only if a label, schema, or DFA exceeds 4 GiB.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&VERSION.to_le_bytes());
        let flags = if self.is_portable() { FLAG_PORTABLE } else { 0 };
        out.extend_from_slice(&flags.to_le_bytes());
        put_bytes(&mut out, self.label.as_bytes());
        let schema = serde_json::to_vec(&self.source).expect("a JSON value always serializes");
        put_bytes(&mut out, &schema);

        let dfas: Vec<_> = self
            .leaves
            .iter()
            .filter_map(|leaf| match &leaf.matcher {
                Matcher::Dfa(dfa) => Some((leaf.pattern.as_str(), dfa.to_bytes_little_endian())),
                Matcher::Regex(_) => None,
            })
            .collect();
        let count = u32::try_from(dfas.len()).expect("a schema has fewer than 2^32 leaves");
        out.extend_from_slice(&count.to_le_bytes());
        for (pattern, dfa) in &dfas {
            put_bytes(&mut out, pattern.as_bytes());
            put_bytes(&mut out, dfa);
        }
        let checksum = fnv1a(&out);
        out.extend_from_slice(&checksum.to_le_bytes());
        out
    }
}

/// A bounds-checked reader over the envelope body.
struct Reader<'a> {
    bytes: &'a [u8],
}

impl<'a> Reader<'a> {
    fn take(&mut self, len: usize, what: &str) -> Result<&'a [u8]> {
        if self.bytes.len() < len {
            return Err(corrupt(&format!("truncated {what}")));
        }
        let (head, rest) = self.bytes.split_at(len);
        self.bytes = rest;
        Ok(head)
    }

    fn u32(&mut self, what: &str) -> Result<u32> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn section(&mut self, what: &str) -> Result<&'a [u8]> {
        let len = usize::try_from(self.u32(what)?).map_err(|_| corrupt(what))?;
        self.take(len, what)
    }

    fn text(&mut self, what: &str) -> Result<&'a str> {
        std::str::from_utf8(self.section(what)?)
            .map_err(|_| corrupt(&format!("{what} is not UTF-8")))
    }
}

impl CompiledSchema {
    /// Load a schema serialized by [`to_bytes`](Self::to_bytes). Needs no
    /// regex compiler, so it works in every build.
    ///
    /// # Errors
    /// Returns `SchemaLoadError` for bytes that are not a compiled schema, are
    /// corrupt, or hold a DFA that `regex-automata` refuses, and for a schema
    /// that is not portable (some leaf has no DFA).
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let Some(body_len) = bytes.len().checked_sub(8) else {
            return Err(corrupt("truncated envelope"));
        };
        let (body, checksum) = bytes.split_at(body_len);
        if body.len() < MAGIC.len() || &body[..MAGIC.len()] != MAGIC {
            return Err(ChexError::schema_load(
                "Not a compiled CHEX schema (bad magic)".to_string(),
            ));
        }
        let mut stored = [0u8; 8];
        stored.copy_from_slice(checksum);
        if u64::from_le_bytes(stored) != fnv1a(body) {
            return Err(corrupt("checksum mismatch"));
        }

        let mut reader = Reader {
            bytes: &body[MAGIC.len()..],
        };
        let version = reader.u32("version")?;
        if version != VERSION {
            return Err(ChexError::schema_load(format!(
                "Compiled schema format version {version} is not supported (expected {VERSION})"
            )));
        }
        let flags = reader.u32("flags")?;
        if flags & !FLAG_PORTABLE != 0 {
            return Err(corrupt("unknown flags"));
        }
        let label = reader.text("label")?.to_owned();
        if flags & FLAG_PORTABLE == 0 {
            return Err(ChexError::schema_load(format!(
                "Compiled schema '{}' is not portable: a pattern has no DFA form (such as a \
                 Unicode word boundary), so only a native build can validate it",
                truncate(&label)
            )));
        }
        // `from_str`, not `from_slice`: the engine already links the former.
        let source: Value = serde_json::from_str(reader.text("schema")?)
            .map_err(|_| corrupt("schema is not JSON"))?;

        let count = reader.u32("entry count")?;
        // A sorted list rather than a hash map: a schema has a few dozen
        // patterns, and the browser build stays smaller.
        let mut dfas: Vec<(&str, &[u8])> = Vec::new();
        for _ in 0..count {
            let pattern = reader.text("pattern")?;
            let dfa = reader.section("DFA")?;
            match dfas.binary_search_by_key(&pattern, |(pattern, _)| pattern) {
                Ok(_) => return Err(corrupt("duplicate pattern")),
                Err(at) => dfas.insert(at, (pattern, dfa)),
            }
        }
        if !reader.bytes.is_empty() {
            return Err(corrupt("trailing bytes"));
        }

        let schema = Self::assemble(source, &label, &mut |pattern, _| {
            let Ok(at) = dfas.binary_search_by_key(&pattern, |(pattern, _)| pattern) else {
                return Err(corrupt("a pattern has no DFA"));
            };
            let bytes = dfas[at].1;
            let (dfa, read) = sparse::DFA::from_bytes(bytes)
                .map_err(|error| corrupt(&format!("invalid DFA: {error}")))?;
            if read != bytes.len() {
                return Err(corrupt("trailing bytes after a DFA"));
            }
            Ok(Matcher::Dfa(Box::new(dfa.to_owned())))
        })?;
        if schema.leaves.len() != dfas.len() {
            return Err(corrupt("a DFA belongs to no pattern"));
        }
        Ok(schema)
    }
}
