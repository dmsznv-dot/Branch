#![cfg(feature = "test-support")]
mod http_support;
use http_support::Fixture;
use media_stream::{BodyProbe, CHUNK_BYTES, LoopbackServer, SelectedMedia};
use std::{sync::Arc, time::Duration};
use tokio::{io::AsyncWriteExt, net::TcpStream};

#[tokio::test]
async fn slow_reader_has_bounded_queue() {
    let fixture = Fixture::new(&vec![7; CHUNK_BYTES * 128]);
    let probe = Arc::new(BodyProbe::paused_after_first_chunk());
    let source = Arc::new(
        SelectedMedia::open(&fixture.path, fixture.source.descriptor(0).media_id)
            .unwrap()
            .with_probe(probe.clone()),
    );
    let weak = Arc::downgrade(&source);
    let server = LoopbackServer::start(
        source,
        fixture.created.room.clone(),
        fixture.created.host_session.clone(),
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
    assert!(probe.peak_chunks() <= 3);
    server.shutdown().await.unwrap();
    assert_eq!(probe.active_bodies(), 0);
    assert_eq!(probe.live_chunks(), 0);
    assert!(weak.upgrade().is_none());
}

#[tokio::test]
async fn paused_pull_consumer_allocates_only_requested_chunks() {
    use futures_util::StreamExt;
    let fixture = Fixture::new(&vec![19; CHUNK_BYTES * 128]);
    let probe = Arc::new(BodyProbe::default());
    let source = Arc::new(
        SelectedMedia::open(&fixture.path, fixture.source.descriptor(0).media_id)
            .unwrap()
            .with_probe(probe.clone()),
    );
    let weak = Arc::downgrade(&source);
    let permit = fixture
        .created
        .room
        .authorize_media(&fixture.created.host_session, source.descriptor(0).media_id)
        .await
        .unwrap();
    let mut body = media_stream::test_support::body(source, permit)
        .unwrap()
        .into_data_stream();
    let first = body.next().await.unwrap().unwrap();
    assert_eq!(first.len(), CHUNK_BYTES);
    // Give any incorrectly introduced producer a chance to run. The actual
    // bound is counted from chunk allocation through final Bytes drop.
    for _ in 0..16 {
        tokio::task::yield_now().await;
    }
    assert_eq!(probe.reads(), 1);
    assert_eq!(probe.live_chunks(), 1);
    assert_eq!(probe.peak_chunks(), 1);
    fixture
        .created
        .room
        .close(&fixture.created.host_session)
        .await
        .unwrap();
    assert!(body.next().await.unwrap().is_err());
    assert!(body.next().await.is_none());
    assert_eq!(probe.active_bodies(), 0);
    drop(first);
    assert_eq!(probe.live_chunks(), 0);
    assert!(weak.upgrade().is_none());
}
