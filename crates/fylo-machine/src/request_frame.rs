use std::io::BufRead;

use crate::Clock;
use crate::operation_field::OperationField;

/// One request frame read from a transport, before it is answered.
///
/// Reading and answering are separate so a server can read from a slow
/// network client without holding the root's executor.
pub struct RequestFrame {
    bytes: Vec<u8>,
    consumed: usize,
    oversized: bool,
    delimited: bool,
    started: Clock,
}

impl RequestFrame {
    /// Read the next LF-delimited frame, keeping at most `limit` bytes of it.
    ///
    /// An oversized frame is still consumed to its delimiter, so the session
    /// can answer it and carry on with the next one.
    ///
    /// # Errors
    /// Returns the transport's read error.
    pub fn read<R: BufRead>(input: &mut R, limit: usize) -> std::io::Result<Self> {
        let mut bytes = Vec::new();
        let (consumed, oversized, delimited) = crate::read_frame(input, limit, &mut bytes)?;
        Ok(Self {
            bytes,
            consumed,
            oversized,
            delimited,
            started: Clock::start(),
        })
    }

    /// The transport ended cleanly before any byte of a new frame.
    #[must_use]
    pub fn is_end_of_stream(&self) -> bool {
        self.consumed == 0 && self.bytes.is_empty()
    }

    /// The frame's `op`, when the frame is a JSON object naming exactly one.
    #[must_use]
    pub fn operation(&self) -> Option<String> {
        serde_json::from_slice::<OperationField>(&self.bytes)
            .ok()
            .and_then(|field| field.op)
    }

    pub(crate) fn into_parts(self) -> ((Vec<u8>, usize, bool, bool), Clock) {
        (
            (self.bytes, self.consumed, self.oversized, self.delimited),
            self.started,
        )
    }
}
