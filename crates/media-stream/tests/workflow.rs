mod http_support;
use http_support::Fixture;
use media_stream::{LoopbackServer, SelectedMedia};
use room_server::{RoomError, RoomHandle};
use shared_protocol::*;
use std::sync::Arc;
use sync_engine::{Clock, ManualClock, position_at};

fn command(snapshot: RoomSnapshot, clock: &dyn Clock, action: PlaybackCommand) -> CommandEnvelope {
    CommandEnvelope {
        room_id: snapshot.room_id,
        media_id: snapshot.media.media_id,
        command_id: CommandId::new(),
        issued_at: clock.now(),
        action,
    }
}
#[tokio::test]
async fn room_to_http_workflow() {
    let fixture = Fixture::new(b"0123456789");
    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: ClockEpochId::new(),
        ticks_ms: WireU64::new(0),
    }));
    let c = RoomHandle::create(fixture.source.descriptor(20_000), "host", clock.clone()).unwrap();
    let guest = c.room.join(&c.invitation, "guest").await.unwrap();
    let snap = c
        .room
        .set_control(&c.host_session, ControlMode::HostOnly)
        .await
        .unwrap();
    assert_eq!(
        c.room
            .apply(
                &guest.session,
                command(
                    snap,
                    &*clock,
                    PlaybackCommand::Seek {
                        position_ms: WireU64::new(100)
                    }
                )
            )
            .await,
        Err(RoomError::Forbidden)
    );
    let snap = c
        .room
        .set_control(&c.host_session, ControlMode::Everyone)
        .await
        .unwrap();
    c.room
        .apply(
            &guest.session,
            command(
                snap,
                &*clock,
                PlaybackCommand::Seek {
                    position_ms: WireU64::new(100),
                },
            ),
        )
        .await
        .unwrap();
    c.room
        .apply(
            &c.host_session,
            command(snap, &*clock, PlaybackCommand::Play),
        )
        .await
        .unwrap();
    clock.advance_ms(2_000);
    let snap = c.room.snapshot(&c.host_session).await.unwrap();
    assert_eq!(
        position_at(&snap.playback, clock.now(), snap.media.duration_ms.get()).unwrap(),
        2100
    );
    let late = c.room.join(&c.invitation, "late").await.unwrap();
    assert_eq!(late.snapshot, snap);
    let server = LoopbackServer::start(
        fixture.source.clone(),
        c.room.clone(),
        guest.session.clone(),
    )
    .await
    .unwrap();
    assert_eq!(fixture.request(&server, "HEAD", "").await.status, 200);
    assert_eq!(
        fixture
            .request(&server, "GET", "Range: bytes=2-4\r\n")
            .await
            .body,
        b"234"
    );
    c.room
        .kick(&c.host_session, guest.participant_id)
        .await
        .unwrap();
    assert_eq!(fixture.request(&server, "GET", "").await.status, 401);
    c.room.close(&c.host_session).await.unwrap();
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn old_media_cannot_serve_new_generation() {
    let fixture = Fixture::new(b"0123456789");
    let old = fixture.start().await;
    let new_source = Arc::new(SelectedMedia::open(&fixture.path, MediaId::new()).unwrap());
    fixture
        .created
        .room
        .replace_media(&fixture.created.host_session, new_source.descriptor(0))
        .await
        .unwrap();
    assert_eq!(fixture.request(&old, "GET", "").await.status, 401);
    let new = LoopbackServer::start(
        new_source.clone(),
        fixture.created.room.clone(),
        fixture.created.host_session.clone(),
    )
    .await
    .unwrap();
    let path = format!("/media/{}", new_source.descriptor(0).media_id);
    let stale = http_support::request(new.address(), &format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", new.address(),old.capability().expose())).await;
    assert_eq!(stale.status, 401);
    let good = http_support::request(new.address(), &format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", new.address(),new.capability().expose())).await;
    assert_eq!(good.body, b"0123456789");
    old.shutdown().await.unwrap();
    new.shutdown().await.unwrap();
}
