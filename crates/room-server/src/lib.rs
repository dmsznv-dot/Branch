//! In-memory authoritative rooms with bounded sessions and revocable media access.
//!
//! All authority mutations are serialized. No network or filesystem I/O is done
//! while holding the room mutex. Tokens never appear in snapshots or errors.
mod authority;
mod limits;
mod room;
mod sessions;

pub use room::{CreatedRoom, JoinedRoom, RoomHandle};
pub use sessions::{InvitationToken, MediaPermit, SessionToken};

/// Public failures contain no token material.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RoomError {
    InvalidInput,
    Unauthorized,
    Forbidden,
    RoomFull,
    RoomClosed,
    MediaChanged,
    CommandConflict,
    CommandExpired,
    RateLimited,
    ClockMismatch,
    CounterExhausted,
    EntropyUnavailable,
}

impl std::fmt::Display for RoomError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidInput => "invalid input",
            Self::Unauthorized => "unauthorized session or invitation",
            Self::Forbidden => "operation not permitted",
            Self::RoomFull => "room is full",
            Self::RoomClosed => "room is closed",
            Self::MediaChanged => "media changed",
            Self::CommandConflict => "command identity conflict",
            Self::CommandExpired => "command outside allowed age",
            Self::RateLimited => "rate limit reached",
            Self::ClockMismatch => "monotonic clock mismatch",
            Self::CounterExhausted => "counter exhausted",
            Self::EntropyUnavailable => "secure randomness unavailable",
        })
    }
}
impl std::error::Error for RoomError {}
