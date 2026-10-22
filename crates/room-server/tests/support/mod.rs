#![allow(dead_code)]
use room_server::{CreatedRoom, RoomHandle};
use shared_protocol::*;
use std::sync::Arc;
use sync_engine::{Clock, ManualClock};

pub fn media() -> MediaDescriptor {
    MediaDescriptor {
        media_id: MediaId::new(),
        size_bytes: WireU64::new(100_000),
        duration_ms: WireU64::new(10_000),
        fingerprint: MediaFingerprint::from_bytes([2; 32]),
    }
}
pub fn setup() -> (CreatedRoom, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: ClockEpochId::new(),
        ticks_ms: WireU64::new(0),
    }));
    (
        RoomHandle::create(media(), "host", clock.clone()).unwrap(),
        clock,
    )
}
pub fn command(
    snapshot: RoomSnapshot,
    clock: &dyn Clock,
    action: PlaybackCommand,
) -> CommandEnvelope {
    CommandEnvelope {
        room_id: snapshot.room_id,
        media_id: snapshot.media.media_id,
        command_id: CommandId::new(),
        issued_at: clock.now(),
        action,
    }
}
pub async fn initial(c: &CreatedRoom) -> RoomSnapshot {
    c.room.snapshot(&c.host_session).await.unwrap()
}
