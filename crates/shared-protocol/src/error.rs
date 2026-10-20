use std::fmt;

use serde::{Deserialize, Serialize};

/// Validation failures at the wire boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProtocolError {
    InvalidUuid,
    InvalidWireU64,
    InvalidSpeed,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidUuid => "expected an RFC 4122 UUIDv4",
            Self::InvalidWireU64 => "expected a canonical decimal u64 string",
            Self::InvalidSpeed => "unsupported playback speed",
        })
    }
}

impl std::error::Error for ProtocolError {}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ErrorCode {
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
}
