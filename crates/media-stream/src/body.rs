use std::sync::Arc;

use axum::body::Body;
use futures_util::stream;
use room_server::MediaPermit;
use tokio_util::sync::CancellationToken;

use crate::{CHUNK_BYTES, ReaderLease, SelectedMedia, StreamError};

struct Reader {
    source: Arc<SelectedMedia>,
    permit: MediaPermit,
    // Retained for this complete response stream, including between pulls.
    _lease: ReaderLease,
    stopped: CancellationToken,
    offset: u64,
    remaining: u64,
}

/// No producer task or queue: at most one bounded chunk per consumer pull.
pub(crate) fn media_body(
    source: Arc<SelectedMedia>,
    permit: MediaPermit,
    lease: ReaderLease,
    stopped: CancellationToken,
    start: u64,
    len: u64,
) -> Body {
    let reader = Reader {
        source,
        permit,
        _lease: lease,
        stopped,
        offset: start,
        remaining: len,
    };
    Body::from_stream(stream::try_unfold(reader, |mut reader| async move {
        if reader.remaining == 0 {
            return Ok(None);
        }
        reader.permit.check_active()?;
        if reader.stopped.is_cancelled() {
            return Err(cancelled());
        }
        let chunk = tokio::select! {
            biased;
            () = reader.stopped.cancelled() => return Err(cancelled()),
            () = reader.permit.cancelled() => {
                reader.permit.check_active()?;
                return Err(cancelled());
            },
            chunk = reader.source.read_chunk(&reader.permit, reader.offset, reader.remaining.min(CHUNK_BYTES as u64) as usize) => chunk?,
        };
        // A read may have finished at the same time as revocation. Recheck at
        // the yield boundary so those newly read bytes cannot be emitted.
        reader.permit.check_active()?;
        if reader.stopped.is_cancelled() {
            return Err(cancelled());
        }
        if chunk.is_empty() {
            return Err(StreamError::MediaChanged);
        }
        reader.offset += chunk.len() as u64;
        reader.remaining -= chunk.len() as u64;
        Ok(Some((chunk, reader)))
    }))
}

fn cancelled() -> StreamError {
    StreamError::Permission(room_server::RoomError::Unauthorized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReaderLimits;
    use futures_util::StreamExt;
    use room_server::{CreatedRoom, RoomHandle};
    use shared_protocol::{ClockEpochId, HostTime, MediaId, WireU64};
    use sync_engine::ManualClock;

    struct Fixture {
        path: std::path::PathBuf,
        source: Arc<SelectedMedia>,
        created: CreatedRoom,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("body-test-{}", MediaId::new()));
            std::fs::write(&path, vec![42; CHUNK_BYTES * 4]).unwrap();
            let source = Arc::new(SelectedMedia::open(&path, MediaId::new()).unwrap());
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
        async fn body(&self, stopped: CancellationToken) -> (Body, MediaPermit) {
            let permit = self
                .created
                .room
                .authorize_media(
                    &self.created.host_session,
                    self.source.descriptor(0).media_id,
                )
                .await
                .unwrap();
            let lease = ReaderLimits::default().try_acquire(&permit).unwrap();
            (
                media_body(
                    self.source.clone(),
                    permit.clone(),
                    lease,
                    stopped,
                    0,
                    self.source.descriptor(0).size_bytes.get(),
                ),
                permit,
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[tokio::test]
    async fn one_bounded_chunk_per_pull_and_lease_spans_pulls() {
        let fixture = Fixture::new();
        let (body, permit) = fixture.body(CancellationToken::new()).await;
        let mut stream = body.into_data_stream();
        let chunk = stream.next().await.unwrap().unwrap();
        assert_eq!(chunk.len(), CHUNK_BYTES);
        assert!(chunk.iter().all(|byte| *byte == 42));
        let second = ReaderLimits::default().try_acquire(&permit).unwrap();
        assert!(matches!(
            ReaderLimits::default().try_acquire(&permit),
            Err(StreamError::ReaderLimit)
        ));
        // With no producer, a later source mutation must affect the next pull;
        // no chunk has already been queued by an independent reader.
        std::fs::write(&fixture.path, b"changed").unwrap();
        assert!(stream.next().await.unwrap().is_err());
        assert!(stream.next().await.is_none());
        let released = ReaderLimits::default().try_acquire(&permit).unwrap();
        drop((second, released));
    }

    #[tokio::test]
    async fn close_and_stop_suppress_next_chunk_and_release_lease() {
        for close in [true, false] {
            let fixture = Fixture::new();
            let stopped = CancellationToken::new();
            let (body, _) = fixture.body(stopped.clone()).await;
            let weak = Arc::downgrade(&fixture.source);
            let mut stream = body.into_data_stream();
            assert_eq!(stream.next().await.unwrap().unwrap().len(), CHUNK_BYTES);
            if close {
                fixture
                    .created
                    .room
                    .close(&fixture.created.host_session)
                    .await
                    .unwrap();
            } else {
                stopped.cancel();
            }
            assert!(stream.next().await.unwrap().is_err());
            assert!(stream.next().await.is_none());
            assert_eq!(weak.strong_count(), 1, "failed stream retained the source");
        }
    }

    #[tokio::test]
    async fn dropping_unpolled_body_releases_lease() {
        let fixture = Fixture::new();
        let (body, permit) = fixture.body(CancellationToken::new()).await;
        let second = ReaderLimits::default().try_acquire(&permit).unwrap();
        assert!(matches!(
            ReaderLimits::default().try_acquire(&permit),
            Err(StreamError::ReaderLimit)
        ));
        drop(body);
        let released = ReaderLimits::default().try_acquire(&permit).unwrap();
        drop((second, released));
    }
}
