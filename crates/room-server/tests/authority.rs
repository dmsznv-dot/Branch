mod support;
use room_server::RoomError;
use shared_protocol::*;
use support::*;
use sync_engine::Clock;

#[tokio::test]
async fn host_uses_same_apply_path() {
    let (c, clock) = setup();
    let initial = initial(&c).await;
    assert_eq!(initial.revision.get(), 0);
    assert_eq!(initial.playback.status, PlaybackStatus::Paused);
    assert_eq!(initial.playback.speed, Speed::Normal);
    let state = c
        .room
        .apply(
            &c.host_session,
            command(initial, &*clock, PlaybackCommand::Play),
        )
        .await
        .unwrap();
    assert_eq!(state.revision.get(), 1);
    assert_eq!(state.playback.status, PlaybackStatus::Playing);
}
#[tokio::test]
async fn guest_control_depends_on_mode() {
    let (c, clock) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    c.room
        .apply(
            &guest.session,
            command(guest.snapshot, &*clock, PlaybackCommand::Play),
        )
        .await
        .unwrap();
    let state = c
        .room
        .set_control(&c.host_session, ControlMode::HostOnly)
        .await
        .unwrap();
    assert_eq!(
        c.room
            .apply(
                &guest.session,
                command(state, &*clock, PlaybackCommand::Pause)
            )
            .await,
        Err(RoomError::Forbidden)
    );
    assert_eq!(initial(&c).await, state);
    assert_eq!(
        c.room
            .set_control(&guest.session, ControlMode::Everyone)
            .await,
        Err(RoomError::Forbidden)
    );
    c.room
        .apply(
            &c.host_session,
            command(state, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
}
#[tokio::test]
async fn pause_and_speed_project_before_update() {
    let (c, clock) = setup();
    let play = command(initial(&c).await, &*clock, PlaybackCommand::Play);
    c.room.apply(&c.host_session, play).await.unwrap();
    clock.advance_ms(1000);
    let speed = Speed::try_from(2.0).unwrap();
    let faster = c
        .room
        .apply(
            &c.host_session,
            command(
                initial(&c).await,
                &*clock,
                PlaybackCommand::SetSpeed { speed },
            ),
        )
        .await
        .unwrap();
    assert_eq!(faster.playback.position_ms.get(), 1000);
    clock.advance_ms(1000);
    let paused = c
        .room
        .apply(
            &c.host_session,
            command(faster, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
    assert_eq!(paused.playback.position_ms.get(), 3000);
    assert_eq!(paused.playback.updated_at, clock.now());
}
#[tokio::test]
async fn seek_clamped_to_duration() {
    let (c, clock) = setup();
    let state = c
        .room
        .apply(
            &c.host_session,
            command(
                initial(&c).await,
                &*clock,
                PlaybackCommand::Seek {
                    position_ms: WireU64::new(u64::MAX),
                },
            ),
        )
        .await
        .unwrap();
    assert_eq!(state.playback.position_ms, state.media.duration_ms);
}
#[tokio::test]
async fn duplicate_is_idempotent() {
    let (c, clock) = setup();
    let cmd = command(initial(&c).await, &*clock, PlaybackCommand::Play);
    let first = c.room.apply(&c.host_session, cmd).await.unwrap();
    c.room
        .apply(
            &c.host_session,
            command(first, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
    assert_eq!(c.room.apply(&c.host_session, cmd).await.unwrap(), first);
    assert_eq!(initial(&c).await.revision.get(), 2);
}
#[tokio::test]
async fn duplicate_payload_conflict() {
    let (c, clock) = setup();
    let cmd = command(initial(&c).await, &*clock, PlaybackCommand::Play);
    c.room.apply(&c.host_session, cmd).await.unwrap();
    let changed = CommandEnvelope {
        action: PlaybackCommand::Pause,
        ..cmd
    };
    assert_eq!(
        c.room.apply(&c.host_session, changed).await,
        Err(RoomError::CommandConflict)
    );
    let changed = CommandEnvelope {
        issued_at: HostTime {
            ticks_ms: WireU64::new(1),
            ..cmd.issued_at
        },
        ..cmd
    };
    assert_eq!(
        c.room.apply(&c.host_session, changed).await,
        Err(RoomError::CommandConflict)
    );
}
#[tokio::test]
async fn wrong_room_or_media_rejected() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    let cmd = command(state, &*clock, PlaybackCommand::Play);
    assert_eq!(
        c.room
            .apply(
                &c.host_session,
                CommandEnvelope {
                    room_id: RoomId::new(),
                    ..cmd
                }
            )
            .await,
        Err(RoomError::InvalidInput)
    );
    assert_eq!(
        c.room
            .apply(
                &c.host_session,
                CommandEnvelope {
                    media_id: MediaId::new(),
                    ..cmd
                }
            )
            .await,
        Err(RoomError::MediaChanged)
    );
    assert_eq!(initial(&c).await, state);
}
#[tokio::test]
async fn buffering_does_not_pause_room() {
    let (c, clock) = setup();
    let state = c
        .room
        .apply(
            &c.host_session,
            command(initial(&c).await, &*clock, PlaybackCommand::Play),
        )
        .await
        .unwrap();
    c.room.set_buffering(&c.host_session, true).await.unwrap();
    c.room.set_buffering(&c.host_session, false).await.unwrap();
    assert_eq!(initial(&c).await, state);
}
#[tokio::test]
async fn concurrent_commands_have_one_total_order() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    let a = command(state, &*clock, PlaybackCommand::Play);
    let b = command(state, &*clock, PlaybackCommand::Pause);
    let (a, b) = tokio::join!(
        c.room.apply(&c.host_session, a),
        c.room.apply(&c.host_session, b)
    );
    let mut revisions = [a.unwrap().revision.get(), b.unwrap().revision.get()];
    revisions.sort();
    assert_eq!(revisions, [1, 2]);
    assert_eq!(initial(&c).await.revision.get(), 2);
}

#[tokio::test]
async fn dedup_and_command_rate_are_scoped_to_authenticated_session() {
    let (c, clock) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let cmd = command(guest.snapshot, &*clock, PlaybackCommand::Play);
    assert_eq!(
        c.room
            .apply(&c.host_session, cmd)
            .await
            .unwrap()
            .revision
            .get(),
        1
    );
    assert_eq!(
        c.room
            .apply(&guest.session, cmd)
            .await
            .unwrap()
            .revision
            .get(),
        2
    );
    for _ in 1..20 {
        c.room
            .apply(
                &c.host_session,
                command(guest.snapshot, &*clock, PlaybackCommand::Pause),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        c.room
            .apply(
                &c.host_session,
                command(guest.snapshot, &*clock, PlaybackCommand::Play)
            )
            .await,
        Err(RoomError::RateLimited)
    );
    c.room
        .apply(
            &guest.session,
            command(guest.snapshot, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
}
