use std::{
    fmt,
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
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
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use room_server::{MediaPermit, RoomHandle, SessionToken};
use subtle::ConstantTimeEq;
use tokio::{
    net::TcpListener,
    sync::watch,
    task::{JoinHandle, JoinSet},
};
use tokio_util::sync::CancellationToken;

use crate::{
    RangeError, ReaderLimits, SelectedMedia, StreamError, body::media_body, resolve_range,
};

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
        self.state.stopped.cancel();
        if let Some(task) = self.task.take() {
            task.await
                .map_err(|_| StreamError::Io(std::io::Error::other("loopback task failed")))??;
        }
        Ok(())
    }
}
impl Drop for LoopbackServer {
    fn drop(&mut self) {
        self.state.stopped.cancel();
    }
}

async fn serve(listener: TcpListener, state: Arc<ServerState>) -> Result<(), StreamError> {
    let mut connections = JoinSet::new();
    let result = loop {
        tokio::select! {
            biased;
            () = state.stopped.cancelled() => break Ok(()),
            Some(_) = connections.join_next(), if !connections.is_empty() => {},
            accepted = listener.accept() => {
                let (stream, _) = match accepted { Ok(value) => value, Err(error) => break Err(error.into()) };
                let (permission, receiver) = watch::channel(None);
                let router = Router::new().fallback(handle).with_state(ConnectionState { server: state.clone(), permission });
                let service = TowerToHyperService::new(router);
                let stopped = state.stopped.clone();
                connections.spawn(async move {
                    let mut builder = http1::Builder::new();
                    // Hyper enforces this while parsing, before the adapter.
                    builder.max_buf_size(8192);
                    let connection = builder.serve_connection(TokioIo::new(stream), service);
                    tokio::select! {
                        biased;
                        () = stopped.cancelled() => {},
                        () = reader_revoked(receiver) => {},
                        // Never log a protocol error: it may contain input.
                        _ = connection => {},
                    }
                });
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
    use std::{future::Future, task::Poll};
    use sync_engine::ManualClock;

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
}
