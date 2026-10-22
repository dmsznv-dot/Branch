use std::{collections::HashMap, sync::Arc, time::Duration};

use crate::invitation::InvitationVerifier;
use axum::{
    Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use shared_protocol::{MediaFingerprint, ParticipantId, PlaybackCommand, RoomId, RoomSnapshot};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector,
    tungstenite::client::IntoClientRequest,
    tungstenite::protocol::{Message as ClientMessage, WebSocketConfig},
};
use url::Url;

const MAX_SIGNAL_MESSAGE_BYTES: usize = 64 * 1024;
const MAX_GUESTS: usize = 4;
const MAX_ROOMS: usize = 128;
const MAX_CONNECTIONS: usize = 512;
const MAX_ROOM_JOINS_PER_MINUTE: u64 = 10;
const FIRST_MESSAGE_TIMEOUT: Duration = Duration::from_secs(5);
const ROOM_IDLE_TTL: Duration = Duration::from_secs(120);
const SIGNAL_PATH: &str = "/v1/signal";

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ClientFrame {
    Register {
        room_id: RoomId,
        verifier: InvitationVerifier,
        fingerprint: MediaFingerprint,
    },
    Join {
        room_id: RoomId,
        verifier: InvitationVerifier,
        fingerprint: MediaFingerprint,
    },
    PublishSnapshot {
        snapshot: RoomSnapshot,
    },
    PlaybackCommand {
        connection_id: ParticipantId,
        action: PlaybackCommand,
    },
    ApproveGuest {
        connection_id: ParticipantId,
    },
    RejectGuest {
        connection_id: ParticipantId,
    },
    KeepAlive,
    Cancel {
        connection_id: ParticipantId,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ServerFrame {
    Registered,
    JoinAccepted {
        connection_id: ParticipantId,
    },
    GuestJoined {
        connection_id: ParticipantId,
    },
    GuestCommand {
        connection_id: ParticipantId,
        action: PlaybackCommand,
    },
    Snapshot {
        snapshot: RoomSnapshot,
    },
    GuestDisconnected {
        connection_id: ParticipantId,
    },
    RoomClosed,
    Rejected {
        code: SignalErrorCode,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SignalErrorCode {
    InvalidMessage,
    RoomUnavailable,
    RoomFull,
    RateLimited,
    DuplicateHost,
    InvalidInvitation,
    MediaMismatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalError {
    InvalidEndpoint,
    ConnectionFailed,
    Closed,
    InvalidMessage,
}

impl std::fmt::Display for SignalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidEndpoint => "invalid signaling endpoint",
            Self::ConnectionFailed => "signaling connection failed",
            Self::Closed => "signaling connection closed",
            Self::InvalidMessage => "invalid signaling message",
        })
    }
}
impl std::error::Error for SignalError {}

/// A production signaling connection. Insecure `ws://` is intentionally not
/// accepted here; local fixture clients use the raw test transport instead.
pub struct SignalClient {
    outgoing: mpsc::Sender<ClientFrame>,
    incoming: mpsc::Receiver<ServerFrame>,
}

impl SignalClient {
    pub async fn connect(endpoint: &str) -> Result<Self, SignalError> {
        Self::connect_inner(endpoint, None).await
    }

    pub async fn connect_pinned(endpoint: &str, certificate: &[u8]) -> Result<Self, SignalError> {
        Self::connect_inner(endpoint, Some(certificate)).await
    }

    async fn connect_inner(endpoint: &str, certificate: Option<&[u8]>) -> Result<Self, SignalError> {
        let parsed = Url::parse(endpoint).map_err(|_| SignalError::InvalidEndpoint)?;
        if parsed.scheme() != "wss"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || parsed.path() != SIGNAL_PATH
        {
            return Err(SignalError::InvalidEndpoint);
        }
        let websocket_config = WebSocketConfig::default()
            .max_message_size(Some(MAX_SIGNAL_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_SIGNAL_MESSAGE_BYTES));
        let connector = certificate.map(|certificate| {
            let mut roots = rustls::RootCertStore::empty();
            roots.add(rustls::pki_types::CertificateDer::from(certificate.to_vec()))
                .map_err(|_| SignalError::InvalidEndpoint)?;
            Ok::<_, SignalError>(Connector::Rustls(Arc::new(
                rustls::ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()
            )))
        }).transpose()?;
        let request = endpoint.into_client_request().map_err(|_| SignalError::InvalidEndpoint)?;
        let (socket, _) = connect_async_tls_with_config(request, Some(websocket_config), false, connector)
            .await
            .map_err(|_| SignalError::ConnectionFailed)?;
        let (outgoing, mut send_queue) = mpsc::channel::<ClientFrame>(32);
        let (deliver, incoming) = mpsc::channel::<ServerFrame>(32);
        tokio::spawn(async move {
            let mut socket = socket;
            loop {
                tokio::select! {
                    outbound = send_queue.recv() => {
                        let Some(frame) = outbound else { break };
                        let Ok(json) = serde_json::to_string(&frame) else { break };
                        if json.len() > MAX_SIGNAL_MESSAGE_BYTES
                            || socket.send(ClientMessage::Text(json.into())).await.is_err()
                        {
                            break;
                        }
                    }
                    inbound = socket.next() => {
                        match inbound {
                            Some(Ok(ClientMessage::Text(text))) if text.len() <= MAX_SIGNAL_MESSAGE_BYTES => {
                                let Ok(frame) = serde_json::from_str::<ServerFrame>(&text) else { break };
                                if deliver.send(frame).await.is_err() { break; }
                            }
                            Some(Ok(ClientMessage::Ping(bytes))) => {
                                if socket.send(ClientMessage::Pong(bytes)).await.is_err() { break; }
                            }
                            Some(Ok(ClientMessage::Pong(_))) => {}
                            Some(Ok(ClientMessage::Close(_))) | None | Some(Err(_)) => break,
                            _ => break,
                        }
                    }
                }
            }
            let _ = socket.close(None).await;
        });
        Ok(Self { outgoing, incoming })
    }

    pub async fn send(&self, message: &ClientFrame) -> Result<(), SignalError> {
        let json = serde_json::to_string(message).map_err(|_| SignalError::InvalidMessage)?;
        if json.len() > MAX_SIGNAL_MESSAGE_BYTES {
            return Err(SignalError::InvalidMessage);
        }
        self.outgoing
            .send(message.clone())
            .await
            .map_err(|_| SignalError::Closed)
    }

    pub async fn receive(&mut self) -> Result<ServerFrame, SignalError> {
        self.incoming.recv().await.ok_or(SignalError::Closed)
    }

    pub async fn close(self) {
        drop(self);
    }
}

type Outbound = mpsc::Sender<ServerFrame>;

pub struct SignalHub {
    rooms: Mutex<HashMap<RoomId, SignalRoom>>,
    connections: Arc<Semaphore>,
}

struct SignalRoom {
    host: Option<Outbound>,
    verifier: Option<InvitationVerifier>,
    fingerprint: Option<MediaFingerprint>,
    guests: HashMap<ParticipantId, Outbound>,
    pending_guests: std::collections::HashSet<ParticipantId>,
    join_window_start: Option<tokio::time::Instant>,
    join_count: u64,
    last_activity: tokio::time::Instant,
}

impl SignalHub {
    pub fn new() -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            connections: Arc::new(Semaphore::new(MAX_CONNECTIONS)),
        }
    }

    async fn register(
        &self,
        room_id: RoomId,
        verifier: InvitationVerifier,
        fingerprint: MediaFingerprint,
        host: Outbound,
    ) -> Result<(), SignalErrorCode> {
        let mut rooms = self.rooms.lock().await;
        prune_expired_rooms(&mut rooms);
        if let Some(existing) = rooms.get_mut(&room_id) {
            if existing.host.is_some() {
                return Err(SignalErrorCode::DuplicateHost);
            }
            existing.host = Some(host);
            existing.verifier = Some(verifier);
            existing.fingerprint = Some(fingerprint);
            existing.last_activity = tokio::time::Instant::now();
            return Ok(());
        }
        if rooms.len() >= MAX_ROOMS {
            return Err(SignalErrorCode::RoomUnavailable);
        }
        rooms.insert(
            room_id,
            SignalRoom {
                host: Some(host),
                verifier: Some(verifier),
                fingerprint: Some(fingerprint),
                guests: HashMap::new(),
                pending_guests: std::collections::HashSet::new(),
                join_window_start: None,
                join_count: 0,
                last_activity: tokio::time::Instant::now(),
            },
        );
        Ok(())
    }

    async fn add_guest(
        &self,
        room_id: RoomId,
        verifier: InvitationVerifier,
        fingerprint: MediaFingerprint,
        guest: Outbound,
    ) -> Result<ParticipantId, SignalErrorCode> {
        let mut rooms = self.rooms.lock().await;
        prune_expired_rooms(&mut rooms);
        let room = rooms
            .get_mut(&room_id)
            .filter(|room| room.host.is_some())
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        let now = tokio::time::Instant::now();
        if room
            .join_window_start
            .is_none_or(|start| now.duration_since(start) >= Duration::from_secs(60))
        {
            room.join_window_start = Some(now);
            room.join_count = 0;
        }
        if room.join_count >= MAX_ROOM_JOINS_PER_MINUTE {
            return Err(SignalErrorCode::RateLimited);
        }
        room.join_count += 1;
        if !room
            .verifier
            .is_some_and(|expected| expected.matches(verifier))
        {
            return Err(SignalErrorCode::InvalidInvitation);
        }
        if !room
            .fingerprint
            .is_some_and(|expected| expected == fingerprint)
        {
            return Err(SignalErrorCode::MediaMismatch);
        }
        if room.guests.len() >= MAX_GUESTS {
            return Err(SignalErrorCode::RoomFull);
        }
        room.last_activity = now;

        let connection_id = ParticipantId::new();
        room.guests.insert(connection_id, guest);
        room.pending_guests.insert(connection_id);
        let host = room.host.as_ref().expect("checked host is present");
        if host
            .try_send(ServerFrame::GuestJoined { connection_id })
            .is_err()
        {
            room.guests.remove(&connection_id);
            return Err(SignalErrorCode::RoomUnavailable);
        }
        Ok(connection_id)
    }

    async fn keep_alive(&self, room_id: RoomId) -> Result<(), SignalErrorCode> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms
            .get_mut(&room_id)
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        if room.host.is_none() {
            return Err(SignalErrorCode::RoomUnavailable);
        }
        room.last_activity = tokio::time::Instant::now();
        Ok(())
    }

    async fn publish_snapshot(
        &self,
        room_id: RoomId,
        snapshot: RoomSnapshot,
    ) -> Result<(), SignalErrorCode> {
        if snapshot.room_id != room_id {
            return Err(SignalErrorCode::InvalidMessage);
        }
        let mut rooms = self.rooms.lock().await;
        let room = rooms
            .get_mut(&room_id)
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        if room.fingerprint != Some(snapshot.media.fingerprint) {
            return Err(SignalErrorCode::MediaMismatch);
        }
        let lagging: Vec<_> = room
            .guests
            .iter()
            .filter_map(|(id, guest)| {
                guest
                    .try_send(ServerFrame::Snapshot { snapshot })
                    .is_err()
                    .then_some(*id)
            })
            .collect();
        for id in lagging {
            room.guests.remove(&id);
            room.pending_guests.remove(&id);
            if let Some(host) = &room.host {
                let _ = host.try_send(ServerFrame::GuestDisconnected { connection_id: id });
            }
        }
        Ok(())
    }

    async fn command(
        &self,
        room_id: RoomId,
        connection_id: ParticipantId,
        action: PlaybackCommand,
    ) -> Result<(), SignalErrorCode> {
        let rooms = self.rooms.lock().await;
        let room = rooms
            .get(&room_id)
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        if !room.guests.contains_key(&connection_id) || room.pending_guests.contains(&connection_id)
        {
            return Err(SignalErrorCode::RoomUnavailable);
        }
        room.host
            .as_ref()
            .ok_or(SignalErrorCode::RoomUnavailable)?
            .try_send(ServerFrame::GuestCommand {
                connection_id,
                action,
            })
            .map_err(|_| SignalErrorCode::RoomUnavailable)
    }

    async fn approve_guest(
        &self,
        room_id: RoomId,
        connection_id: ParticipantId,
        approved: bool,
    ) -> Result<(), SignalErrorCode> {
        let mut rooms = self.rooms.lock().await;
        let room = rooms
            .get_mut(&room_id)
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        let guest = room
            .guests
            .get(&connection_id)
            .ok_or(SignalErrorCode::RoomUnavailable)?;
        if !room.pending_guests.remove(&connection_id) {
            return Err(SignalErrorCode::InvalidMessage);
        }
        let frame = if approved {
            ServerFrame::JoinAccepted { connection_id }
        } else {
            ServerFrame::Rejected {
                code: SignalErrorCode::RoomUnavailable,
            }
        };
        if guest.try_send(frame).is_err() {
            room.guests.remove(&connection_id);
            return Err(SignalErrorCode::RoomUnavailable);
        }
        Ok(())
    }

    async fn remove_guest(&self, room_id: RoomId, connection_id: ParticipantId) {
        let rooms = self.rooms.lock().await;
        if let Some(room) = rooms.get(&room_id) {
            if room.guests.contains_key(&connection_id)
                && let Some(host) = &room.host
            {
                let _ = host.try_send(ServerFrame::GuestDisconnected { connection_id });
            }
        }
        drop(rooms);
        if let Some(room) = self.rooms.lock().await.get_mut(&room_id) {
            room.guests.remove(&connection_id);
            room.pending_guests.remove(&connection_id);
        }
    }

    async fn remove_host(&self, room_id: RoomId) {
        let mut rooms = self.rooms.lock().await;
        if let Some(room) = rooms.remove(&room_id) {
            for guest in room.guests.into_values() {
                let _ = guest.try_send(ServerFrame::RoomClosed);
            }
        }
    }
}

fn prune_expired_rooms(rooms: &mut HashMap<RoomId, SignalRoom>) {
    let now = tokio::time::Instant::now();
    rooms.retain(|_, room| {
        room.host.is_some() && now.duration_since(room.last_activity) < ROOM_IDLE_TTL
    });
}

/// Create the in-memory WSS signaling broker. Terminate TLS at a trusted
/// reverse proxy and forward only this path to the loopback service listener.
pub fn router(hub: Arc<SignalHub>) -> Router {
    Router::new()
        .route(SIGNAL_PATH, get(upgrade))
        .with_state(hub)
}

async fn upgrade(State(hub): State<Arc<SignalHub>>, upgrade: WebSocketUpgrade) -> Response {
    let Ok(connection_permit) = hub.connections.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "signaling capacity reached",
        )
            .into_response();
    };
    upgrade
        .max_message_size(MAX_SIGNAL_MESSAGE_BYTES)
        .max_frame_size(MAX_SIGNAL_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, hub, connection_permit))
}

async fn handle_socket(mut socket: WebSocket, hub: Arc<SignalHub>, _permit: OwnedSemaphorePermit) {
    let first = tokio::time::timeout(FIRST_MESSAGE_TIMEOUT, socket.recv()).await;
    let Some(Ok(Message::Text(text))) = first.ok().flatten() else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    if text.len() > MAX_SIGNAL_MESSAGE_BYTES {
        let _ = socket.send(Message::Close(None)).await;
        return;
    }
    let Ok(first) = serde_json::from_str::<ClientFrame>(&text) else {
        let _ = socket.send(Message::Close(None)).await;
        return;
    };
    match first {
        ClientFrame::Register {
            room_id,
            verifier,
            fingerprint,
        } => host_socket(socket, hub, room_id, verifier, fingerprint).await,
        ClientFrame::Join {
            room_id,
            verifier,
            fingerprint,
        } => guest_socket(socket, hub, room_id, verifier, fingerprint).await,
        _ => {
            let _ = socket.send(Message::Close(None)).await;
        }
    }
}

async fn host_socket(
    mut socket: WebSocket,
    hub: Arc<SignalHub>,
    room_id: RoomId,
    verifier: InvitationVerifier,
    fingerprint: MediaFingerprint,
) {
    let (outbound, mut messages) = mpsc::channel(8);
    if let Err(code) = hub.register(room_id, verifier, fingerprint, outbound).await {
        let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
        let _ = socket.close().await;
        return;
    }
    if send_server(&mut socket, &ServerFrame::Registered)
        .await
        .is_err()
    {
        hub.remove_host(room_id).await;
        return;
    }

    loop {
        tokio::select! {
            outgoing = messages.recv() => {
                let Some(outgoing) = outgoing else { break };
                if send_server(&mut socket, &outgoing).await.is_err() { break; }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Text(text))) if text.len() <= MAX_SIGNAL_MESSAGE_BYTES => {
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::PlaybackCommand { connection_id, action }) => {
                                let result = hub.command(room_id, connection_id, action).await;
                                if let Err(code) = result {
                                    let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
                                }
                            }
                            Ok(ClientFrame::PublishSnapshot { snapshot }) => {
                                if let Err(code) = hub.publish_snapshot(room_id, snapshot).await {
                                    let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
                                }
                            }
                            Ok(ClientFrame::ApproveGuest { connection_id }) => {
                                if let Err(code) = hub.approve_guest(room_id, connection_id, true).await {
                                    let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
                                }
                            }
                            Ok(ClientFrame::RejectGuest { connection_id }) => {
                                let _ = hub.approve_guest(room_id, connection_id, false).await;
                                hub.remove_guest(room_id, connection_id).await;
                            }
                            Ok(ClientFrame::Cancel { connection_id }) => hub.remove_guest(room_id, connection_id).await,
                            Ok(ClientFrame::KeepAlive) => {
                                if let Err(code) = hub.keep_alive(room_id).await {
                                    let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
                                    break;
                                }
                            }
                            _ => {
                                let _ = send_server(&mut socket, &ServerFrame::Rejected { code: SignalErrorCode::InvalidMessage }).await;
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Ping(bytes))) => if socket.send(Message::Pong(bytes)).await.is_err() { break; },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    _ => {
                        let _ = send_server(&mut socket, &ServerFrame::Rejected { code: SignalErrorCode::InvalidMessage }).await;
                        break;
                    }
                }
            }
        }
    }
    hub.remove_host(room_id).await;
}

async fn guest_socket(
    mut socket: WebSocket,
    hub: Arc<SignalHub>,
    room_id: RoomId,
    verifier: InvitationVerifier,
    fingerprint: MediaFingerprint,
) {
    let (outbound, mut messages) = mpsc::channel(2);
    let connection_id = match hub
        .add_guest(room_id, verifier, fingerprint, outbound)
        .await
    {
        Ok(id) => id,
        Err(code) => {
            let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
            let _ = socket.close().await;
            return;
        }
    };
    loop {
        tokio::select! {
            outgoing = messages.recv() => {
                let Some(outgoing) = outgoing else { break };
                if send_server(&mut socket, &outgoing).await.is_err() { break; }
                if matches!(outgoing, ServerFrame::Rejected { .. }) { break; }
                if matches!(outgoing, ServerFrame::RoomClosed) { break; }
            }
            incoming = socket.recv() => {
                match incoming {
                    Some(Ok(Message::Ping(bytes))) => if socket.send(Message::Pong(bytes)).await.is_err() { break; },
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                    Some(Ok(Message::Text(text))) if text.len() <= MAX_SIGNAL_MESSAGE_BYTES => {
                        match serde_json::from_str::<ClientFrame>(&text) {
                            Ok(ClientFrame::PlaybackCommand { connection_id: id, action }) if id == connection_id => {
                                if let Err(code) = hub.command(room_id, connection_id, action).await {
                                    let _ = send_server(&mut socket, &ServerFrame::Rejected { code }).await;
                                }
                            }
                            Ok(ClientFrame::Cancel { connection_id: id }) if id == connection_id => break,
                            _ => {
                                let _ = send_server(&mut socket, &ServerFrame::Rejected { code: SignalErrorCode::InvalidMessage }).await;
                                break;
                            }
                        }
                    }
                    _ => {
                        let _ = send_server(&mut socket, &ServerFrame::Rejected { code: SignalErrorCode::InvalidMessage }).await;
                        break;
                    }
                }
            }
        }
    }
    hub.remove_guest(room_id, connection_id).await;
}

async fn send_server(socket: &mut WebSocket, message: &ServerFrame) -> Result<(), ()> {
    let text = serde_json::to_string(message).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::invitation::JoinInvitation;
    use base64::Engine;
    use room_server::InvitationToken;
    use shared_protocol::{
        ClockEpochId, ControlMode, HostTime, MediaDescriptor, MediaFingerprint, MediaId, Playback,
        PlaybackCommand, PlaybackStatus, Speed, WireU64,
    };
    use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

    fn make_invitation(room_id: RoomId, token: [u8; 32]) -> JoinInvitation {
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token);
        let capability = InvitationToken::parse(&token).unwrap();
        JoinInvitation::new(room_id, "wss://signal.example/v1/signal", capability).unwrap()
    }

    #[tokio::test]
    async fn signaling_authenticates_guests_and_routes_commands_and_snapshots() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let hub = Arc::new(SignalHub::new());
        let app = router(hub);
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let (mut host, _) = connect_async(format!("ws://{address}{SIGNAL_PATH}"))
            .await
            .unwrap();
        let room_id = RoomId::new();
        let invitation = make_invitation(room_id, [1; 32]);
        let media_fingerprint = MediaFingerprint::from_bytes([9; 32]);
        host.send(WsMessage::Text(
            serde_json::to_string(&ClientFrame::Register {
                room_id,
                verifier: invitation.verifier(),
                fingerprint: media_fingerprint,
            })
            .unwrap()
            .into(),
        ))
        .await
        .unwrap();
        let registered = host.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&registered).unwrap(),
            ServerFrame::Registered
        ));

        let (mut intruder, _) = connect_async(format!("ws://{address}{SIGNAL_PATH}"))
            .await
            .unwrap();
        intruder
            .send(WsMessage::Text(
                serde_json::to_string(&ClientFrame::Join {
                    room_id,
                    verifier: make_invitation(room_id, [2; 32]).verifier(),
                    fingerprint: media_fingerprint,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let rejected = intruder.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&rejected).unwrap(),
            ServerFrame::Rejected {
                code: SignalErrorCode::InvalidInvitation
            }
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), host.next())
                .await
                .is_err()
        );
        intruder.close(None).await.unwrap();

        let (mut wrong_file, _) = connect_async(format!("ws://{address}{SIGNAL_PATH}"))
            .await
            .unwrap();
        wrong_file
            .send(WsMessage::Text(
                serde_json::to_string(&ClientFrame::Join {
                    room_id,
                    verifier: invitation.verifier(),
                    fingerprint: MediaFingerprint::from_bytes([8; 32]),
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let rejected = wrong_file
            .next()
            .await
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&rejected).unwrap(),
            ServerFrame::Rejected {
                code: SignalErrorCode::MediaMismatch
            }
        ));
        wrong_file.close(None).await.unwrap();

        let (mut guest, _) = connect_async(format!("ws://{address}{SIGNAL_PATH}"))
            .await
            .unwrap();
        guest
            .send(WsMessage::Text(
                serde_json::to_string(&ClientFrame::Join {
                    room_id,
                    verifier: invitation.verifier(),
                    fingerprint: media_fingerprint,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let joined = host.next().await.unwrap().unwrap().into_text().unwrap();
        let connection_id = match serde_json::from_str::<ServerFrame>(&joined).unwrap() {
            ServerFrame::GuestJoined { connection_id } => connection_id,
            frame => panic!("unexpected host frame: {frame:?}"),
        };
        host.send(WsMessage::Text(
            serde_json::to_string(&ClientFrame::ApproveGuest { connection_id })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
        let accepted = guest.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&accepted).unwrap(),
            ServerFrame::JoinAccepted { connection_id: id } if id == connection_id
        ));

        let now = HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(1),
        };
        let media_id = MediaId::new();
        let action = PlaybackCommand::Play;
        guest
            .send(WsMessage::Text(
                serde_json::to_string(&ClientFrame::PlaybackCommand {
                    connection_id,
                    action,
                })
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let forwarded = host.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&forwarded).unwrap(),
            ServerFrame::GuestCommand { connection_id: id, action: forwarded } if id == connection_id && forwarded == action
        ));

        let snapshot = RoomSnapshot {
            room_id,
            media: MediaDescriptor {
                media_id,
                size_bytes: WireU64::new(8),
                duration_ms: WireU64::new(2_000),
                fingerprint: MediaFingerprint::from_bytes([9; 32]),
            },
            revision: WireU64::new(1),
            playback: Playback {
                status: PlaybackStatus::Playing,
                position_ms: WireU64::new(0),
                speed: Speed::Normal,
                updated_at: now,
            },
            control: ControlMode::Everyone,
        };
        host.send(WsMessage::Text(
            serde_json::to_string(&ClientFrame::PublishSnapshot { snapshot })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
        let published = guest.next().await.unwrap().unwrap().into_text().unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&published).unwrap(),
            ServerFrame::Snapshot { snapshot: received } if received == snapshot
        ));

        guest.close(None).await.unwrap();
        let disconnected = tokio::time::timeout(Duration::from_secs(2), host.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        assert!(matches!(
            serde_json::from_str::<ServerFrame>(&disconnected).unwrap(),
            ServerFrame::GuestDisconnected { connection_id: id } if id == connection_id
        ));
        host.close(None).await.unwrap();
        server.abort();
    }

    #[tokio::test]
    async fn production_client_rejects_plaintext_and_credential_endpoints() {
        assert_eq!(
            SignalClient::connect("ws://127.0.0.1:8787/v1/signal")
                .await
                .err(),
            Some(SignalError::InvalidEndpoint)
        );
        assert_eq!(
            SignalClient::connect("wss://user:secret@signal.example/v1/signal")
                .await
                .err(),
            Some(SignalError::InvalidEndpoint)
        );
    }

    #[tokio::test]
    async fn hub_bounds_connections_rooms_and_idle_room_lifetime() {
        let hub = SignalHub::new();
        let mut permits = Vec::with_capacity(MAX_CONNECTIONS);
        for _ in 0..MAX_CONNECTIONS {
            permits.push(hub.connections.clone().try_acquire_owned().unwrap());
        }
        assert!(hub.connections.clone().try_acquire_owned().is_err());
        drop(permits);

        let verifier = InvitationVerifier::from_token(
            &InvitationToken::parse(
                &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode([7; 32]),
            )
            .unwrap(),
        );
        let mut receivers = Vec::with_capacity(MAX_ROOMS);
        for _ in 0..MAX_ROOMS {
            let (sender, receiver) = mpsc::channel(1);
            hub.register(
                RoomId::new(),
                verifier,
                MediaFingerprint::from_bytes([7; 32]),
                sender,
            )
            .await
            .unwrap();
            receivers.push(receiver);
        }
        let (sender, _receiver) = mpsc::channel(1);
        assert_eq!(
            hub.register(
                RoomId::new(),
                verifier,
                MediaFingerprint::from_bytes([7; 32]),
                sender
            )
            .await,
            Err(SignalErrorCode::RoomUnavailable)
        );

        {
            let mut rooms = hub.rooms.lock().await;
            let room = rooms.values_mut().next().unwrap();
            room.last_activity =
                tokio::time::Instant::now() - ROOM_IDLE_TTL - Duration::from_secs(1);
            prune_expired_rooms(&mut rooms);
            assert_eq!(rooms.len(), MAX_ROOMS - 1);
        }
        let (sender, _receiver) = mpsc::channel(1);
        hub.register(
            RoomId::new(),
            verifier,
            MediaFingerprint::from_bytes([7; 32]),
            sender,
        )
        .await
        .unwrap();
        assert_eq!(hub.rooms.lock().await.len(), MAX_ROOMS);
        drop(receivers);
    }
}
