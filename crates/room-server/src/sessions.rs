use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use arc_swap::ArcSwapOption;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use shared_protocol::{ClockEpochId, HostTime, MediaId, ParticipantId, RoomId, WireU64};
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
            pub fn parse(value: &str) -> Result<Self, RoomError> {
                let bytes = URL_SAFE_NO_PAD
                    .decode(value)
                    .map_err(|_| RoomError::InvalidInput)?;
                if bytes.len() != 32 || URL_SAFE_NO_PAD.encode(&bytes) != value {
                    return Err(RoomError::InvalidInput);
                }
                Ok(Self(value.to_owned()))
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
        // Only a high-water mark observed before sampling can establish a
        // genuine regression. Another caller may publish a newer tick while
        // this sample is in flight; completion order is not clock order.
        let before = self.last.load(Ordering::SeqCst);
        let now = self.clock.now();
        if now.epoch != self.epoch || now.ticks_ms.get() < before {
            return Err(RoomError::ClockMismatch);
        }
        let published = self.last.fetch_max(now.ticks_ms.get(), Ordering::SeqCst);
        Ok(HostTime {
            ticks_ms: WireU64::new(now.ticks_ms.get().max(published)),
            ..now
        })
    }
}

#[cfg(test)]
#[derive(Default)]
struct TransitionHook(std::sync::Mutex<Option<Box<dyn FnOnce() + Send>>>);
#[cfg(test)]
impl TransitionHook {
    fn run(&self) {
        let hook = self.0.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }
}

pub(crate) struct SessionState {
    pub(crate) cancelled: Revocation,
    // None is terminal expiry; every Some record is immutable and has its own
    // identity, so a stale expiry CAS cannot revoke a successful renewal.
    last_activity: ArcSwapOption<u64>,
    #[cfg(test)]
    before_expiry_commit: TransitionHook,
    #[cfg(test)]
    before_renewal_commit: TransitionHook,
}
impl SessionState {
    pub(crate) fn new(now: u64) -> Self {
        Self {
            cancelled: Revocation::new(),
            last_activity: ArcSwapOption::from_pointee(now),
            #[cfg(test)]
            before_expiry_commit: TransitionHook::default(),
            #[cfg(test)]
            before_renewal_commit: TransitionHook::default(),
        }
    }
    fn replace_activity(&self, observed: &Arc<u64>, next: Option<Arc<u64>>) -> bool {
        self.last_activity
            .compare_and_swap(observed, next)
            .as_ref()
            .is_some_and(|previous| Arc::ptr_eq(previous, observed))
    }
    pub(crate) fn touch(&self, now: u64) -> Result<(), RoomError> {
        loop {
            if self.cancelled.is_cancelled() {
                return Err(RoomError::Unauthorized);
            }
            let Some(last) = self.last_activity.load_full() else {
                self.cancelled.cancel();
                return Err(RoomError::Unauthorized);
            };
            let expired = now.saturating_sub(*last) > SESSION_TTL_MS;
            #[cfg(test)]
            if expired {
                self.before_expiry_commit.run();
            } else {
                self.before_renewal_commit.run();
            }
            let next = (!expired).then(|| Arc::new(now.max(*last)));
            // Holding the observed Arc prevents address reuse while this CAS
            // competes with a renewal or terminal expiry transition.
            if !self.replace_activity(&last, next) {
                continue;
            }
            if expired {
                self.cancelled.cancel();
                return Err(RoomError::Unauthorized);
            }
            if self.cancelled.is_cancelled() {
                return Err(RoomError::Unauthorized);
            }
            return Ok(());
        }
    }
    pub(crate) fn expire(&self, now: u64) -> bool {
        loop {
            if self.cancelled.is_cancelled() {
                return true;
            }
            let Some(last) = self.last_activity.load_full() else {
                self.cancelled.cancel();
                return true;
            };
            if now.saturating_sub(*last) <= SESSION_TTL_MS {
                return false;
            }
            #[cfg(test)]
            self.before_expiry_commit.run();
            if self.replace_activity(&last, None) {
                self.cancelled.cancel();
                return true;
            }
            // A concurrent successful read changed the activity generation;
            // decide again instead of cancelling from the stale timestamp.
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Barrier, thread};

    fn pause(hook: &TransitionHook) -> (Arc<Barrier>, Arc<Barrier>) {
        let entered = Arc::new(Barrier::new(2));
        let resume = Arc::new(Barrier::new(2));
        let (entered_hook, resume_hook) = (entered.clone(), resume.clone());
        *hook.0.lock().unwrap() = Some(Box::new(move || {
            entered_hook.wait();
            resume_hook.wait();
        }));
        (entered, resume)
    }

    fn renew_while_expiry_is_pending(expired_touch: bool) {
        let session = SessionState::new(0);
        let (entered, resume) = pause(&session.before_expiry_commit);
        thread::scope(|scope| {
            let attempt = scope.spawn(|| {
                if expired_touch {
                    session.touch(300_001).map(|()| false)
                } else {
                    Ok(session.expire(300_001))
                }
            });
            entered.wait();
            let renewal = session.touch(300_000);
            resume.wait();
            let expiry = attempt.join().unwrap();
            assert_eq!(renewal, Ok(()));
            assert_eq!(expiry, Ok(false));
        });
        assert!(!session.cancelled.is_cancelled());
        assert_eq!(session.touch(300_001), Ok(()));
    }

    #[test]
    fn renewal_wins_against_stale_prune_expiry() {
        renew_while_expiry_is_pending(false);
    }

    #[test]
    fn renewal_wins_against_stale_touch_expiry() {
        renew_while_expiry_is_pending(true);
    }

    #[test]
    fn committed_expiry_prevents_pending_renewal_from_succeeding() {
        let session = SessionState::new(0);
        let (entered, resume) = pause(&session.before_renewal_commit);
        thread::scope(|scope| {
            let renewal = scope.spawn(|| session.touch(300_000));
            entered.wait();
            let expired = session.expire(300_001);
            resume.wait();
            assert!(expired);
            assert_eq!(renewal.join().unwrap(), Err(RoomError::Unauthorized));
        });
        assert!(session.cancelled.is_cancelled());
    }
}
