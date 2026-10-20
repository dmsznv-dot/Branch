use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hyper::server::conn::http1;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
use room_server::{InvitationToken, MediaPermit, RoomHandle, RoomOwner, SessionToken};
use shared_protocol::MediaId;
use subtle::ConstantTimeEq;
use sync_engine::Clock;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{Semaphore, watch},
    task::{JoinHandle, JoinSet},
};
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    RangeError, ReaderLimits, SelectedMedia, StreamError, body::media_body, resolve_range,
};

const MAX_CONNECTIONS: usize = 16;
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Separate 32-byte OS-random local capability, never placed in a URL or log.
/// Secret-bearing serialization and formatting are deliberately absent.
pub struct LoopbackCapability(String);
impl LoopbackCapability {
    fn generate() -> Result<Self, StreamError> {
        let mut bytes = [0_u8; 32];
        getrandom::fill(&mut bytes)
            .map_err(|_| StreamError::Permission(room_server::RoomError::EntropyUnavailable))?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn accepts(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(header::AUTHORIZATION).iter();
        let Some(value) = values.next() else {
            return false;
        };
        if values.next().is_some() {
            return false;
        }
        let Some(token) = value.as_bytes().strip_prefix(b"Bearer ") else {
            return false;
        };
        bool::from(token.ct_eq(self.0.as_bytes()))
    }
}
impl fmt::Debug for LoopbackCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LoopbackCapability([REDACTED])")
    }
}
impl fmt::Display for LoopbackCapability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[REDACTED]")
    }
}

struct ServerState {
    host: String,
    path: String,
    source: Arc<SelectedMedia>,
    room: RoomHandle,
    session: SessionToken,
    capability: LoopbackCapability,
    stopped: CancellationToken,
    connections: TaskTracker,
}

#[derive(Clone)]
struct ConnectionState {
    server: Arc<ServerState>,
    // Only installed after Host/capability/session/media authorization and
    // reader acquisition. The watcher also cancels a blocked socket write.
    permission: watch::Sender<Option<MediaPermit>>,
}

/// A loopback-only original-byte adapter. Its accept task owns every connection;
/// response readers are polled in those tasks and never spawned independently.
/// At most 16 connections are admitted, with a 5-second HTTP header deadline.
pub struct LoopbackServer {
    address: SocketAddr,
    state: Arc<ServerState>,
    task: Option<JoinHandle<Result<(), StreamError>>>,
}
impl LoopbackServer {
    pub async fn start(
        source: Arc<SelectedMedia>,
        room: RoomHandle,
        session: SessionToken,
    ) -> Result<Self, StreamError> {
        let capability = LoopbackCapability::generate()?;
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let state = Arc::new(ServerState {
            host: address.to_string(),
            path: format!("/media/{}", source.descriptor(0).media_id),
            source,
            room,
            session,
            capability,
            stopped: CancellationToken::new(),
            connections: TaskTracker::new(),
        });
        let task = tokio::spawn(serve(listener, state.clone()));
        Ok(Self {
            address,
            state,
            task: Some(task),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn capability(&self) -> &LoopbackCapability {
        &self.state.capability
    }

    /// Stops accepts, aborts and joins all owned connections, and drops their
    /// body leases. Bytes previously delivered to a client cannot be recalled.
    pub async fn shutdown(mut self) -> Result<(), StreamError> {
        self.shutdown_by(tokio::time::Instant::now() + Duration::from_secs(2))
            .await
    }

    async fn shutdown_by(&mut self, deadline: tokio::time::Instant) -> Result<(), StreamError> {
        self.state.stopped.cancel();
        self.state.connections.close();
        let result = if let Some(mut task) = self.task.take() {
            match tokio::time::timeout_at(deadline, &mut task).await {
                Ok(result) => result
                    .map_err(|_| StreamError::Io(std::io::Error::other("loopback task failed")))
                    .and_then(std::convert::identity),
                Err(_) => {
                    task.abort();
                    // Join the aborted accept task; its JoinSet aborts every
                    // connection. The tracker below also waits for those drops.
                    let _ = task.await;
                    Err(StreamError::ShutdownTimeout)
                }
            }
        } else {
            Ok(())
        };
        self.state.connections.wait().await;
        result
    }
}
impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.state.stopped.cancel();
    }
}

/// Owns the room, selected descriptor and all local HTTP tasks.
/// Explicit shutdown revokes authority before joining readers and dropping media.
pub struct LocalRoomSession {
    room: RoomHandle,
    owner: RoomOwner,
    invitation: InvitationToken,
    source: Arc<SelectedMedia>,
    server: LoopbackServer,
}
impl LocalRoomSession {
    pub async fn start(
        file: &Path,
        duration_ms: u64,
        host_name: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StreamError> {
        let source = Arc::new(SelectedMedia::open(file, MediaId::new())?);
        Self::start_source(source, duration_ms, host_name, clock).await
    }

    #[cfg(feature = "test-support")]
    pub async fn start_with_probe(
        file: &Path,
        duration_ms: u64,
        host_name: &str,
        clock: Arc<dyn Clock>,
        probe: Arc<crate::BodyProbe>,
    ) -> Result<Self, StreamError> {
        let source = Arc::new(SelectedMedia::open(file, MediaId::new())?.with_probe(probe));
        Self::start_source(source, duration_ms, host_name, clock).await
    }

    async fn start_source(
        source: Arc<SelectedMedia>,
        duration_ms: u64,
        host_name: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StreamError> {
        let created = RoomHandle::create(source.descriptor(duration_ms), host_name, clock)?;
        let server =
            match LoopbackServer::start(source.clone(), created.room.clone(), created.host_session)
                .await
            {
                Ok(server) => server,
                Err(error) => {
                    created.owner.close().await;
                    return Err(error);
                }
            };
        Ok(Self {
            room: created.room,
            owner: created.owner,
            invitation: created.invitation,
            source,
            server,
        })
    }
    pub fn room(&self) -> &RoomHandle {
        &self.room
    }
    pub fn invitation(&self) -> &InvitationToken {
        &self.invitation
    }
    pub fn server(&self) -> &LoopbackServer {
        &self.server
    }
    #[cfg(feature = "test-support")]
    pub fn source_weak(&self) -> std::sync::Weak<SelectedMedia> {
        Arc::downgrade(&self.source)
    }

    /// A bounded authenticated HEAD over the real HTTP parser and listener.
    pub async fn check_ready(&self) -> Result<(), StreamError> {
        let check = async {
            let address = self.server.address();
            let mut stream = TcpStream::connect(address).await?;
            let request = format!(
                "HEAD /media/{} HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                self.source.descriptor(0).media_id,
                self.server.capability().expose()
            );
            stream.write_all(request.as_bytes()).await?;
            let mut response = Vec::new();
            // A hostile or broken adapter cannot turn a health check into an
            // unbounded read. HEAD must have headers only and exact metadata.
            stream.take(8193).read_to_end(&mut response).await?;
            let text = std::str::from_utf8(&response).map_err(|_| StreamError::ReadinessFailed)?;
            let Some(headers) = text.strip_suffix("\r\n\r\n") else {
                return Err(StreamError::ReadinessFailed);
            };
            let mut lines = headers.split("\r\n");
            if lines.next() != Some("HTTP/1.1 200 OK") || response.len() > 8192 {
                return Err(StreamError::ReadinessFailed);
            }
            let expected = self.source.descriptor(0).size_bytes.get().to_string();
            if !lines.any(|line| {
                line.split_once(':').is_some_and(|(name, value)| {
                    name.eq_ignore_ascii_case("content-length") && value.trim() == expected
                })
            }) {
                return Err(StreamError::ReadinessFailed);
            }
            Ok(())
        };
        tokio::time::timeout(Duration::from_secs(2), check)
            .await
            .map_err(|_| StreamError::ReadinessFailed)?
    }

    pub async fn shutdown(self) -> Result<(), StreamError> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let Self {
            owner,
            mut server,
            source,
            ..
        } = self;
        let closed = tokio::time::timeout_at(deadline, owner.close()).await;
        let stopped = server.shutdown_by(deadline).await;
        drop(server);
        drop(source);
        if closed.is_err() {
            return Err(StreamError::ShutdownTimeout);
        }
        stopped
    }
}

async fn serve(listener: TcpListener, state: Arc<ServerState>) -> Result<(), StreamError> {
    let mut connections = JoinSet::new();
    let admission = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let result = loop {
        tokio::select! {
            biased;
            () = state.stopped.cancelled() => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(value) => value, Err(error) => break Err(error.into()) };
                // Reject before allocating any per-connection channel/router
                // or spawning work. The owned slot lives through all HTTP I/O.
                let Ok(slot) = admission.clone().try_acquire_owned() else {
                    drop(stream);
                    continue;
                };
                let (permission, receiver) = watch::channel(None);
                let router = Router::new().fallback(handle).with_state(ConnectionState { server: state.clone(), permission });
                let service = TowerToHyperService::new(router);
                let stopped = state.stopped.clone();
                connections.spawn(state.connections.track_future(async move {
                    let _slot = slot;
                    let mut builder = http1::Builder::new();
                    // Hyper enforces this while parsing, before the adapter.
                    builder
                        .timer(TokioTimer::new())
                        .header_read_timeout(HEADER_READ_TIMEOUT)
                        .max_buf_size(8192);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::select! {
                        biased;
                        () = stopped.cancelled() => {},
                        () = reader_revoked(receiver) => {},
                        // Never log a protocol error: it may contain input.
                        _ = connection => {},
                    }
                }));
            }
        }
    };
    connections.shutdown().await;
    result
}

fn empty(status: StatusCode) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_LENGTH, "0")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .expect("static response")
}

async fn handle(State(connection): State<ConnectionState>, request: Request) -> Response {
    let state = connection.server;
    let headers = request.headers();
    let mut hosts = headers.get_all(header::HOST).iter();
    if hosts.next().map(|value| value.as_bytes()) != Some(state.host.as_bytes())
        || hosts.next().is_some()
    {
        return empty(StatusCode::FORBIDDEN);
    }
    if !state.capability.accepts(headers) {
        return empty(StatusCode::UNAUTHORIZED);
    }
    // Compare the literal path; never decode it into a filesystem location.
    if request.uri().path() != state.path {
        return empty(StatusCode::NOT_FOUND);
    }
    let permit = match state
        .room
        .authorize_media(&state.session, state.source.descriptor(0).media_id)
        .await
    {
        Ok(permit) => permit,
        Err(_) => return empty(StatusCode::UNAUTHORIZED),
    };
    if state.stopped.is_cancelled() || permit.check_active().is_err() {
        return empty(StatusCode::UNAUTHORIZED);
    }
    let head = request.method() == Method::HEAD;
    if request.method() != Method::GET && !head {
        let mut response = empty(StatusCode::METHOD_NOT_ALLOWED);
        response.headers_mut().insert(
            header::ALLOW,
            "GET, HEAD".parse().expect("static allow header"),
        );
        return response;
    }
    // Only authenticated, active media may reveal its descriptor size.
    let size = state.source.descriptor(0).size_bytes.get();
    let range = if head {
        None
    } else {
        let mut ranges = headers.get_all(header::RANGE).iter();
        let value = ranges.next();
        if ranges.next().is_some() {
            return empty(StatusCode::BAD_REQUEST);
        }
        let value = match value.map(|value| value.to_str()).transpose() {
            Ok(value) => value,
            Err(_) => return empty(StatusCode::BAD_REQUEST),
        };
        match resolve_range(value, size) {
            Ok(range) => range,
            Err(RangeError::Unsatisfiable) => {
                let mut response = empty(StatusCode::RANGE_NOT_SATISFIABLE);
                response.headers_mut().insert(
                    header::CONTENT_RANGE,
                    format!("bytes */{size}").parse().expect("numeric range"),
                );
                return response;
            }
            Err(_) => return empty(StatusCode::BAD_REQUEST),
        }
    };
    let start = range.map_or(0, |range| range.start);
    let len = range.map_or(size, |range| range.len);
    let body = if head {
        Body::empty()
    } else {
        let lease = match ReaderLimits::default().try_acquire(&permit) {
            Ok(lease) => lease,
            Err(StreamError::ReaderLimit) => return empty(StatusCode::TOO_MANY_REQUESTS),
            Err(_) => return empty(StatusCode::UNAUTHORIZED),
        };
        connection.permission.send_replace(Some(permit.clone()));
        media_body(
            state.source.clone(),
            permit,
            lease,
            state.stopped.clone(),
            start,
            len,
        )
    };
    let mut response = Response::builder()
        .status(if range.is_some() {
            StatusCode::PARTIAL_CONTENT
        } else {
            StatusCode::OK
        })
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "no-store");
    if range.is_some() {
        response = response.header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{}/{size}", start + len - 1),
        );
    }
    response.body(body).expect("numeric response headers")
}

async fn reader_revoked(mut permission: watch::Receiver<Option<MediaPermit>>) {
    loop {
        let permit = permission.borrow_and_update().clone();
        if let Some(permit) = permit {
            permit.cancelled().await;
            return;
        }
        if permission.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_protocol::{ClockEpochId, HostTime, MediaDescriptor, MediaId, WireU64};
    use std::{future::Future, path::PathBuf, task::Poll};
    use sync_engine::ManualClock;

    struct ConnectionFixture(PathBuf);
    impl ConnectionFixture {
        async fn start() -> (Self, LoopbackServer) {
            let id = MediaId::new();
            let path = std::env::temp_dir().join(format!("connection-admission-{id}"));
            std::fs::write(&path, b"selected").unwrap();
            let source = Arc::new(SelectedMedia::open(&path, id).unwrap());
            let clock = Arc::new(ManualClock::new(HostTime {
                epoch: ClockEpochId::new(),
                ticks_ms: WireU64::new(0),
            }));
            let created = RoomHandle::create(source.descriptor(0), "host", clock).unwrap();
            let server = LoopbackServer::start(source, created.room, created.host_session)
                .await
                .unwrap();
            (Self(path), server)
        }
    }
    impl Drop for ConnectionFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    async fn wait_connections(server: &LoopbackServer, expected: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while server.state.connections.len() != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("connection tasks did not reach the expected count");
    }

    async fn authenticated_get(server: &LoopbackServer) {
        tokio::time::timeout(Duration::from_secs(2), async {
            let mut stream = TcpStream::connect(server.address()).await.unwrap();
            let request = format!(
                "GET {} HTTP/1.1\r\nHost: {}\r\nAuthorization: Bearer {}\r\nConnection: close\r\n\r\n",
                server.state.path, server.address(), server.capability().expose()
            );
            stream.write_all(request.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));
            assert!(response.ends_with(b"\r\n\r\nselected"));
        })
        .await
        .expect("authenticated request did not resume");
    }

    #[tokio::test]
    async fn connection_admission_caps_partial_headers_and_reclaims_close_and_error_slots() {
        let (_fixture, server) = ConnectionFixture::start().await;
        let mut held = Vec::new();
        for _ in 0..16 {
            let mut stream = TcpStream::connect(server.address()).await.unwrap();
            stream.write_all(b"GET /media/").await.unwrap();
            held.push(stream);
        }
        wait_connections(&server, 16).await;
        for _ in 0..3 {
            let mut excess = TcpStream::connect(server.address()).await.unwrap();
            let mut byte = [0];
            let result = tokio::time::timeout(Duration::from_secs(2), excess.read(&mut byte))
                .await
                .expect("excess socket was retained instead of rejected");
            assert!(matches!(result, Ok(0) | Err(_)));
            assert_eq!(server.state.connections.len(), 16);
        }
        drop(held.pop());
        wait_connections(&server, 15).await;
        authenticated_get(&server).await;
        wait_connections(&server, 15).await;

        // Finish one admitted request with an invalid request line, forcing
        // Hyper's protocol-error exit rather than a client-close exit.
        let mut malformed = held.pop().unwrap();
        malformed.write_all(b"\0\r\n\r\n").await.unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(Duration::from_secs(2), malformed.read_to_end(&mut response))
            .await
            .expect("malformed request did not close")
            .unwrap();
        assert!(response.starts_with(b"HTTP/1.1 400 Bad Request\r\n"));
        wait_connections(&server, 14).await;
        authenticated_get(&server).await;
        let tracker = server.state.connections.clone();
        server.shutdown().await.unwrap();
        assert!(tracker.is_empty());
    }

    #[tokio::test]
    async fn connection_header_timeout_closes_idle_and_partial_requests_and_reclaims_slots() {
        let (_fixture, server) = ConnectionFixture::start().await;
        let started = tokio::time::Instant::now();
        let mut held = Vec::new();
        for index in 0..16 {
            let mut stream = TcpStream::connect(server.address()).await.unwrap();
            if index % 2 == 0 {
                stream.write_all(b"GET /media/").await.unwrap();
            }
            held.push(stream);
        }
        wait_connections(&server, 16).await;
        // Await observable EOF with a failure bound, never sleep to guess
        // when the parser or timer has run. Exercise the production 5s timer.
        tokio::time::timeout(Duration::from_secs(8), async {
            for mut stream in held {
                let mut response = Vec::new();
                stream.read_to_end(&mut response).await.unwrap();
                assert!(
                    response.is_empty(),
                    "incomplete headers revealed a response"
                );
            }
        })
        .await
        .expect("idle/partial headers were not closed by the header timer");
        assert!(started.elapsed() >= Duration::from_secs(5));
        wait_connections(&server, 0).await;
        authenticated_get(&server).await;
        server.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn connection_watcher_wakes_on_revocation_without_a_body_pull() {
        let media_id = MediaId::new();
        let descriptor = MediaDescriptor {
            media_id,
            size_bytes: WireU64::new(10),
            duration_ms: WireU64::new(0),
        };
        let clock = Arc::new(ManualClock::new(HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(0),
        }));
        let created = RoomHandle::create(descriptor, "host", clock).unwrap();
        let permit = created
            .room
            .authorize_media(&created.host_session, media_id)
            .await
            .unwrap();
        let (tx, rx) = tokio::sync::watch::channel(None);
        let watcher = reader_revoked(rx);
        tokio::pin!(watcher);
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(watcher.as_mut().poll(cx).is_pending())).await
        );
        tx.send(Some(permit)).unwrap();
        assert!(
            std::future::poll_fn(|cx| Poll::Ready(watcher.as_mut().poll(cx).is_pending())).await
        );
        created.room.close(&created.host_session).await.unwrap();
        assert!(std::future::poll_fn(|cx| Poll::Ready(watcher.as_mut().poll(cx).is_ready())).await);
    }
    #[tokio::test]
    async fn expired_shutdown_deadline_aborts_and_joins_owned_tasks() {
        let media_id = MediaId::new();
        let path = std::env::temp_dir().join(format!("shutdown-deadline-{media_id}"));
        std::fs::write(&path, b"selected").unwrap();
        let source = Arc::new(SelectedMedia::open(&path, media_id).unwrap());
        let clock = Arc::new(ManualClock::new(HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(0),
        }));
        let created = RoomHandle::create(source.descriptor(0), "host", clock).unwrap();
        let mut server = LoopbackServer::start(source.clone(), created.room, created.host_session)
            .await
            .unwrap();
        server.state.stopped.cancel();
        server.task.take().unwrap().await.unwrap().unwrap();
        let tracker = server.state.connections.clone();
        let weak = Arc::downgrade(&source);
        let (entered, ready) = tokio::sync::oneshot::channel();
        // Substitute a stalled accept task which owns a tracked connection.
        // The expired deadline must abort it, then join its child teardown.
        server.task = Some(tokio::spawn(async move {
            let mut children = JoinSet::new();
            children.spawn(tracker.track_future(async move {
                let _owned_source = source;
                entered.send(()).unwrap();
                std::future::pending::<()>().await;
            }));
            std::future::pending::<Result<(), StreamError>>().await
        }));
        ready.await.unwrap();
        assert!(matches!(
            server.shutdown_by(tokio::time::Instant::now()).await,
            Err(StreamError::ShutdownTimeout)
        ));
        assert!(server.state.connections.is_empty());
        drop(server);
        assert!(
            weak.upgrade().is_none(),
            "shutdown detached a child retaining media"
        );
        std::fs::remove_file(path).unwrap();
    }
}
