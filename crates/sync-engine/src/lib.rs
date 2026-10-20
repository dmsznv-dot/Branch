//! Deterministic playback synchronization.
//!
//! Clock epochs scope monotonic ticks. All synchronization decisions accept
//! explicit inputs and never emit authoritative playback commands.

mod clock;
mod correction;
mod estimator;
mod projection;

#[cfg(feature = "test-support")]
pub use clock::ManualClock;
pub use clock::{Clock, MonotonicClock};
pub use correction::{Correction, correction};
pub use estimator::{ClockEstimate, ClockEstimator, ClockSample};
pub use projection::{SnapshotCursor, position_at};

/// Invalid input must not advance synchronization state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SyncError {
    ClockMismatch,
    TimeWentBackwards,
    InvalidClockSample,
    RoundTripTooHigh,
    RoomMismatch,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ClockMismatch => "clock epoch mismatch",
            Self::TimeWentBackwards => "monotonic time went backwards",
            Self::InvalidClockSample => "invalid clock sample intervals",
            Self::RoundTripTooHigh => "clock sample round trip exceeds 2000 ms",
            Self::RoomMismatch => "snapshot room mismatch",
        })
    }
}

impl std::error::Error for SyncError {}
