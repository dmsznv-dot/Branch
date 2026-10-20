#![cfg(feature = "test-support")]
mod http_support;
use http_support::Fixture;
use media_stream::{BodyProbe, CHUNK_BYTES, LocalRoomSession, LoopbackServer};
use room_server::RoomError;
use shared_protocol::{ClockEpochId, HostTime, WireU64};
use std::{sync::Arc, time::Duration};
use sync_engine::ManualClock;
use tokio::{io::AsyncWriteExt, net::TcpStream};

async fn cancel_between_chunks(kick: bool) {
    let fixture = Fixture::new(&vec![42; CHUNK_BYTES * 8]);
    let probe = Arc::new(BodyProbe::paused_after_first_chunk());
    let source =
        media_stream::SelectedMedia::open(&fixture.path, fixture.source.descriptor(0).media_id)
            .unwrap()
            .with_probe(probe.clone());
    let guest = fixture
        .created
        .room
        .join(&fixture.created.invitation, "guest")
        .await
        .unwrap();
    let server = LoopbackServer::start(
        Arc::new(source),
        fixture.created.room.clone(),
        guest.session.clone(),
    )
    .await
    .unwrap();
    let mut socket = TcpStream::connect(server.address()).await.unwrap();
    socket
        .write_all(
            format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n\r\n",
                fixture.endpoint,
                server.address(),
                server.capability().expose()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), probe.wait_between_chunks())
        .await
        .unwrap();
    assert_eq!(probe.reads(), 1);
    assert_eq!(probe.yields(), 1);
    if kick {
        fixture
            .created
            .room
            .kick(&fixture.created.host_session, guest.participant_id)
            .await
            .unwrap();
    } else {
        fixture
            .created
            .room
            .close(&fixture.created.host_session)
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(2), probe.wait_idle())
        .await
        .unwrap();
    assert_eq!(probe.reads(), 1, "revoked body performed a later read");
    assert_eq!(probe.yields(), 1, "revoked body yielded a later chunk");
    server.shutdown().await.unwrap();
    assert_eq!(probe.live_chunks(), 0);
}
#[tokio::test]
async fn kick_cancels_between_chunks() {
    cancel_between_chunks(true).await;
}
#[tokio::test]
async fn close_cancels_between_chunks() {
    cancel_between_chunks(false).await;
}

#[tokio::test]
async fn closing_drops_media_and_tasks_after_host_ttl() {
    let fixture = Fixture::new(b"selected original");
    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: ClockEpochId::new(),
        ticks_ms: WireU64::new(0),
    }));
    let session = LocalRoomSession::start(&fixture.path, 10_000, "host", clock.clone())
        .await
        .unwrap();
    session.check_ready().await.unwrap();
    let address = session.server().address();
    let source = session.source_weak();
    let room = session.room().clone();
    clock.advance_ms(299_000);
    let guest = room.join(session.invitation(), "late guest").await.unwrap();
    let permit = room
        .authorize_media(&guest.session, guest.snapshot.media.media_id)
        .await
        .unwrap();
    let mut rx = room.subscribe(&guest.session).await.unwrap();
    clock.advance_ms(2_000);
    session.shutdown().await.unwrap();
    assert_eq!(permit.check_active(), Err(RoomError::RoomClosed));
    assert!(rx.changed().await.is_err());
    assert!(source.upgrade().is_none());
    assert!(TcpStream::connect(address).await.is_err());
}

#[tokio::test]
async fn closing_drops_active_reader_and_owned_source() {
    let fixture = Fixture::new(&vec![23; CHUNK_BYTES * 8]);
    let clock = Arc::new(ManualClock::new(HostTime {
        epoch: ClockEpochId::new(),
        ticks_ms: WireU64::new(0),
    }));
    let probe = Arc::new(BodyProbe::paused_after_first_chunk());
    let session =
        LocalRoomSession::start_with_probe(&fixture.path, 10_000, "host", clock, probe.clone())
            .await
            .unwrap();
    session.check_ready().await.unwrap();
    let source = session.source_weak();
    let descriptor = session
        .room()
        .join(session.invitation(), "guest")
        .await
        .unwrap()
        .snapshot
        .media;
    let address = session.server().address();
    let mut socket = TcpStream::connect(address).await.unwrap();
    socket
        .write_all(
            format!(
                "GET /media/{} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\n\r\n",
                descriptor.media_id,
                session.server().capability().expose()
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), probe.wait_between_chunks())
        .await
        .unwrap();
    assert_eq!(probe.active_bodies(), 1);
    session.shutdown().await.unwrap();
    assert_eq!(probe.active_bodies(), 0);
    assert_eq!(probe.reads(), 1);
    assert_eq!(probe.yields(), 1);
    assert_eq!(probe.live_chunks(), 0);
    assert!(source.upgrade().is_none());
    assert!(TcpStream::connect(address).await.is_err());
}
