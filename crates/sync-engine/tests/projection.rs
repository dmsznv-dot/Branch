use shared_protocol::{
    ClockEpochId, ControlMode, HostTime, MediaDescriptor, MediaId, Playback, PlaybackStatus,
    RoomId, RoomSnapshot, Speed, WireU64,
};
use sync_engine::{SnapshotCursor, position_at};

fn time(ticks: u64) -> HostTime {
    HostTime {
        epoch: ClockEpochId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        ticks_ms: WireU64::new(ticks),
    }
}

fn playback() -> Playback {
    Playback {
        status: PlaybackStatus::Playing,
        position_ms: WireU64::new(125_000),
        speed: Speed::ThreeHalves,
        updated_at: time(1000),
    }
}

#[test]
fn playing_projects_rational_speed() {
    let mut playback = playback();
    assert_eq!(position_at(&playback, time(3000), 200_000), Ok(128_000));
    playback.status = PlaybackStatus::Paused;
    assert_eq!(position_at(&playback, time(3000), 200_000), Ok(125_000));
}

#[test]
fn projection_clamps_end_and_rejects_epoch() {
    let playback = playback();
    assert_eq!(position_at(&playback, time(3000), 126_000), Ok(126_000));
    assert_eq!(position_at(&playback, time(1000), 0), Ok(0));
    assert!(position_at(&playback, time(999), 200_000).is_err());
    let other_epoch = HostTime {
        epoch: ClockEpochId::parse("22222222-2222-4222-8222-222222222222").unwrap(),
        ..time(3000)
    };
    assert!(position_at(&playback, other_epoch, 200_000).is_err());
    let paused = Playback {
        status: PlaybackStatus::Paused,
        ..playback
    };
    assert!(position_at(&paused, time(999), 200_000).is_err());
    assert!(position_at(&paused, other_epoch, 200_000).is_err());
}

#[test]
fn all_speeds_project_exact_rationals_with_truncation() {
    for (speed, projected) in [
        (Speed::Half, 125_001),
        (Speed::ThreeQuarters, 125_002),
        (Speed::Normal, 125_003),
        (Speed::FiveQuarters, 125_003),
        (Speed::ThreeHalves, 125_004),
        (Speed::Double, 125_006),
    ] {
        let playback = Playback {
            speed,
            ..playback()
        };
        assert_eq!(position_at(&playback, time(1003), u64::MAX), Ok(projected));
    }
}

#[test]
fn projection_uses_wide_arithmetic_before_clamping() {
    let playback = Playback {
        position_ms: WireU64::new(u64::MAX - 1),
        speed: Speed::Double,
        updated_at: time(0),
        ..playback()
    };
    assert_eq!(
        position_at(&playback, time(u64::MAX), u64::MAX),
        Ok(u64::MAX)
    );
}

fn snapshot() -> RoomSnapshot {
    RoomSnapshot {
        room_id: RoomId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
        media: MediaDescriptor {
            media_id: MediaId::parse("11111111-1111-4111-8111-111111111111").unwrap(),
            size_bytes: WireU64::new(1),
            duration_ms: WireU64::new(200_000),
        },
        revision: WireU64::new(10),
        playback: playback(),
        control: ControlMode::Everyone,
    }
}

#[test]
fn stale_snapshot_and_epoch_require_reset() {
    let mut cursor = SnapshotCursor::new();
    let mut snapshot = snapshot();
    assert_eq!(cursor.accept(&snapshot), Ok(true));
    assert_eq!(cursor.accept(&snapshot), Ok(false));
    snapshot.revision = WireU64::new(9);
    assert_eq!(cursor.accept(&snapshot), Ok(false));
    snapshot.revision = WireU64::new(11);
    snapshot.media.media_id = MediaId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    assert_eq!(cursor.accept(&snapshot), Ok(true));
    snapshot.playback.updated_at.epoch =
        ClockEpochId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    snapshot.revision = WireU64::new(12);
    assert!(cursor.accept(&snapshot).is_err());
    cursor.reset(&snapshot);
    assert_eq!(cursor.accept(&snapshot), Ok(false));
    snapshot.revision = WireU64::new(13);
    assert_eq!(cursor.accept(&snapshot), Ok(true));
    snapshot.room_id = RoomId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    assert!(cursor.accept(&snapshot).is_err());
    snapshot.revision = WireU64::new(0);
    cursor.reset(&snapshot);
    assert_eq!(cursor.accept(&snapshot), Ok(false));
    snapshot.revision = WireU64::new(1);
    assert_eq!(cursor.accept(&snapshot), Ok(true));
}

#[test]
fn rejected_snapshot_does_not_change_cursor_or_require_higher_revision() {
    let mut cursor = SnapshotCursor::default();
    let snapshot = snapshot();
    cursor.reset(&snapshot);
    let mut wrong_room = snapshot;
    wrong_room.room_id = RoomId::parse("22222222-2222-4222-8222-222222222222").unwrap();
    wrong_room.revision = WireU64::new(u64::MAX);
    assert!(cursor.accept(&wrong_room).is_err());
    wrong_room.revision = WireU64::new(0);
    assert!(cursor.accept(&wrong_room).is_err());
    let next = RoomSnapshot {
        revision: WireU64::new(11),
        ..snapshot
    };
    assert_eq!(cursor.accept(&next), Ok(true));
}
