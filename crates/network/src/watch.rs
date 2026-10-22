use std::{collections::HashMap, time::Duration};

use room_server::{CreatedRoom, SessionToken};
use shared_protocol::{MediaFingerprint, PlaybackCommand, RoomSnapshot};
use tokio::sync::{mpsc, watch};

use crate::{ClientFrame, JoinInvitation, ServerFrame, SignalClient, SignalError};

const KEEP_ALIVE_EVERY: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchError {
    Signal(SignalError),
    Room,
    Invitation,
    Rejected,
    Closed,
}
impl From<SignalError> for WatchError {
    fn from(value: SignalError) -> Self {
        Self::Signal(value)
    }
}
impl std::fmt::Display for WatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Signal(_) => "signaling unavailable",
            Self::Room => "room operation failed",
            Self::Invitation => "invalid invitation",
            Self::Rejected => "signaling server rejected the request",
            Self::Closed => "watch session closed",
        })
    }
}
impl std::error::Error for WatchError {}

pub struct HostSession {
    pub invitation: JoinInvitation,
    commands: mpsc::Sender<PlaybackCommand>,
    stop: mpsc::Sender<()>,
    pub snapshots: watch::Receiver<RoomSnapshot>,
}

impl HostSession {
    pub async fn start(endpoint: &str, created: CreatedRoom) -> Result<Self, WatchError> {
        Self::start_inner(endpoint, endpoint, None, created).await
    }

    pub async fn start_embedded(
        public_endpoint: &str,
        local_endpoint: &str,
        certificate: &[u8],
        created: CreatedRoom,
    ) -> Result<Self, WatchError> {
        Self::start_inner(public_endpoint, local_endpoint, Some(certificate), created).await
    }

    async fn start_inner(
        public_endpoint: &str,
        connect_endpoint: &str,
        certificate: Option<&[u8]>,
        created: CreatedRoom,
    ) -> Result<Self, WatchError> {
        let room_id = created
            .room
            .snapshot(&created.host_session)
            .await
            .map_err(|_| WatchError::Room)?
            .room_id;
        let invitation = match certificate {
            Some(certificate) => JoinInvitation::new_pinned(room_id, public_endpoint, created.invitation.clone(), certificate.to_vec()),
            None => JoinInvitation::new(room_id, public_endpoint, created.invitation.clone()),
        }.map_err(|_| WatchError::Invitation)?;
        let fingerprint = created
            .room
            .snapshot(&created.host_session)
            .await
            .map_err(|_| WatchError::Room)?
            .media
            .fingerprint;
        let mut client = match certificate {
            Some(certificate) => SignalClient::connect_pinned(connect_endpoint, certificate).await?,
            None => SignalClient::connect(connect_endpoint).await?,
        };
        client
            .send(&ClientFrame::Register {
                room_id,
                verifier: invitation.verifier(),
                fingerprint,
            })
            .await?;
        match client.receive().await? {
            ServerFrame::Registered => {}
            ServerFrame::Rejected { .. } => return Err(WatchError::Rejected),
            _ => return Err(WatchError::Rejected),
        }
        let (commands, mut command_rx) = mpsc::channel(32);
        let (stop, mut stop_rx) = mpsc::channel(1);
        let snapshot_rx = created
            .room
            .subscribe(&created.host_session)
            .await
            .map_err(|_| WatchError::Room)?;
        let (snapshot_tx, snapshots) = watch::channel(*snapshot_rx.borrow());
        let room = created.room.clone();
        let host_session = created.host_session.clone();
        let capability = created.invitation.clone();
        let owner = created.owner;
        tokio::spawn(async move {
            run_host(
                client,
                room,
                host_session,
                capability,
                snapshot_rx,
                snapshot_tx,
                &mut command_rx,
                &mut stop_rx,
            )
            .await;
            owner.close().await;
        });
        Ok(Self {
            invitation,
            commands,
            stop,
            snapshots,
        })
    }

    pub async fn command(&self, action: PlaybackCommand) -> Result<(), WatchError> {
        self.commands
            .send(action)
            .await
            .map_err(|_| WatchError::Closed)
    }

    pub fn is_active(&self) -> bool {
        !self.commands.is_closed()
    }

    pub async fn close(self) {
        let _ = self.stop.send(()).await;
    }
}

async fn run_host(
    mut client: SignalClient,
    room: room_server::RoomHandle,
    host: SessionToken,
    invitation: room_server::InvitationToken,
    mut room_snapshots: watch::Receiver<RoomSnapshot>,
    snapshots: watch::Sender<RoomSnapshot>,
    commands: &mut mpsc::Receiver<PlaybackCommand>,
    stop: &mut mpsc::Receiver<()>,
) {
    let mut guests: HashMap<shared_protocol::ParticipantId, SessionToken> = HashMap::new();
    let mut heartbeat = tokio::time::interval(KEEP_ALIVE_EVERY);
    let initial_snapshot = *room_snapshots.borrow();
    let _ = client
        .send(&ClientFrame::PublishSnapshot {
            snapshot: initial_snapshot,
        })
        .await;
    loop {
        tokio::select! {
            _ = stop.recv() => break,
            _ = heartbeat.tick() => { let _ = client.send(&ClientFrame::KeepAlive).await; }
            action = commands.recv() => {
                let Some(action) = action else { break };
                if let Ok(snapshot) = room.apply_action(&host, action).await {
                    snapshots.send_replace(snapshot);
                    let _ = client.send(&ClientFrame::PublishSnapshot { snapshot }).await;
                }
            }
            frame = client.receive() => match frame {
                Ok(ServerFrame::GuestJoined { connection_id }) => {
                    let fingerprint = room_snapshots.borrow().media.fingerprint;
                    if let Ok(joined) = room.join_with_media(&invitation, "Гость", fingerprint).await {
                        guests.insert(connection_id, joined.session);
                        let _ = client.send(&ClientFrame::ApproveGuest { connection_id }).await;
                        let snapshot = room.snapshot(&host).await;
                        if let Ok(snapshot) = snapshot {
                            snapshots.send_replace(snapshot);
                            let _ = client.send(&ClientFrame::PublishSnapshot { snapshot }).await;
                        }
                    } else {
                        let _ = client.send(&ClientFrame::RejectGuest { connection_id }).await;
                    }
                }
                Ok(ServerFrame::GuestCommand { connection_id, action }) => {
                    if let Some(session) = guests.get(&connection_id)
                        && let Ok(snapshot) = room.apply_action(session, action).await
                    {
                        snapshots.send_replace(snapshot);
                        let _ = client.send(&ClientFrame::PublishSnapshot { snapshot }).await;
                    }
                }
                Ok(ServerFrame::GuestDisconnected { connection_id }) => {
                    if let Some(session) = guests.remove(&connection_id) {
                        let _ = room.leave(&session).await;
                    }
                }
                Ok(ServerFrame::Rejected { .. }) | Err(_) | Ok(ServerFrame::RoomClosed) => break,
                Ok(ServerFrame::Snapshot { .. } | ServerFrame::Registered | ServerFrame::JoinAccepted { .. }) => {}
            },
            changed = room_snapshots.changed() => {
                if changed.is_err() { break; }
                let snapshot = *room_snapshots.borrow_and_update();
                snapshots.send_replace(snapshot);
                let _ = client.send(&ClientFrame::PublishSnapshot { snapshot }).await;
            }
        }
    }
}

pub struct GuestSession {
    pub connection_id: shared_protocol::ParticipantId,
    commands: mpsc::Sender<PlaybackCommand>,
    pub snapshots: watch::Receiver<Option<RoomSnapshot>>,
}
impl GuestSession {
    pub async fn join(
        invitation: JoinInvitation,
        fingerprint: MediaFingerprint,
    ) -> Result<Self, WatchError> {
        let mut client = match invitation.certificate() {
            Some(certificate) => SignalClient::connect_pinned(invitation.signaling_url(), certificate).await?,
            None => SignalClient::connect(invitation.signaling_url()).await?,
        };
        client
            .send(&ClientFrame::Join {
                room_id: invitation.room_id(),
                verifier: invitation.verifier(),
                fingerprint,
            })
            .await?;
        let connection_id = match client.receive().await? {
            ServerFrame::JoinAccepted { connection_id } => connection_id,
            ServerFrame::Rejected { .. } => return Err(WatchError::Rejected),
            _ => return Err(WatchError::Rejected),
        };
        let (commands, mut command_rx) = mpsc::channel(32);
        let (snapshot_tx, snapshots) = watch::channel(None);
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    action = command_rx.recv() => {
                        let Some(action) = action else { break };
                        if client.send(&ClientFrame::PlaybackCommand { connection_id, action }).await.is_err() { break; }
                    }
                    frame = client.receive() => match frame {
                        Ok(ServerFrame::Snapshot { snapshot }) => { snapshot_tx.send_replace(Some(snapshot)); }
                        Ok(ServerFrame::RoomClosed) | Ok(ServerFrame::Rejected { .. }) | Err(_) => break,
                        _ => {}
                    }
                }
            }
            let _ = client.send(&ClientFrame::Cancel { connection_id }).await;
        });
        Ok(Self {
            connection_id,
            commands,
            snapshots,
        })
    }
    pub async fn command(&self, action: PlaybackCommand) -> Result<(), WatchError> {
        self.commands
            .send(action)
            .await
            .map_err(|_| WatchError::Closed)
    }

    pub fn is_active(&self) -> bool {
        !self.commands.is_closed()
    }
}
