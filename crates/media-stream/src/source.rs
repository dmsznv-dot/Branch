use std::{
    collections::HashMap,
    fs::Metadata,
    path::Path,
    sync::{
        Arc, Mutex as StdMutex, OnceLock,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};

use bytes::Bytes;
use room_server::MediaPermit;
use shared_protocol::{MediaDescriptor, MediaId, ParticipantId, RoomId, WireU64};
use tokio::{
    fs::File,
    io::{AsyncReadExt, AsyncSeekExt, SeekFrom},
    sync::Mutex,
};

use crate::{CHUNK_BYTES, MAX_READERS_PER_ROOM, MAX_READERS_PER_SESSION, StreamError};

/// One trusted selection, opened once. No pathname is retained or reopened.
/// All cursor operations share one mutex, including concurrent range readers.
pub struct SelectedMedia {
    #[cfg(feature = "test-support")]
    pub(crate) probe: Option<Arc<crate::BodyProbe>>,
    file: Mutex<File>,
    media_id: MediaId,
    size: u64,
    modified: SystemTime,
    changed: AtomicBool,
}

impl SelectedMedia {
    /// This synchronous call belongs at the trusted Rust file-selection boundary.
    /// The caller must assign a fresh media ID to each new selection.
    pub fn open(path: &Path, media_id: MediaId) -> Result<Self, StreamError> {
        let file = std::fs::File::open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(StreamError::NotRegularFile);
        }
        Ok(Self {
            #[cfg(feature = "test-support")]
            probe: None,
            file: Mutex::new(File::from_std(file)),
            media_id,
            size: metadata.len(),
            modified: metadata.modified()?,
            changed: AtomicBool::new(false),
        })
    }

    #[cfg(feature = "test-support")]
    pub fn with_probe(mut self, probe: Arc<crate::BodyProbe>) -> Self {
        self.probe = Some(probe);
        self
    }

    pub fn descriptor(&self, duration_ms: u64) -> MediaDescriptor {
        MediaDescriptor {
            media_id: self.media_id,
            size_bytes: WireU64::new(self.size),
            duration_ms: WireU64::new(duration_ms),
        }
    }

    fn check(&self, permit: &MediaPermit) -> Result<(), StreamError> {
        permit.check_active()?;
        if permit.media_id() != self.media_id || self.changed.load(Ordering::SeqCst) {
            return Err(StreamError::MediaChanged);
        }
        Ok(())
    }

    fn check_metadata(&self, metadata: Metadata) -> Result<(), StreamError> {
        if metadata.len() != self.size || metadata.modified()? != self.modified {
            return self.mark_changed();
        }
        Ok(())
    }

    fn mark_changed<T>(&self) -> Result<T, StreamError> {
        self.changed.store(true, Ordering::SeqCst);
        Err(StreamError::MediaChanged)
    }

    /// Reads at most min(max_len, CHUNK_BYTES, remaining). EOF returns empty;
    /// offsets beyond EOF fail. A transport must hold a reader lease separately
    /// for the entire stream, rather than acquiring one lease for each chunk.
    pub async fn read_chunk(
        &self,
        permit: &MediaPermit,
        offset: u64,
        max_len: usize,
    ) -> Result<Bytes, StreamError> {
        self.check(permit)?;
        let remaining = self
            .size
            .checked_sub(offset)
            .ok_or(StreamError::InvalidOffset)?;
        let len = remaining.min(max_len.min(CHUNK_BYTES) as u64) as usize;
        let mut file = tokio::select! {
            biased;
            () = permit.cancelled() => {
                permit.check_active()?;
                return Err(StreamError::Permission(room_server::RoomError::Unauthorized));
            },
            file = self.file.lock() => file,
        };
        self.check(permit)?;
        let metadata = file.metadata().await;
        self.check(permit)?;
        self.check_metadata(metadata?)?;
        if len == 0 {
            return Ok(Bytes::new());
        }

        self.check(permit)?;
        let seek = file.seek(SeekFrom::Start(offset)).await;
        self.check(permit)?;
        seek?;
        #[cfg(feature = "test-support")]
        let mut tracked = self.probe.as_ref().map(|probe| probe.chunk(len));
        #[cfg(feature = "test-support")]
        let mut fallback = if tracked.is_none() {
            vec![0; len]
        } else {
            Vec::new()
        };
        #[cfg(feature = "test-support")]
        let chunk = tracked
            .as_mut()
            .map_or(fallback.as_mut_slice(), |chunk| chunk.data.as_mut_slice());
        #[cfg(not(feature = "test-support"))]
        let mut chunk = vec![0; len];
        let mut filled = 0;
        while filled < len {
            self.check(permit)?;
            let read = file.read(&mut chunk[filled..]).await;
            self.check(permit)?;
            let count = read?;
            if count == 0 {
                return self.mark_changed();
            }
            filled += count;
        }
        self.check(permit)?;
        let metadata = file.metadata().await;
        self.check(permit)?;
        self.check_metadata(metadata?)?;
        #[cfg(feature = "test-support")]
        return Ok(match tracked {
            Some(chunk) => Bytes::from_owner(chunk),
            None => Bytes::from(fallback),
        });
        #[cfg(not(feature = "test-support"))]
        Ok(Bytes::from(chunk))
    }
}

#[derive(Default)]
struct RoomReaders {
    total: usize,
    sessions: HashMap<ParticipantId, usize>,
}
#[derive(Default)]
struct Counts {
    rooms: HashMap<RoomId, RoomReaders>,
}

/// Process-wide accounting shared by independent adapters and media generations.
/// Only active leases retain room/session entries; no token material is stored.
#[derive(Clone)]
pub struct ReaderLimits {
    counts: Arc<StdMutex<Counts>>,
}
impl Default for ReaderLimits {
    fn default() -> Self {
        static SHARED: OnceLock<Arc<StdMutex<Counts>>> = OnceLock::new();
        Self {
            counts: SHARED
                .get_or_init(|| Arc::new(StdMutex::new(Counts::default())))
                .clone(),
        }
    }
}

/// Hold until a response body finishes, errors, or is dropped. Not cloneable.
#[must_use = "retain the reader lease until the response body ends"]
pub struct ReaderLease {
    limits: ReaderLimits,
    room_id: RoomId,
    participant_id: ParticipantId,
}
impl ReaderLimits {
    /// Fail immediately at either quota. Concurrent acquisitions are atomic.
    pub fn try_acquire(&self, permit: &MediaPermit) -> Result<ReaderLease, StreamError> {
        permit.check_active()?;
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        // A permit can be revoked while this thread waits for accounting.
        permit.check_active()?;
        let room = counts.rooms.entry(permit.room_id()).or_default();
        let session = room
            .sessions
            .get(&permit.participant_id())
            .copied()
            .unwrap_or(0);
        if room.total >= MAX_READERS_PER_ROOM || session >= MAX_READERS_PER_SESSION {
            return Err(StreamError::ReaderLimit);
        }
        room.sessions.insert(permit.participant_id(), session + 1);
        room.total += 1;
        Ok(ReaderLease {
            limits: self.clone(),
            room_id: permit.room_id(),
            participant_id: permit.participant_id(),
        })
    }
}

impl Drop for ReaderLease {
    fn drop(&mut self) {
        let mut counts = self
            .limits
            .counts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let room = counts
            .rooms
            .get_mut(&self.room_id)
            .expect("live reader room");
        let session = room
            .sessions
            .get_mut(&self.participant_id)
            .expect("live reader session");
        *session -= 1;
        if *session == 0 {
            room.sessions.remove(&self.participant_id);
        }
        room.total -= 1;
        if room.total == 0 {
            counts.rooms.remove(&self.room_id);
        }
        // Reclaim the historical peak capacity after active rooms disappear.
        if counts.rooms.capacity() > counts.rooms.len().saturating_mul(4).max(16) {
            counts.rooms.shrink_to_fit();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use room_server::{CreatedRoom, RoomError, RoomHandle};
    use shared_protocol::{ClockEpochId, HostTime};
    use std::{future::Future, path::PathBuf, task::Poll};
    use sync_engine::ManualClock;

    struct Fixture {
        path: PathBuf,
        source: SelectedMedia,
        created: CreatedRoom,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("media-stream-unit-{}", MediaId::new()));
            std::fs::write(&path, b"original").unwrap();
            let source = SelectedMedia::open(&path, MediaId::new()).unwrap();
            let clock = Arc::new(ManualClock::new(HostTime {
                epoch: ClockEpochId::new(),
                ticks_ms: WireU64::new(0),
            }));
            let created = RoomHandle::create(source.descriptor(0), "host", clock).unwrap();
            Self {
                path,
                source,
                created,
            }
        }
        async fn permit(&self) -> MediaPermit {
            self.created
                .room
                .authorize_media(&self.created.host_session, self.source.media_id)
                .await
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn waiting_reader_is_cancelled_before_cursor_lock_is_available() {
        let fixture = Fixture::new();
        let permit = fixture.permit().await;
        let guard = fixture.source.file.lock().await;
        let read = fixture.source.read_chunk(&permit, 0, 8);
        tokio::pin!(read);
        assert!(std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx).is_pending())).await);
        fixture
            .created
            .room
            .close(&fixture.created.host_session)
            .await
            .unwrap();
        // Keep the cursor mutex locked: cancellation must wake independently.
        let result = std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx))).await;
        assert!(matches!(
            result,
            Poll::Ready(Err(StreamError::Permission(RoomError::RoomClosed)))
        ));
        drop(guard);
    }

    #[test]
    fn revocation_during_awaited_io_never_emits_bytes() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let fixture = Fixture::new();
            let permit = fixture.permit().await;
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel();
            // The only file-I/O worker is occupied so metadata cannot finish
            // until after the room is closed. No sleeps or timing assumptions.
            let blocker = tokio::task::spawn_blocking(move || {
                entered_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
            entered_rx.await.unwrap();
            let read = fixture.source.read_chunk(&permit, 0, 8);
            tokio::pin!(read);
            let pending =
                std::future::poll_fn(|cx| Poll::Ready(read.as_mut().poll(cx).is_pending())).await;
            fixture
                .created
                .room
                .close(&fixture.created.host_session)
                .await
                .unwrap();
            release_tx.send(()).unwrap();
            assert!(pending, "read did not await file I/O");
            assert!(matches!(
                read.await,
                Err(StreamError::Permission(RoomError::RoomClosed))
            ));
            blocker.await.unwrap();
        });
    }

    #[tokio::test]
    async fn concurrent_acquisitions_are_atomic_and_remove_idle_entries() {
        let fixture = Fixture::new();
        let permit = fixture.permit().await;
        let limits = ReaderLimits::default();
        let barrier = std::sync::Barrier::new(16);
        let results = std::thread::scope(|scope| {
            let workers: Vec<_> = (0..16)
                .map(|_| {
                    let barrier = &barrier;
                    let permit = &permit;
                    scope.spawn(move || {
                        barrier.wait();
                        ReaderLimits::default().try_acquire(permit)
                    })
                })
                .collect();
            workers
                .into_iter()
                .map(|worker| worker.join().unwrap())
                .collect::<Vec<_>>()
        });
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 2);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(StreamError::ReaderLimit)))
                .count(),
            14
        );
        drop(results);
        assert!(
            !limits
                .counts
                .lock()
                .unwrap()
                .rooms
                .contains_key(&permit.room_id())
        );
        // Historical sessions/rooms must not accumulate accounting records.
        for _ in 0..32 {
            let fixture = Fixture::new();
            let permit = fixture.permit().await;
            drop(limits.try_acquire(&permit).unwrap());
            assert!(
                !limits
                    .counts
                    .lock()
                    .unwrap()
                    .rooms
                    .contains_key(&permit.room_id())
            );
        }
    }
}
