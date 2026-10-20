#![allow(dead_code)]
use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc, time::Duration};

use media_stream::{LoopbackServer, SelectedMedia};
use room_server::{CreatedRoom, RoomHandle};
use shared_protocol::{ClockEpochId, HostTime, MediaId, WireU64};
use sync_engine::ManualClock;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

pub struct Fixture {
    pub path: PathBuf,
    pub source: Arc<SelectedMedia>,
    pub created: CreatedRoom,
    pub endpoint: String,
}
impl Fixture {
    pub fn new(bytes: &[u8]) -> Self {
        let id = MediaId::new();
        let path = std::env::temp_dir().join(format!("loopback-test-{id}"));
        std::fs::write(&path, bytes).unwrap();
        let source = Arc::new(SelectedMedia::open(&path, id).unwrap());
        let clock = Arc::new(ManualClock::new(HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(0),
        }));
        let created = RoomHandle::create(source.descriptor(0), "host", clock).unwrap();
        Self {
            path,
            source,
            created,
            endpoint: format!("/media/{id}"),
        }
    }
    pub async fn start(&self) -> LoopbackServer {
        LoopbackServer::start(
            self.source.clone(),
            self.created.room.clone(),
            self.created.host_session.clone(),
        )
        .await
        .unwrap()
    }
    pub async fn request(&self, server: &LoopbackServer, method: &str, extra: &str) -> Response {
        request(server.address(), &format!("{method} {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\n{extra}Connection: close\r\n\r\n", self.endpoint, server.address(), server.capability().expose())).await
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

#[derive(Debug)]
pub struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub async fn request(address: SocketAddr, request: &str) -> Response {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut stream = TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).await.unwrap();
        parse(&bytes)
    })
    .await
    .expect("loopback response timed out")
}
pub fn parse(bytes: &[u8]) -> Response {
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .expect("HTTP headers");
    let text = std::str::from_utf8(&bytes[..split]).unwrap();
    let mut lines = text.split("\r\n");
    let status = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let headers = lines
        .map(|line| {
            let (k, v) = line.split_once(':').unwrap();
            (k.to_ascii_lowercase(), v.trim().to_owned())
        })
        .collect();
    Response {
        status,
        headers,
        body: bytes[split + 4..].to_vec(),
    }
}
pub fn no_metadata(response: &Response) {
    assert!(!response.headers.contains_key("content-range"));
    assert!(!response.headers.contains_key("accept-ranges"));
    assert_eq!(
        response.headers.get("content-length").map(String::as_str),
        Some("0")
    );
    assert!(response.body.is_empty());
    assert!(!response.headers.contains_key("location"));
    assert!(!response.headers.contains_key("access-control-allow-origin"));
}
