use shared_protocol::{ClockEpochId, HostTime, Playback, PlaybackStatus, RoomId, RoomSnapshot};

use crate::SyncError;

/// Projects rational playback speed, truncating fractional milliseconds.
pub fn position_at(playback: &Playback, now: HostTime, duration_ms: u64) -> Result<u64, SyncError> {
    if now.epoch != playback.updated_at.epoch {
        return Err(SyncError::ClockMismatch);
    }
    let elapsed = now
        .ticks_ms
        .get()
        .checked_sub(playback.updated_at.ticks_ms.get())
        .ok_or(SyncError::TimeWentBackwards)?;
    let (numerator, denominator) = playback.speed.ratio();
    let advance = match playback.status {
        PlaybackStatus::Paused => 0,
        PlaybackStatus::Playing => {
            u128::from(elapsed) * u128::from(numerator) / u128::from(denominator)
        }
    };
    Ok((u128::from(playback.position_ms.get()) + advance).min(u128::from(duration_ms)) as u64)
}

/// Revision cursor. Call `reset` only after a trusted reconnect snapshot.
#[derive(Default)]
pub struct SnapshotCursor {
    current: Option<(RoomId, ClockEpochId, u64)>,
}

impl SnapshotCursor {
    pub fn new() -> Self {
        Self::default()
    }

    /// The first snapshot establishes identity. Later identities require reset,
    /// even if their revision would otherwise be stale.
    pub fn accept(&mut self, snapshot: &RoomSnapshot) -> Result<bool, SyncError> {
        if let Some((room_id, epoch, revision)) = self.current {
            if snapshot.room_id != room_id {
                return Err(SyncError::RoomMismatch);
            }
            if snapshot.playback.updated_at.epoch != epoch {
                return Err(SyncError::ClockMismatch);
            }
            if snapshot.revision.get() <= revision {
                return Ok(false);
            }
        }
        self.reset(snapshot);
        Ok(true)
    }

    /// Records the trusted snapshot as already applied, including its revision.
    pub fn reset(&mut self, snapshot: &RoomSnapshot) {
        self.current = Some((
            snapshot.room_id,
            snapshot.playback.updated_at.epoch,
            snapshot.revision.get(),
        ));
    }
}
