mod support;
use room_server::RoomError;
use shared_protocol::*;
use support::*;

#[tokio::test]
async fn kick_revokes_media_permit() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let permit = c
        .room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let mut watch = c.room.subscribe(&guest.session).await.unwrap();
    c.room
        .kick(&c.host_session, guest.participant_id)
        .await
        .unwrap();
    permit.cancelled().await;
    assert_eq!(permit.check_active(), Err(RoomError::Unauthorized));
    assert_eq!(
        c.room.snapshot(&guest.session).await,
        Err(RoomError::Unauthorized)
    );
    assert!(watch.changed().await.is_err());
    c.room.join(&c.invitation, "replacement").await.unwrap();
}
#[tokio::test]
async fn close_revokes_all() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let host = c
        .room
        .authorize_media(&c.host_session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let permit = c
        .room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let mut watch = c.room.subscribe(&c.host_session).await.unwrap();
    c.room.close(&c.host_session).await.unwrap();
    host.cancelled().await;
    permit.cancelled().await;
    assert_eq!(host.check_active(), Err(RoomError::RoomClosed));
    assert_eq!(permit.check_active(), Err(RoomError::RoomClosed));
    assert_eq!(
        c.room.snapshot(&c.host_session).await,
        Err(RoomError::RoomClosed)
    );
    assert!(matches!(
        c.room.join(&c.invitation, "guest").await,
        Err(RoomError::RoomClosed)
    ));
    assert_eq!(
        c.room.close(&c.host_session).await,
        Err(RoomError::RoomClosed)
    );
    assert!(watch.changed().await.is_err());
}
#[tokio::test]
async fn replacement_cancels_old_media() {
    let (c, clock) = setup();
    let old = initial(&c).await;
    let permit = c
        .room
        .authorize_media(&c.host_session, old.media.media_id)
        .await
        .unwrap();
    c.room
        .apply(
            &c.host_session,
            command(old, &*clock, PlaybackCommand::Play),
        )
        .await
        .unwrap();
    let new_media = media();
    let new = c
        .room
        .replace_media(&c.host_session, new_media)
        .await
        .unwrap();
    permit.cancelled().await;
    assert_eq!(permit.check_active(), Err(RoomError::MediaChanged));
    assert_eq!(new.media, new_media);
    assert_eq!(new.revision.get(), 2);
    assert_eq!(new.playback.position_ms.get(), 0);
    assert_eq!(new.playback.status, PlaybackStatus::Paused);
    assert_eq!(new.playback.speed, Speed::Normal);
    assert!(matches!(
        c.room
            .authorize_media(&c.host_session, old.media.media_id)
            .await,
        Err(RoomError::MediaChanged)
    ));
    c.room
        .authorize_media(&c.host_session, new.media.media_id)
        .await
        .unwrap()
        .check_active()
        .unwrap();
    assert_eq!(
        c.room.replace_media(&c.host_session, new_media).await,
        Err(RoomError::InvalidInput)
    );
}
#[tokio::test]
async fn watches_coalesce_state_and_release_subscription_capacity() {
    let (c, clock) = setup();
    let mut receivers = Vec::new();
    for _ in 0..8 {
        receivers.push(c.room.subscribe(&c.host_session).await.unwrap());
    }
    assert!(matches!(
        c.room.subscribe(&c.host_session).await,
        Err(RoomError::RateLimited)
    ));
    let first = initial(&c).await;
    for _ in 0..3 {
        c.room
            .apply(
                &c.host_session,
                command(first, &*clock, PlaybackCommand::Play),
            )
            .await
            .unwrap();
    }
    for rx in &mut receivers {
        assert!(rx.has_changed().unwrap());
        assert_eq!(rx.borrow_and_update().revision.get(), 3);
    }
    receivers.pop();
    c.room.subscribe(&c.host_session).await.unwrap();
}

#[tokio::test]
async fn permit_identity_is_stable_across_independent_authorizations() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let a = c
        .room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let b = c
        .room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    assert_eq!(a.room_id(), guest.snapshot.room_id);
    assert_eq!(a.participant_id(), guest.participant_id);
    assert_eq!(a.media_id(), guest.snapshot.media.media_id);
    assert_eq!(a.room_id(), b.room_id());
    assert_eq!(a.participant_id(), b.participant_id());
    assert_eq!(a.media_id(), b.media_id());
}

#[tokio::test]
async fn already_waiting_permits_wake_on_explicit_revocation() {
    let (c, _) = setup();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let permit = c
        .room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let ((), kicked) = tokio::join!(
        permit.cancelled(),
        c.room.kick(&c.host_session, guest.participant_id)
    );
    kicked.unwrap();
    assert_eq!(permit.check_active(), Err(RoomError::Unauthorized));
    let host_permit = c
        .room
        .authorize_media(&c.host_session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let ((), closed) = tokio::join!(host_permit.cancelled(), c.room.close(&c.host_session));
    closed.unwrap();
    assert_eq!(host_permit.check_active(), Err(RoomError::RoomClosed));
}
