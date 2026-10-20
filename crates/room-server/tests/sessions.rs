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
