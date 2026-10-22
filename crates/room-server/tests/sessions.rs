mod support;
use room_server::{RoomError, RoomHandle};
use shared_protocol::*;
use support::*;

#[tokio::test]
async fn fifth_allowed_sixth_rejected() {
    let (c, _) = setup();
    for _ in 0..4 {
        c.room.join(&c.invitation, "guest").await.unwrap();
    }
    assert!(matches!(
        c.room.join(&c.invitation, "sixth").await,
        Err(RoomError::RoomFull)
    ));
}
#[tokio::test]
async fn revoke_invite_keeps_existing_session() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let new = c.room.rotate_invitation(&c.host_session).await.unwrap();
    assert_ne!(new.expose(), c.invitation.expose());
    assert!(matches!(
        c.room.join(&c.invitation, "guest").await,
        Err(RoomError::Unauthorized)
    ));
    c.room.join(&new, "new guest").await.unwrap();
    c.room.snapshot(&guest.session).await.unwrap();
    assert!(matches!(
        c.room.rotate_invitation(&guest.session).await,
        Err(RoomError::Forbidden)
    ));
}

#[tokio::test]
async fn guest_must_select_the_same_video_bytes_before_joining() {
    let (c, _) = setup();
    let expected = initial(&c).await.media.fingerprint;
    assert!(matches!(
        c.room
            .join_with_media(
                &c.invitation,
                "guest",
                MediaFingerprint::from_bytes([0xEE; 32])
            )
            .await,
        Err(RoomError::MediaMismatch)
    ));
    let joined = c
        .room
        .join_with_media(&c.invitation, "guest", expected)
        .await
        .unwrap();
    assert_eq!(joined.snapshot.media.fingerprint, expected);
}
#[tokio::test]
async fn expired_session_cannot_be_revived() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    let permit = c
        .room
        .authorize_media(&c.host_session, state.media.media_id)
        .await
        .unwrap();
    clock.advance_ms(300_001);
    assert!(matches!(
        c.room
            .authorize_media(&c.host_session, state.media.media_id)
            .await,
        Err(RoomError::Unauthorized)
    ));
    assert_eq!(permit.check_active(), Err(RoomError::Unauthorized));
    assert_eq!(
        c.room.snapshot(&c.host_session).await,
        Err(RoomError::Unauthorized)
    );
    permit.cancelled().await;
}
#[tokio::test]
async fn active_reads_and_status_extend_live_session() {
    let (c, clock) = setup();
    let state = initial(&c).await;
    let permit = c
        .room
        .authorize_media(&c.host_session, state.media.media_id)
        .await
        .unwrap();
    clock.advance_ms(300_000);
    permit.check_active().unwrap();
    clock.advance_ms(300_000);
    c.room.set_buffering(&c.host_session, true).await.unwrap();
    clock.advance_ms(300_000);
    c.room.snapshot(&c.host_session).await.unwrap();
}
#[tokio::test]
async fn tokens_are_distinct_random_redacted_secrets() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    for secret in [
        c.host_session.expose(),
        guest.session.expose(),
        c.invitation.expose(),
    ] {
        assert_eq!(secret.len(), 43);
        assert!(
            secret
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert!(!format!("{:?} {}", c.host_session, c.host_session).contains(secret));
        assert!(!format!("{:?} {}", c.invitation, c.invitation).contains(secret));
    }
    assert_ne!(c.host_session.expose(), guest.session.expose());
    assert_ne!(c.host_session.expose(), c.invitation.expose());
}
#[test]
fn invitation_and_session_tokens_round_trip_canonically() {
    let (created, _) = setup();
    let invitation = room_server::InvitationToken::parse(created.invitation.expose()).unwrap();
    assert_eq!(invitation.expose(), created.invitation.expose());
    assert!(room_server::InvitationToken::parse("short").is_err());
    assert!(
        room_server::InvitationToken::parse(&format!("{}=", created.invitation.expose())).is_err()
    );
    assert!(room_server::InvitationToken::parse(&format!("!{}", "A".repeat(42))).is_err());
}

#[tokio::test]
async fn foreign_session_has_no_authority() {
    let (c, clock) = setup();
    let other = RoomHandle::create(media(), "other", clock).unwrap();
    assert_eq!(
        c.room.snapshot(&other.host_session).await,
        Err(RoomError::Unauthorized)
    );
}
#[tokio::test]
async fn host_only_lifecycle_methods_reject_guest() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    assert_eq!(
        c.room.kick(&guest.session, c.host_id).await,
        Err(RoomError::Forbidden)
    );
    assert_eq!(
        c.room.replace_media(&guest.session, media()).await,
        Err(RoomError::Forbidden)
    );
    assert_eq!(
        c.room.close(&guest.session).await,
        Err(RoomError::Forbidden)
    );
    assert_eq!(
        c.room.kick(&c.host_session, c.host_id).await,
        Err(RoomError::Forbidden)
    );
    assert_eq!(
        c.room.kick(&c.host_session, ParticipantId::new()).await,
        Err(RoomError::InvalidInput)
    );
}

#[test]
fn overlapping_permit_clock_samples_are_not_false_clock_regressions() {
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    };
    use sync_engine::Clock;

    struct OverlappingClock {
        epoch: ClockEpochId,
        other_epoch: ClockEpochId,
        switch_epoch: AtomicBool,
        ticks: AtomicU64,
        pause_next: AtomicBool,
        sampled: mpsc::Sender<()>,
        resume: Mutex<mpsc::Receiver<()>>,
    }
    impl Clock for OverlappingClock {
        fn now(&self) -> HostTime {
            let sample = HostTime {
                epoch: if self.switch_epoch.load(Ordering::SeqCst) {
                    self.other_epoch
                } else {
                    self.epoch
                },
                ticks_ms: WireU64::new(self.ticks.load(Ordering::SeqCst)),
            };
            if self.pause_next.swap(false, Ordering::SeqCst) {
                self.sampled.send(()).unwrap();
                self.resume.lock().unwrap().recv().unwrap();
            }
            sample
        }
    }
    let (sampled_tx, sampled_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let clock = Arc::new(OverlappingClock {
        epoch: ClockEpochId::new(),
        other_epoch: ClockEpochId::new(),
        switch_epoch: AtomicBool::new(false),
        ticks: AtomicU64::new(100),
        pause_next: AtomicBool::new(false),
        sampled: sampled_tx,
        resume: Mutex::new(resume_rx),
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let c = RoomHandle::create(media(), "host", clock.clone()).unwrap();
    let descriptor = runtime
        .block_on(c.room.snapshot(&c.host_session))
        .unwrap()
        .media;
    let a = runtime
        .block_on(c.room.authorize_media(&c.host_session, descriptor.media_id))
        .unwrap();
    let b = runtime
        .block_on(c.room.authorize_media(&c.host_session, descriptor.media_id))
        .unwrap();
    clock.pause_next.store(true, Ordering::SeqCst);
    let (earlier, later) = std::thread::scope(|scope| {
        let earlier = scope.spawn(|| a.check_active());
        sampled_rx.recv().unwrap();
        clock.ticks.store(101, Ordering::SeqCst);
        let later = b.check_active();
        resume_tx.send(()).unwrap();
        (earlier.join().unwrap(), later)
    });
    assert_eq!(later, Ok(()));
    assert_eq!(earlier, Ok(()));
    // Genuine nonoverlapping regression and epoch changes remain invalid.
    clock.ticks.store(100, Ordering::SeqCst);
    assert_eq!(a.check_active(), Err(RoomError::ClockMismatch));
    clock.ticks.store(101, Ordering::SeqCst);
    clock.switch_epoch.store(true, Ordering::SeqCst);
    assert_eq!(a.check_active(), Err(RoomError::ClockMismatch));
}
