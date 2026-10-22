use std::{collections::HashMap, sync::Arc};

use shared_protocol::{
    CommandEnvelope, CommandId, ControlMode, HostTime, MediaDescriptor, MediaFingerprint, MediaId,
    ParticipantId, Playback, PlaybackStatus, RoomId, RoomSnapshot, Speed, WireU64,
};
use sync_engine::Clock;
use tokio::sync::{Mutex, watch};

use crate::{
    RoomError, authority,
    limits::{COMMAND_TTL_MS, DEDUP_CAPACITY, FixedWindow, validate_name},
    sessions::{
        InvitationToken, MediaPermit, Revocation, RoomClock, SessionState, SessionToken,
        TokenDigest, matches,
    },
};

/// Local close-only authority. Minted only by room creation; never serialized.
/// Dropping it has no effect, and it cannot authorize other operations.
pub struct RoomOwner {
    room: RoomHandle,
}
impl RoomOwner {
    pub async fn close(self) {
        // Revoke before awaiting the authority mutex, even after clock/session expiry.
        self.room.clock.closed.cancel();
        let mut state = self.room.state.lock().await;
        state.revoke();
    }
}

pub struct CreatedRoom {
    pub owner: RoomOwner,
    pub room: RoomHandle,
    pub host_id: ParticipantId,
    pub host_session: SessionToken,
    pub invitation: InvitationToken,
}
pub struct JoinedRoom {
    pub participant_id: ParticipantId,
    pub session: SessionToken,
    pub snapshot: RoomSnapshot,
}
#[derive(Clone)]
pub struct RoomHandle {
    state: Arc<Mutex<State>>,
    clock: Arc<RoomClock>,
}
struct CachedCommand {
    command: CommandEnvelope,
    snapshot: RoomSnapshot,
}
struct Session {
    id: ParticipantId,
    digest: TokenDigest,
    active: Arc<SessionState>,
    commands: HashMap<CommandId, CachedCommand>,
    rate: FixedWindow,
    buffering: bool,
    watch: Option<watch::Sender<RoomSnapshot>>,
}
impl Session {
    fn new(id: ParticipantId, digest: TokenDigest, now: u64) -> Self {
        Self {
            id,
            digest,
            active: Arc::new(SessionState::new(now)),
            commands: HashMap::new(),
            rate: FixedWindow::default(),
            buffering: false,
            watch: None,
        }
    }
}
struct State {
    host_id: ParticipantId,
    snapshot: RoomSnapshot,
    sessions: Vec<Session>,
    invitation: Option<TokenDigest>,
    joins: FixedWindow,
    media_cancelled: Arc<Revocation>,
}
impl State {
    fn revoke(&mut self) {
        self.media_cancelled.cancel();
        for actor in &self.sessions {
            actor.active.cancelled.cancel();
        }
        self.sessions.clear();
        self.invitation = None;
    }
    fn authenticate(&mut self, token: &SessionToken, now: HostTime) -> Result<usize, RoomError> {
        self.prune(now);
        let digest = token.digest();
        let index = self
            .sessions
            .iter()
            .position(|s| matches(&s.digest, &digest))
            .ok_or(RoomError::Unauthorized)?;
        self.sessions[index].active.touch(now.ticks_ms.get())?;
        Ok(index)
    }
    fn host(&mut self, token: &SessionToken, now: HostTime) -> Result<usize, RoomError> {
        let index = self.authenticate(token, now)?;
        if self.sessions[index].id != self.host_id {
            return Err(RoomError::Forbidden);
        }
        Ok(index)
    }
    fn prune(&mut self, now: HostTime) {
        self.sessions
            .retain(|session| !session.active.expire(now.ticks_ms.get()));
    }
    fn publish(&self) {
        for session in &self.sessions {
            if let Some(sender) = &session.watch {
                sender.send_replace(self.snapshot);
            }
        }
    }
}
fn initial_playback(now: HostTime) -> Playback {
    Playback {
        status: PlaybackStatus::Paused,
        position_ms: WireU64::new(0),
        speed: Speed::Normal,
        updated_at: now,
    }
}
impl RoomHandle {
    pub fn create(
        media: MediaDescriptor,
        host_name: &str,
        clock: Arc<dyn Clock>,
    ) -> Result<CreatedRoom, RoomError> {
        validate_name(host_name)?;
        let host_session = SessionToken::generate()?;
        let invitation = InvitationToken::generate()?;
        let host_id = ParticipantId::new();
        let (clock, now) = RoomClock::new(clock);
        let snapshot = RoomSnapshot {
            room_id: RoomId::new(),
            media,
            revision: WireU64::new(0),
            playback: initial_playback(now),
            control: ControlMode::Everyone,
        };
        let state = State {
            host_id,
            snapshot,
            sessions: vec![Session::new(
                host_id,
                host_session.digest(),
                now.ticks_ms.get(),
            )],
            invitation: Some(invitation.digest()),
            joins: FixedWindow::default(),
            media_cancelled: Arc::new(Revocation::new()),
        };
        let room = Self {
            state: Arc::new(Mutex::new(state)),
            clock,
        };
        Ok(CreatedRoom {
            owner: RoomOwner { room: room.clone() },
            room,
            host_id,
            host_session,
            invitation,
        })
    }
    pub async fn join(
        &self,
        invitation: &InvitationToken,
        name: &str,
    ) -> Result<JoinedRoom, RoomError> {
        self.join_matching_media(invitation, name, None).await
    }

    /// Join only if the guest's locally selected file has exactly the same
    /// SHA-256 fingerprint as the file already selected by the host.
    pub async fn join_with_media(
        &self,
        invitation: &InvitationToken,
        name: &str,
        fingerprint: MediaFingerprint,
    ) -> Result<JoinedRoom, RoomError> {
        self.join_matching_media(invitation, name, Some(fingerprint))
            .await
    }

    async fn join_matching_media(
        &self,
        invitation: &InvitationToken,
        name: &str,
        fingerprint: Option<MediaFingerprint>,
    ) -> Result<JoinedRoom, RoomError> {
        self.clock.now()?;
        validate_name(name)?;
        // Secure entropy is obtained before entering the authority lock.
        let session = SessionToken::generate()?;
        let participant_id = ParticipantId::new();
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        if !state
            .invitation
            .as_ref()
            .is_some_and(|d| matches(d, &invitation.digest()))
        {
            return Err(RoomError::Unauthorized);
        }
        if fingerprint.is_some_and(|fingerprint| fingerprint != state.snapshot.media.fingerprint) {
            return Err(RoomError::MediaMismatch);
        }
        state.prune(now);
        if state.sessions.len() >= 5 {
            return Err(RoomError::RoomFull);
        }
        state.joins.take(now.ticks_ms.get(), 60_000, 10)?;
        state.sessions.push(Session::new(
            participant_id,
            session.digest(),
            now.ticks_ms.get(),
        ));
        Ok(JoinedRoom {
            participant_id,
            session,
            snapshot: state.snapshot,
        })
    }
    pub async fn snapshot(&self, session: &SessionToken) -> Result<RoomSnapshot, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.authenticate(session, now)?;
        Ok(state.snapshot)
    }
    pub async fn apply(
        &self,
        session: &SessionToken,
        command: CommandEnvelope,
    ) -> Result<RoomSnapshot, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        let index = state.authenticate(session, now)?;
        if command.room_id != state.snapshot.room_id {
            return Err(RoomError::InvalidInput);
        }
        if command.media_id != state.snapshot.media.media_id {
            return Err(RoomError::MediaChanged);
        }
        if state.snapshot.control == ControlMode::HostOnly
            && state.sessions[index].id != state.host_id
        {
            return Err(RoomError::Forbidden);
        }
        let actor = &mut state.sessions[index];
        // Retain until the issued timestamp itself expires. A permitted future
        // timestamp otherwise creates a replay gap after acceptance-time eviction.
        actor.commands.retain(|_, cached| {
            now.ticks_ms
                .get()
                .saturating_sub(cached.command.issued_at.ticks_ms.get())
                <= COMMAND_TTL_MS
        });
        if let Some(cached) = actor.commands.get(&command.command_id) {
            if cached.command != command {
                return Err(RoomError::CommandConflict);
            }
            authority::validate_time(command, now)?;
            return Ok(cached.snapshot);
        }
        authority::validate_time(command, now)?;
        if actor.commands.len() >= DEDUP_CAPACITY {
            return Err(RoomError::RateLimited);
        }
        actor.rate.take(now.ticks_ms.get(), 1000, 20)?;
        let snapshot = authority::apply(state.snapshot, command.action, now)?;
        state.sessions[index]
            .commands
            .insert(command.command_id, CachedCommand { command, snapshot });
        state.snapshot = snapshot;
        state.publish();
        Ok(snapshot)
    }

    /// Apply an action using this room's own monotonic clock. This is the
    /// entry point for local host controls and broker-relayed guest intents;
    /// remote clients never get to choose an authoritative timestamp.
    pub async fn apply_action(
        &self,
        session: &SessionToken,
        action: shared_protocol::PlaybackCommand,
    ) -> Result<RoomSnapshot, RoomError> {
        let now = self.clock.now()?;
        let snapshot = self.snapshot(session).await?;
        self.apply(
            session,
            CommandEnvelope {
                room_id: snapshot.room_id,
                media_id: snapshot.media.media_id,
                command_id: shared_protocol::CommandId::new(),
                issued_at: now,
                action,
            },
        )
        .await
    }
    pub async fn set_control(
        &self,
        session: &SessionToken,
        mode: ControlMode,
    ) -> Result<RoomSnapshot, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.host(session, now)?;
        if state.snapshot.control != mode {
            state.snapshot.revision = authority::next_revision(state.snapshot)?;
            state.snapshot.control = mode;
            state.publish();
        }
        Ok(state.snapshot)
    }
    pub async fn rotate_invitation(
        &self,
        session: &SessionToken,
    ) -> Result<InvitationToken, RoomError> {
        self.clock.now()?;
        let invitation = InvitationToken::generate()?;
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.host(session, now)?;
        state.invitation = Some(invitation.digest());
        Ok(invitation)
    }
    pub async fn kick(
        &self,
        session: &SessionToken,
        target: ParticipantId,
    ) -> Result<(), RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.host(session, now)?;
        if target == state.host_id {
            return Err(RoomError::Forbidden);
        }
        let index = state
            .sessions
            .iter()
            .position(|s| s.id == target)
            .ok_or(RoomError::InvalidInput)?;
        let removed = state.sessions.remove(index);
        removed.active.cancelled.cancel();
        Ok(())
    }
    /// End a guest session from its owning client connection. The host session
    /// can only remove guests through `kick` or by closing the room.
    pub async fn leave(&self, session: &SessionToken) -> Result<(), RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        let index = state.authenticate(session, now)?;
        if state.sessions[index].id == state.host_id {
            return Err(RoomError::Forbidden);
        }
        let removed = state.sessions.remove(index);
        removed.active.cancelled.cancel();
        Ok(())
    }
    pub async fn replace_media(
        &self,
        session: &SessionToken,
        media: MediaDescriptor,
    ) -> Result<RoomSnapshot, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.host(session, now)?;
        if state.snapshot.media.media_id == media.media_id {
            return Err(RoomError::InvalidInput);
        }
        let revision = authority::next_revision(state.snapshot)?;
        state.media_cancelled.cancel();
        state.media_cancelled = Arc::new(Revocation::new());
        state.snapshot.media = media;
        state.snapshot.revision = revision;
        state.snapshot.playback = initial_playback(now);
        state.publish();
        Ok(state.snapshot)
    }
    pub async fn close(&self, session: &SessionToken) -> Result<(), RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        state.host(session, now)?;
        self.clock.closed.cancel();
        state.revoke();
        Ok(())
    }
    pub async fn authorize_media(
        &self,
        session: &SessionToken,
        media_id: MediaId,
    ) -> Result<MediaPermit, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        let index = state.authenticate(session, now)?;
        if media_id != state.snapshot.media.media_id {
            return Err(RoomError::MediaChanged);
        }
        Ok(MediaPermit {
            room_id: state.snapshot.room_id,
            participant_id: state.sessions[index].id,
            media_id,
            clock: self.clock.clone(),
            session: state.sessions[index].active.clone(),
            media_cancelled: state.media_cancelled.clone(),
        })
    }
    pub async fn set_buffering(
        &self,
        session: &SessionToken,
        buffering: bool,
    ) -> Result<(), RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        let index = state.authenticate(session, now)?;
        state.sessions[index].buffering = buffering;
        Ok(())
    }
    pub async fn subscribe(
        &self,
        session: &SessionToken,
    ) -> Result<watch::Receiver<RoomSnapshot>, RoomError> {
        let mut state = self.state.lock().await;
        let now = self.clock.now()?;
        let index = state.authenticate(session, now)?;
        let count: usize = state
            .sessions
            .iter()
            .filter_map(|s| s.watch.as_ref())
            .map(watch::Sender::receiver_count)
            .sum();
        if count >= 8 {
            return Err(RoomError::RateLimited);
        }
        let snapshot = state.snapshot;
        let sender = state.sessions[index]
            .watch
            .get_or_insert_with(|| watch::channel(snapshot).0);
        Ok(sender.subscribe())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shared_protocol::{ClockEpochId, PlaybackCommand};
    use sync_engine::ManualClock;

    #[tokio::test]
    async fn revision_exhaustion_is_transactional_for_all_mutations() {
        let clock = Arc::new(ManualClock::new(HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(0),
        }));
        let media = MediaDescriptor {
            media_id: MediaId::new(),
            size_bytes: WireU64::new(1),
            duration_ms: WireU64::new(1),
            fingerprint: MediaFingerprint::from_bytes([3; 32]),
        };
        let c = RoomHandle::create(media, "host", clock).unwrap();
        c.room.state.lock().await.snapshot.revision = WireU64::new(u64::MAX);
        let before = c.room.snapshot(&c.host_session).await.unwrap();
        let permit = c
            .room
            .authorize_media(&c.host_session, media.media_id)
            .await
            .unwrap();
        let command = CommandEnvelope {
            room_id: before.room_id,
            media_id: media.media_id,
            command_id: CommandId::new(),
            issued_at: before.playback.updated_at,
            action: PlaybackCommand::Play,
        };
        assert_eq!(
            c.room.apply(&c.host_session, command).await,
            Err(RoomError::CounterExhausted)
        );
        assert_eq!(
            c.room
                .set_control(&c.host_session, ControlMode::HostOnly)
                .await,
            Err(RoomError::CounterExhausted)
        );
        assert_eq!(
            c.room
                .replace_media(
                    &c.host_session,
                    MediaDescriptor {
                        media_id: MediaId::new(),
                        ..media
                    }
                )
                .await,
            Err(RoomError::CounterExhausted)
        );
        assert_eq!(c.room.snapshot(&c.host_session).await.unwrap(), before);
        permit.check_active().unwrap();
        assert!(c.room.state.lock().await.sessions[0].commands.is_empty());
    }
    #[tokio::test]
    async fn active_permit_checks_do_not_acquire_the_room_mutex() {
        let clock = Arc::new(ManualClock::new(HostTime {
            epoch: ClockEpochId::new(),
            ticks_ms: WireU64::new(0),
        }));
        let media = MediaDescriptor {
            media_id: MediaId::new(),
            size_bytes: WireU64::new(1),
            duration_ms: WireU64::new(1),
            fingerprint: MediaFingerprint::from_bytes([4; 32]),
        };
        let c = RoomHandle::create(media, "host", clock).unwrap();
        let permit = c
            .room
            .authorize_media(&c.host_session, media.media_id)
            .await
            .unwrap();
        let _authority_lock = c.room.state.lock().await;
        permit.check_active().unwrap();
    }
}
