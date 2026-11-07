use crate::{NativeStorageError, NativeStorageErrorCode};
use fylo_query::IndexKeyRange;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

/// Reads runs of keys from a sorted, newline-delimited index snapshot without
/// reading the whole snapshot.
///
/// A binary search over byte offsets finds where a run starts: from any
/// offset, the next line start is found by reading to the end of the current
/// line. Empty lines are skipped, as the snapshot parser skips them.
pub(super) struct SortedKeyReader<R> {
    reader: BufReader<R>,
    length: u64,
    position: u64,
    max_key_bytes: u64,
}

impl<R: Read + Seek> SortedKeyReader<R> {
    const LINE_END: u8 = b'\n';
    const BUFFER_BYTES: usize = 4096;

    pub(super) fn new(reader: R, length: u64, max_key_bytes: usize) -> Self {
        Self {
            reader: BufReader::with_capacity(Self::BUFFER_BYTES, reader),
            length,
            position: 0,
            max_key_bytes: u64::try_from(max_key_bytes).unwrap_or(u64::MAX),
        }
    }

    /// Every key in `range`, in snapshot order.
    ///
    /// # Errors
    ///
    /// Returns an I/O error, or a corrupt-index error when a key is too long
    /// or the keys of the run are not strictly sorted.
    pub(super) fn keys_in(
        &mut self,
        range: &IndexKeyRange,
    ) -> Result<Vec<Vec<u8>>, NativeStorageError> {
        let start = self.first_line_at_or_after(range.start())?;
        self.seek(start)?;
        self.keys_before_end(range)
    }

    /// The offset of the first line whose key is at least `key`, or the
    /// snapshot length when there is none.
    fn first_line_at_or_after(&mut self, key: &[u8]) -> Result<u64, NativeStorageError> {
        let mut low = 0;
        let mut high = self.length;
        while low < high {
            let middle = u64::midpoint(low, high);
            let line_is_below = self
                .key_from(middle)?
                .is_some_and(|(_, found)| found.as_slice() < key);
            (low, high) = Self::narrow(low, high, middle, line_is_below);
        }
        Ok(self.key_from(low)?.map_or(self.length, |(start, _)| start))
    }

    fn narrow(low: u64, high: u64, middle: u64, line_is_below: bool) -> (u64, u64) {
        if line_is_below {
            return (middle + 1, high);
        }
        (low, middle)
    }

    /// The first key whose line starts at or after `offset`, with its offset.
    fn key_from(&mut self, offset: u64) -> Result<Option<(u64, Vec<u8>)>, NativeStorageError> {
        self.seek_to_line_start(offset)?;
        self.next_key()
    }

    fn seek_to_line_start(&mut self, offset: u64) -> Result<(), NativeStorageError> {
        if offset == 0 {
            return self.seek(0);
        }
        self.seek(offset - 1)?;
        self.read_line().map(drop)
    }

    fn keys_before_end(
        &mut self,
        range: &IndexKeyRange,
    ) -> Result<Vec<Vec<u8>>, NativeStorageError> {
        let mut keys: Vec<Vec<u8>> = Vec::new();
        while let Some((_, key)) = self.next_key()? {
            if !range.is_before_end(&key) {
                break;
            }
            Self::require_sorted(keys.last(), &key)?;
            keys.push(key);
        }
        Ok(keys)
    }

    fn require_sorted(previous: Option<&Vec<u8>>, key: &[u8]) -> Result<(), NativeStorageError> {
        if previous.is_some_and(|previous| previous.as_slice() >= key) {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptIndex,
                "index snapshot keys must be strictly sorted and unique",
            ));
        }
        Ok(())
    }

    /// The next non-empty line and the offset it starts at.
    fn next_key(&mut self) -> Result<Option<(u64, Vec<u8>)>, NativeStorageError> {
        let mut start = self.position;
        let mut line = self.read_line()?;
        while line.as_ref().is_some_and(Vec::is_empty) {
            start = self.position;
            line = self.read_line()?;
        }
        Ok(line.map(|key| (start, key)))
    }

    /// One line without its line end, or `None` at the end of the snapshot.
    fn read_line(&mut self) -> Result<Option<Vec<u8>>, NativeStorageError> {
        let mut line = Vec::new();
        let read = (&mut self.reader)
            .take(self.max_key_bytes.saturating_add(1))
            .read_until(Self::LINE_END, &mut line)
            .map_err(NativeStorageError::io)?;
        self.position += read as u64;
        Self::strip_line_end(line, self.max_key_bytes)
    }

    fn strip_line_end(
        mut line: Vec<u8>,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<u8>>, NativeStorageError> {
        if line.last() == Some(&Self::LINE_END) {
            line.pop();
            return Ok(Some(line));
        }
        Self::final_line(line, max_key_bytes)
    }

    /// A line with no line end is the snapshot's last line, unless it is
    /// longer than any key may be.
    fn final_line(
        line: Vec<u8>,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<u8>>, NativeStorageError> {
        if line.len() as u64 > max_key_bytes {
            return Err(NativeStorageError::new(
                NativeStorageErrorCode::CorruptIndex,
                format!("index key exceeds {max_key_bytes} bytes"),
            ));
        }
        Ok((!line.is_empty()).then_some(line))
    }

    fn seek(&mut self, offset: u64) -> Result<(), NativeStorageError> {
        self.reader
            .seek(SeekFrom::Start(offset))
            .map_err(NativeStorageError::io)?;
        self.position = offset;
        Ok(())
    }
}
