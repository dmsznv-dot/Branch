use std::{
    fmt,
    fs::{File, OpenOptions},
    io::{ErrorKind, Read, Seek, SeekFrom},
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use tokio_util::sync::CancellationToken;

use crate::EngineError;

/// One trusted file selection. The pathname is never retained or reopened.
/// All reads and seeks share one cursor lock. No serialization is provided.
pub struct SelectedFile {
    file: Mutex<File>,
    display_name: String,
    size: u64,
    modified: SystemTime,
    changed: AtomicBool,
}

impl fmt::Debug for SelectedFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SelectedFile").finish_non_exhaustive()
    }
}

impl SelectedFile {
    /// Call only at the trusted native file-selection boundary.
    pub fn open(path: &Path) -> Result<Self, EngineError> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // A read-only FIFO open otherwise waits for a writer before we can
            // inspect the descriptor. O_NONBLOCK is ignored for regular files;
            // the descriptor check below still rejects special files if the
            // pathname changes during selection.
            options.custom_flags(libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE.
            options.share_mode(0x1 | 0x2 | 0x4);
            // FILE_FLAG_BACKUP_SEMANTICS allows opening a directory so the
            // descriptor metadata below rejects it consistently on Windows.
            options.custom_flags(0x0200_0000);
        }
        let file = options.open(path).map_err(|_| EngineError::Unavailable)?;
        let metadata = file.metadata().map_err(|_| EngineError::Unavailable)?;
        if !metadata.is_file() {
            return Err(EngineError::UnsupportedMedia);
        }
        let size = metadata.len();
        checked_stream_length(size)?;
        Ok(Self {
            file: Mutex::new(file),
            display_name: path
                .file_name()
                .ok_or(EngineError::UnsupportedMedia)?
                .to_string_lossy()
                .into_owned(),
            size,
            modified: metadata.modified().map_err(|_| EngineError::Unavailable)?,
            changed: AtomicBool::new(false),
        })
    }

    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    pub fn len(&self) -> u64 {
        self.size
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Read at most CHUNK_BYTES from the original descriptor. EOF returns zero;
    /// offsets beyond EOF fail. Discard `dst` on error, including cancellation.
    /// Cancellation is checked around blocking operations; it cannot interrupt
    /// an operating-system read already in progress.
    pub fn read_at(
        &self,
        offset: u64,
        dst: &mut [u8],
        cancel: &CancellationToken,
    ) -> Result<usize, EngineError> {
        self.check(cancel)?;
        let remaining = self
            .size
            .checked_sub(offset)
            .ok_or(EngineError::InvalidCommand)?;
        let len = remaining.min(dst.len().min(media_stream::CHUNK_BYTES) as u64) as usize;
        let mut file = self.file.lock().map_err(|_| EngineError::Unavailable)?;
        self.check(cancel)?;
        self.check_metadata(&file, cancel)?;
        if len == 0 {
            return Ok(0);
        }

        let seek = file.seek(SeekFrom::Start(offset));
        self.check(cancel)?;
        seek.map_err(|_| EngineError::Unavailable)?;
        let mut filled = 0;
        while filled < len {
            self.check(cancel)?;
            let read = file.read(&mut dst[filled..len]);
            self.check(cancel)?;
            match read {
                Ok(0) => return self.mark_changed(),
                Ok(count) => filled += count,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(_) => return Err(EngineError::Unavailable),
            }
        }
        self.check_metadata(&file, cancel)?;
        Ok(filled)
    }

    fn check(&self, cancel: &CancellationToken) -> Result<(), EngineError> {
        if cancel.is_cancelled() {
            Err(EngineError::Cancelled)
        } else if self.changed.load(Ordering::SeqCst) {
            Err(EngineError::SourceChanged)
        } else {
            Ok(())
        }
    }

    fn check_metadata(&self, file: &File, cancel: &CancellationToken) -> Result<(), EngineError> {
        self.check(cancel)?;
        let metadata = file.metadata();
        self.check(cancel)?;
        let metadata = metadata.map_err(|_| EngineError::Unavailable)?;
        if metadata.len() != self.size
            || metadata.modified().map_err(|_| EngineError::Unavailable)? != self.modified
        {
            return self.mark_changed();
        }
        Ok(())
    }

    fn mark_changed<T>(&self) -> Result<T, EngineError> {
        self.changed.store(true, Ordering::SeqCst);
        Err(EngineError::SourceChanged)
    }
}

fn checked_stream_length(len: u64) -> Result<i64, EngineError> {
    i64::try_from(len).map_err(|_| EngineError::InvalidCommand)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stream_length_is_checked_before_ffi() {
        assert_eq!(checked_stream_length(i64::MAX as u64), Ok(i64::MAX));
        assert_eq!(
            checked_stream_length(i64::MAX as u64 + 1),
            Err(EngineError::InvalidCommand)
        );
    }
}
