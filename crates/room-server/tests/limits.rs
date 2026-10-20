mod support;
use room_server::{RoomError, RoomHandle};
use shared_protocol::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use support::*;
use sync_engine::Clock;

#[tokio::test]
async fn limits_are_bounded() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    for _ in 0..20 {
        c.room
            .apply(
                &c.host_session,
                command(state, &*clock, PlaybackCommand::Play),
            )
            .await
            .unwrap();
    }
    assert_eq!(
        c.room
            .apply(
                &c.host_session,
                command(state, &*clock, PlaybackCommand::Pause)
            )
            .await,
        Err(RoomError::RateLimited)
    );
    clock.advance_ms(1000);
    c.room
        .apply(
            &c.host_session,
            command(state, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
    for name in [
        "",
        &"a".repeat(65),
        &"é".repeat(33),
        "a\nb",
        "a\0b",
        "a\u{7f}b",
    ] {
        assert!(matches!(
            c.room.join(&c.invitation, name).await,
            Err(RoomError::InvalidInput)
        ));
        assert!(matches!(
            RoomHandle::create(media(), name, clock.clone()),
            Err(RoomError::InvalidInput)
        ));
    }
    c.room.join(&c.invitation, &"é".repeat(32)).await.unwrap();
}
#[tokio::test]
async fn join_fixed_window_survives_kick_churn() {
    let (c, clock) = setup();
    for _ in 0..10 {
        let guest = c.room.join(&c.invitation, "guest").await.unwrap();
        c.room
            .kick(&c.host_session, guest.participant_id)
            .await
            .unwrap();
    }
    assert!(matches!(
        c.room.join(&c.invitation, "guest").await,
        Err(RoomError::RateLimited)
    ));
    clock.advance_ms(60_000);
    c.room.join(&c.invitation, "guest").await.unwrap();
}
#[tokio::test]
async fn expired_command_is_not_reapplied() {
    let (c, clock) = setup();
    let cmd = command(initial(&c).await, &*clock, PlaybackCommand::Play);
    c.room.apply(&c.host_session, cmd).await.unwrap();
    clock.advance_ms(120_000);
    assert_eq!(
        c.room
            .apply(&c.host_session, cmd)
            .await
            .unwrap()
            .revision
            .get(),
        1
    );
    clock.advance_ms(1);
    assert_eq!(
        c.room.apply(&c.host_session, cmd).await,
        Err(RoomError::CommandExpired)
    );
    assert_eq!(initial(&c).await.revision.get(), 1);
}
#[tokio::test]
async fn command_age_future_and_epoch_are_checked() {
    let (c, clock) = setup();
    let cmd = command(initial(&c).await, &*clock, PlaybackCommand::Play);
    let future = CommandEnvelope {
        issued_at: HostTime {
            ticks_ms: WireU64::new(2001),
            ..cmd.issued_at
        },
        ..cmd
    };
    assert_eq!(
        c.room.apply(&c.host_session, future).await,
        Err(RoomError::CommandExpired)
    );
    let other_epoch = CommandEnvelope {
        issued_at: HostTime {
            epoch: ClockEpochId::new(),
            ..cmd.issued_at
        },
        ..cmd
    };
    assert_eq!(
        c.room.apply(&c.host_session, other_epoch).await,
        Err(RoomError::ClockMismatch)
    );
    let allowed = CommandEnvelope {
        issued_at: HostTime {
            ticks_ms: WireU64::new(2000),
            ..cmd.issued_at
        },
        ..cmd
    };
    c.room.apply(&c.host_session, allowed).await.unwrap();
    clock.advance_ms(120_001);
    // A future-dated accepted command must still never execute twice.
    assert_eq!(
        c.room
            .apply(&c.host_session, allowed)
            .await
            .unwrap()
            .revision
            .get(),
        1
    );
    clock.advance_ms(2_000);
    assert_eq!(
        c.room.apply(&c.host_session, allowed).await,
        Err(RoomError::CommandExpired)
    );
    assert_eq!(initial(&c).await.revision.get(), 1);
}
#[tokio::test]
async fn dedup_capacity_does_not_evict_live_results() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    let first = command(state, &*clock, PlaybackCommand::Play);
    c.room.apply(&c.host_session, first).await.unwrap();
    for i in 1..256 {
        if i % 20 == 0 {
            clock.advance_ms(1000);
        }
        c.room
            .apply(
                &c.host_session,
                command(state, &*clock, PlaybackCommand::Play),
            )
            .await
            .unwrap();
    }
    clock.advance_ms(1000);
    assert_eq!(
        c.room
            .apply(
                &c.host_session,
                command(state, &*clock, PlaybackCommand::Pause)
            )
            .await,
        Err(RoomError::RateLimited)
    );
    assert_eq!(
        c.room
            .apply(&c.host_session, first)
            .await
            .unwrap()
            .revision
            .get(),
        1
    );
    clock.advance_ms(120_001);
    c.room
        .apply(
            &c.host_session,
            command(state, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
    assert_eq!(
        c.room.apply(&c.host_session, first).await,
        Err(RoomError::CommandExpired)
    );
}
struct AdjustableClock {
    epoch: ClockEpochId,
    ticks: AtomicU64,
}
impl Clock for AdjustableClock {
    fn now(&self) -> HostTime {
        HostTime {
            epoch: self.epoch,
            ticks_ms: WireU64::new(self.ticks.load(Ordering::SeqCst)),
        }
    }
}
#[tokio::test]
async fn backwards_clock_cannot_change_state_or_extend_permit() {
    let clock = Arc::new(AdjustableClock {
        epoch: ClockEpochId::new(),
        ticks: AtomicU64::new(100),
    });
    let c = RoomHandle::create(media(), "host", clock.clone()).unwrap();
    let state = initial(&c).await;
    let permit = c
        .room
        .authorize_media(&c.host_session, state.media.media_id)
        .await
        .unwrap();
    clock.ticks.store(99, Ordering::SeqCst);
    assert_eq!(
        c.room.snapshot(&c.host_session).await,
        Err(RoomError::ClockMismatch)
    );
    assert_eq!(permit.check_active(), Err(RoomError::ClockMismatch));
    clock.ticks.store(100, Ordering::SeqCst);
    assert_eq!(initial(&c).await, state);
}
#[tokio::test]
async fn elapsed_calculations_do_not_wrap_at_maximum_ticks() {
    let clock = Arc::new(AdjustableClock {
        epoch: ClockEpochId::new(),
        ticks: AtomicU64::new(u64::MAX - 100),
    });
    let c = RoomHandle::create(media(), "host", clock.clone()).unwrap();
    let state = initial(&c).await;
    let cmd = command(state, &*clock, PlaybackCommand::Play);
    c.room.apply(&c.host_session, cmd).await.unwrap();
    clock.ticks.store(u64::MAX, Ordering::SeqCst);
    c.room
        .apply(
            &c.host_session,
            command(state, &*clock, PlaybackCommand::Pause),
        )
        .await
        .unwrap();
    assert_eq!(initial(&c).await.playback.position_ms.get(), 100);
}
