use fylo_vfs::File;

use crate::NativeStorageError;

/// Reads at a byte offset without moving a shared file position, so one
/// handle serves every read of a query.
pub(crate) struct PositionedRead;

impl PositionedRead {
    pub(crate) fn exact(
        file: &File,
        buffer: &mut [u8],
        offset: u64,
    ) -> Result<(), NativeStorageError> {
        Self::read_exact_at(file, buffer, offset).map_err(NativeStorageError::io)
    }

    #[cfg(unix)]
    fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
        use std::os::unix::fs::FileExt;
        file.read_exact_at(buffer, offset)
    }

    #[cfg(windows)]
    fn read_exact_at(file: &File, mut buffer: &mut [u8], mut offset: u64) -> std::io::Result<()> {
        use std::os::windows::fs::FileExt;
        while !buffer.is_empty() {
            let read = file.seek_read(buffer, offset)?;
            if read == 0 {
                return Err(std::io::ErrorKind::UnexpectedEof.into());
            }
            buffer = &mut buffer[read..];
            offset += read as u64;
        }
        Ok(())
    }

    #[cfg(not(any(unix, windows)))]
    fn read_exact_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<()> {
        use std::io::{Read, Seek, SeekFrom};
        let mut reader = file;
        reader.seek(SeekFrom::Start(offset))?;
        reader.read_exact(buffer)
    }
}
