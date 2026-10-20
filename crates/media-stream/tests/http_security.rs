mod http_support;
use http_support::{Fixture, no_metadata, request};
use media_stream::{LoopbackServer, ReaderLimits, SelectedMedia};
use shared_protocol::MediaId;
use std::sync::Arc;

#[tokio::test]
async fn unauthorized_head_and_get_do_not_leak_size() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    for method in ["GET", "HEAD"] {
        for auth in [
            "",
            "Authorization: Bearer stale\r\n",
            "Authorization: Bearer stale\r\nAuthorization: Bearer stale\r\n",
        ] {
            let response = request(server.address(), &format!("{method} {} HTTP/1.1\r\nHost: {}\r\n{auth}Range: bytes=100-\r\nConnection: close\r\n\r\n", fixture.endpoint, server.address())).await;
            assert_eq!(response.status, 401);
            no_metadata(&response);
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn wrong_host_is_forbidden_before_authentication() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    for host in [
        format!("localhost:{}", server.address().port()),
        "127.0.0.1:1".to_owned(),
        "evil.example".to_owned(),
        format!("127.0.0.1:{}.", server.address().port()),
    ] {
        let response = request(
            server.address(),
            &format!(
                "GET {} HTTP/1.1\r\nHost: {host}\r\nRange: bytes=100-\r\nConnection: close\r\n\r\n",
                fixture.endpoint
            ),
        )
        .await;
        assert_eq!(response.status, 403);
        no_metadata(&response);
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn only_exact_media_route_and_read_methods() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    for path in [
        "/",
        "/media/../../etc/passwd",
        "/media/%2e%2e%2fetc%2fpasswd",
        "/media/wrong",
        "/media/",
    ] {
        let response = request(server.address(), &format!("GET {path} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", server.address(), server.capability().expose())).await;
        assert_eq!(response.status, 404);
        no_metadata(&response);
    }
    let response = fixture.request(&server, "POST", "").await;
    assert_eq!(response.status, 405);
    assert_eq!(response.headers["allow"], "GET, HEAD");
    no_metadata(&response);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn capabilities_are_distinct_redacted_and_not_interchangeable() {
    let fixture = Fixture::new(b"0123456789");
    let first = fixture.start().await;
    let second = fixture.start().await;
    let secret = first.capability().expose();
    assert_eq!(secret.len(), 43); // 32 random bytes in unpadded base64url.
    assert!(secret != second.capability().expose());
    assert!(secret != fixture.created.host_session.expose());
    assert!(secret != fixture.created.invitation.expose());
    assert!(!format!("{:?}", first.capability()).contains(secret));
    assert!(!format!("{}", first.capability()).contains(secret));
    let response = request(second.address(), &format!("GET {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {secret}\r\nConnection: close\r\n\r\n", fixture.endpoint, second.address())).await;
    assert_eq!(response.status, 401);
    no_metadata(&response);
    first.shutdown().await.unwrap();
    second.shutdown().await.unwrap();
}

#[tokio::test]
async fn origin_gets_no_cors_or_redirect() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    let response = fixture
        .request(&server, "GET", "Origin: https://evil.example\r\n")
        .await;
    assert_eq!(response.status, 200);
    assert!(!response.headers.contains_key("access-control-allow-origin"));
    assert!(!response.headers.contains_key("location"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn transport_rejects_headers_over_8192_bytes() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    let response = fixture
        .request(&server, "GET", &format!("Range: {}\r\n", "x".repeat(9000)))
        .await;
    assert_eq!(response.status, 431);
    assert!(!response.headers.contains_key("content-range"));
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn closed_room_rejects_old_local_capability() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    fixture
        .created
        .room
        .close(&fixture.created.host_session)
        .await
        .unwrap();
    for method in ["GET", "HEAD"] {
        let response = fixture
            .request(&server, method, "Range: bytes=100-\r\n")
            .await;
        assert_eq!(response.status, 401);
        no_metadata(&response);
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn mismatched_source_room_rejected_before_metadata_or_io() {
    let fixture = Fixture::new(b"0123456789");
    let source = Arc::new(SelectedMedia::open(&fixture.path, MediaId::new()).unwrap());
    let endpoint = format!("/media/{}", source.descriptor(0).media_id);
    let server = LoopbackServer::start(
        source,
        fixture.created.room.clone(),
        fixture.created.host_session.clone(),
    )
    .await
    .unwrap();
    let response = request(server.address(), &format!("HEAD {endpoint} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n", server.address(), server.capability().expose())).await;
    assert_eq!(response.status, 401);
    no_metadata(&response);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn reader_quotas_are_shared_with_other_adapters_and_head_uses_no_reader() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    let permit = fixture
        .created
        .room
        .authorize_media(
            &fixture.created.host_session,
            fixture.source.descriptor(0).media_id,
        )
        .await
        .unwrap();
    let first = ReaderLimits::default().try_acquire(&permit).unwrap();
    let second = ReaderLimits::default().try_acquire(&permit).unwrap();
    let response = fixture.request(&server, "GET", "").await;
    assert_eq!(response.status, 429);
    no_metadata(&response);
    assert_eq!(fixture.request(&server, "HEAD", "").await.status, 200);
    drop(first);
    assert_eq!(
        fixture.request(&server, "GET", "").await.body,
        b"0123456789"
    );
    drop(second);
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn shutdown_joins_idle_connections_and_releases_the_source() {
    use std::time::Duration;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpStream,
    };
    let fixture = Fixture::new(b"0123456789");
    let weak = Arc::downgrade(&fixture.source);
    let server = fixture.start().await;
    let address = server.address();
    let mut idle = TcpStream::connect(address).await.unwrap();
    // A partial request holds an owned connection task, without a body permit.
    idle.write_all(b"GET /media/").await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), server.shutdown())
        .await
        .unwrap()
        .unwrap();
    let mut buffer = [0; 32];
    match tokio::time::timeout(Duration::from_secs(2), idle.read(&mut buffer))
        .await
        .unwrap()
    {
        Ok(count) => assert_eq!(count, 0),
        Err(error) => assert!(matches!(
            error.kind(),
            std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
        )),
    }
    assert!(TcpStream::connect(address).await.is_err());
    assert_eq!(
        weak.strong_count(),
        1,
        "shutdown returned with an HTTP owner retaining the source"
    );
}
