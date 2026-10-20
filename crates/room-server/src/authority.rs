use crate::{
    RoomError,
    limits::{COMMAND_TTL_MS, FUTURE_ALLOWANCE_MS},
};
use shared_protocol::{
    CommandEnvelope, HostTime, PlaybackCommand, PlaybackStatus, RoomSnapshot, WireU64,
};
use sync_engine::position_at;

pub(crate) fn validate_time(command: CommandEnvelope, now: HostTime) -> Result<(), RoomError> {
    if command.issued_at.epoch != now.epoch {
        return Err(RoomError::ClockMismatch);
    }
    let issued = command.issued_at.ticks_ms.get();
    let ticks = now.ticks_ms.get();
    if ticks.saturating_sub(issued) > COMMAND_TTL_MS
        || issued.saturating_sub(ticks) > FUTURE_ALLOWANCE_MS
    {
        return Err(RoomError::CommandExpired);
    }
    Ok(())
}

pub(crate) fn next_revision(snapshot: RoomSnapshot) -> Result<WireU64, RoomError> {
    snapshot
        .revision
        .get()
        .checked_add(1)
        .map(WireU64::new)
        .ok_or(RoomError::CounterExhausted)
}

pub(crate) fn apply(
    snapshot: RoomSnapshot,
    action: PlaybackCommand,
    now: HostTime,
) -> Result<RoomSnapshot, RoomError> {
    let revision = next_revision(snapshot)?;
    let position = position_at(&snapshot.playback, now, snapshot.media.duration_ms.get())
        .map_err(|_| RoomError::ClockMismatch)?;
    let mut next = snapshot;
    next.revision = revision;
    next.playback.position_ms = WireU64::new(position);
    next.playback.updated_at = now;
    match action {
        PlaybackCommand::Play => next.playback.status = PlaybackStatus::Playing,
        PlaybackCommand::Pause => next.playback.status = PlaybackStatus::Paused,
        PlaybackCommand::Seek { position_ms } => {
            next.playback.position_ms =
                WireU64::new(position_ms.get().min(next.media.duration_ms.get()))
        }
        PlaybackCommand::SetSpeed { speed } => next.playback.speed = speed,
    }
    Ok(next)
}
