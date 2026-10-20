//! Original bytes from one explicitly selected file descriptor.
//! Transports must retain a [`ReaderLease`] for the entire response body and
//! observe the media permit's cancellation signal while transmitting it.
mod range;
mod source;

pub use range::{ByteRange, RangeError, resolve_range};
pub use source::{ReaderLease, ReaderLimits, SelectedMedia};

pub const CHUNK_BYTES: usize = 65_536;
pub const MAX_READERS_PER_SESSION: usize = 2;
pub const MAX_READERS_PER_ROOM: usize = 8;

#[derive(Debug)]
pub enum StreamError {
    Io(std::io::Error),
    Permission(room_server::RoomError),
    MediaChanged,
    InvalidOffset,
    ReaderLimit,
    NotRegularFile,
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Io(_) => "selected media I/O failed",
            Self::Permission(_) => "media permission revoked",
            Self::MediaChanged => "selected media changed",
            Self::InvalidOffset => "offset outside selected media",
            Self::ReaderLimit => "active reader limit reached",
            Self::NotRegularFile => "selected media is not a regular file",
        })
    }
}
impl std::error::Error for StreamError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Permission(error) => Some(error),
            _ => None,
        }
    }
}
impl From<std::io::Error> for StreamError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}
impl From<room_server::RoomError> for StreamError {
    fn from(error: room_server::RoomError) -> Self {
        Self::Permission(error)
    }
}
