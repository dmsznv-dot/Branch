mod http_support;
use http_support::Fixture;

#[tokio::test]
async fn get_exact_range() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    let response = fixture
        .request(&server, "GET", "Range: bytes=2-4\r\n")
        .await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, b"234");
    assert_eq!(response.headers["content-range"], "bytes 2-4/10");
    assert_eq!(response.headers["content-length"], "3");
    assert_eq!(response.headers["accept-ranges"], "bytes");
    assert_eq!(response.headers["content-type"], "application/octet-stream");
    assert_eq!(response.headers["cache-control"], "no-store");
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn head_has_full_length_and_no_body() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    for range in ["Range: bytes=2-4\r\n", "Range: malformed\r\n", ""] {
        let response = fixture.request(&server, "HEAD", range).await;
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["content-length"], "10");
        assert!(!response.headers.contains_key("content-range"));
        assert_eq!(response.headers["content-type"], "application/octet-stream");
        assert_eq!(response.headers["cache-control"], "no-store");
        assert_eq!(response.headers["accept-ranges"], "bytes");
        assert!(response.body.is_empty());
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn full_and_empty_get() {
    for bytes in [b"0123456789".as_slice(), b"".as_slice()] {
        let fixture = Fixture::new(bytes);
        let server = fixture.start().await;
        let response = fixture.request(&server, "GET", "").await;
        assert_eq!(response.status, 200);
        assert_eq!(response.body, bytes);
        assert_eq!(response.headers["content-length"], bytes.len().to_string());
        assert!(!response.headers.contains_key("content-range"));
        server.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn invalid_and_unsatisfiable_ranges() {
    let fixture = Fixture::new(b"0123456789");
    let server = fixture.start().await;
    for range in [
        "bytes=10-",
        "bytes=0-0,2-4",
        "invalid",
        "bytes=18446744073709551616-",
        "bytes=2-4\r\nRange: bytes=5-6",
    ] {
        let response = fixture
            .request(&server, "GET", &format!("Range: {range}\r\n"))
            .await;
        assert_eq!(
            response.status,
            if range == "bytes=10-" { 416 } else { 400 }
        );
        assert!(response.body.is_empty());
        if response.status == 416 {
            assert_eq!(response.headers["content-range"], "bytes */10");
        } else {
            assert!(!response.headers.contains_key("content-range"));
        }
    }
    server.shutdown().await.unwrap();
}

#[tokio::test]
async fn bounded_chunks_preserve_large_original_bytes() {
    let bytes: Vec<_> = (0..250_000).map(|n| (n % 251) as u8).collect();
    let fixture = Fixture::new(&bytes);
    let server = fixture.start().await;
    let response = fixture
        .request(&server, "GET", "Range: bytes=50000-220000\r\n")
        .await;
    assert_eq!(response.status, 206);
    assert_eq!(response.body, bytes[50000..=220000]);
    server.shutdown().await.unwrap();
}
