use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use shared_protocol::{ClockEpochId, HostTime, MediaId, ParticipantId, RoomId};
use subtle::ConstantTimeEq;
use sync_engine::Clock;
use tokio_util::sync::CancellationToken;

use crate::{RoomError, limits::SESSION_TTL_MS};

pub(crate) type TokenDigest = [u8; 32];
fn digest(secret: &str) -> TokenDigest {
    Sha256::digest(secret.as_bytes()).into()
}
pub(crate) fn matches(left: &TokenDigest, right: &TokenDigest) -> bool {
    bool::from(left.ct_eq(right))
}

macro_rules! token {
    ($name:ident) => {
        /// A 32-byte random capability encoded as unpadded base64url.
        /// Deliberately does not implement serialization or secret-bearing formatting.
        #[derive(Clone)]
        pub struct $name(String);
        impl $name {
            pub(crate) fn generate() -> Result<Self, RoomError> {
                let mut bytes = [0_u8; 32];
                getrandom::fill(&mut bytes).map_err(|_| RoomError::EntropyUnavailable)?;
                Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
            }
            pub fn expose(&self) -> &str {
                &self.0
            }
            pub(crate) fn digest(&self) -> TokenDigest {
                digest(self.expose())
            }
        }
        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(concat!(stringify!($name), "([REDACTED])"))
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("[REDACTED]")
            }
        }
    };
}
token!(SessionToken);
token!(InvitationToken);

/// Atomic revocation reads keep active media checks off notification locks.
pub(crate) struct Revocation {
    revoked: AtomicBool,
    notification: CancellationToken,
}
impl Revocation {
    pub(crate) fn new() -> Self {
        Self {
            revoked: AtomicBool::new(false),
            notification: CancellationToken::new(),
        }
    }
    pub(crate) fn is_cancelled(&self) -> bool {
        self.revoked.load(Ordering::SeqCst)
    }
    pub(crate) fn cancel(&self) {
        if !self.revoked.swap(true, Ordering::SeqCst) {
            self.notification.cancel();
        }
    }
    pub(crate) async fn cancelled(&self) {
        if !self.is_cancelled() {
            self.notification.cancelled().await;
        }
    }
}

pub(crate) struct RoomClock {
    clock: Arc<dyn Clock>,
    epoch: ClockEpochId,
    last: AtomicU64,
    pub(crate) closed: Revocation,
}
impl RoomClock {
    pub(crate) fn new(clock: Arc<dyn Clock>) -> (Arc<Self>, HostTime) {
        let now = clock.now();
        (
            Arc::new(Self {
                clock,
                epoch: now.epoch,
                last: AtomicU64::new(now.ticks_ms.get()),
                closed: Revocation::new(),
            }),
            now,
        )
    }
    pub(crate) fn now(&self) -> Result<HostTime, RoomError> {
        if self.closed.is_cancelled() {
            return Err(RoomError::RoomClosed);
        }
        let now = self.clock.now();
        if now.epoch != self.epoch {
            return Err(RoomError::ClockMismatch);
        }
        self.last
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
                (now.ticks_ms.get() >= last).then_some(now.ticks_ms.get())
            })
            .map_err(|_| RoomError::ClockMismatch)?;
        Ok(now)
    }
}

pub(crate) struct SessionState {
    pub(crate) cancelled: Revocation,
    last_activity: AtomicU64,
}
impl SessionState {
    pub(crate) fn new(now: u64) -> Self {
        Self {
            cancelled: Revocation::new(),
            last_activity: AtomicU64::new(now),
        }
    }
    pub(crate) fn touch(&self, now: u64) -> Result<(), RoomError> {
        if self.cancelled.is_cancelled() {
            return Err(RoomError::Unauthorized);
        }
        self.last_activity
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |last| {
                (now.saturating_sub(last) <= SESSION_TTL_MS).then_some(now.max(last))
            })
            .map_err(|_| {
                self.cancelled.cancel();
                RoomError::Unauthorized
            })?;
        if self.cancelled.is_cancelled() {
            return Err(RoomError::Unauthorized);
        }
        Ok(())
    }
    pub(crate) fn expire(&self, now: u64) -> bool {
        let expired =
            now.saturating_sub(self.last_activity.load(Ordering::SeqCst)) > SESSION_TTL_MS;
        if expired {
            self.cancelled.cancel();
        }
        self.cancelled.is_cancelled()
    }
}

/// Revocable permission tied to one participant and one media generation.
/// `check_active` is lock-free and refreshes a live session before every read.
#[derive(Clone)]
pub struct MediaPermit {
    pub(crate) room_id: RoomId,
    pub(crate) participant_id: ParticipantId,
    pub(crate) media_id: MediaId,
    pub(crate) clock: Arc<RoomClock>,
    pub(crate) session: Arc<SessionState>,
    pub(crate) media_cancelled: Arc<Revocation>,
}
impl MediaPermit {
    pub fn room_id(&self) -> RoomId {
        self.room_id
    }
    pub fn participant_id(&self) -> ParticipantId {
        self.participant_id
    }
    pub fn media_id(&self) -> MediaId {
        self.media_id
    }
    pub fn check_active(&self) -> Result<(), RoomError> {
        let now = self.clock.now()?;
        if self.media_cancelled.is_cancelled() {
            return Err(RoomError::MediaChanged);
        }
        self.session.touch(now.ticks_ms.get())?;
        if self.clock.closed.is_cancelled() {
            return Err(RoomError::RoomClosed);
        }
        if self.media_cancelled.is_cancelled() {
            return Err(RoomError::MediaChanged);
        }
        Ok(())
    }
    /// Resolves when closure, kick, observed expiry, or media replacement revokes access.
    /// Clock expiry is observed by activity checks; no wall clock timer is used.
    pub async fn cancelled(&self) {
        if self.check_active().is_err() {
            return;
        }
        tokio::select! {
            () = self.clock.closed.cancelled() => {},
            () = self.session.cancelled.cancelled() => {},
            () = self.media_cancelled.cancelled() => {},
        }
    }
}
