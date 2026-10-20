//! Deterministic test instrumentation; absent from default production builds.
use crate::{ReaderLimits, SelectedMedia, StreamError};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::Notify;

#[derive(Default)]
pub struct BodyProbe {
    pause: bool,
    boundary: Notify,
    idle: Notify,
    reads: AtomicUsize,
    yields: AtomicUsize,
    active: AtomicUsize,
    live: AtomicUsize,
    peak: AtomicUsize,
}
impl BodyProbe {
    pub fn paused_after_first_chunk() -> Self {
        Self {
            pause: true,
            ..Self::default()
        }
    }
    pub fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
    pub fn yields(&self) -> usize {
        self.yields.load(Ordering::SeqCst)
    }
    pub fn active_bodies(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }
    pub fn live_chunks(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }
    pub fn peak_chunks(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }
    pub async fn wait_between_chunks(&self) {
        self.boundary.notified().await;
    }
    pub async fn wait_idle(&self) {
        loop {
            let notified = self.idle.notified();
            if self.active_bodies() == 0 {
                return;
            }
            notified.await;
        }
    }
    pub(crate) fn body(self: &Arc<Self>) -> BodyGuard {
        self.active.fetch_add(1, Ordering::SeqCst);
        BodyGuard(self.clone())
    }
    pub(crate) async fn before_read(&self, after_first: bool) {
        if self.pause && after_first {
            self.boundary.notify_one();
            std::future::pending::<()>().await;
        }
    }
    pub(crate) fn yielded(&self) {
        self.yields.fetch_add(1, Ordering::SeqCst);
    }
    pub(crate) fn chunk(self: &Arc<Self>, len: usize) -> TrackedChunk {
        self.reads.fetch_add(1, Ordering::SeqCst);
        let live = self.live.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(live, Ordering::SeqCst);
        TrackedChunk {
            data: vec![0; len],
            probe: self.clone(),
        }
    }
}
pub(crate) struct BodyGuard(Arc<BodyProbe>);
impl Drop for BodyGuard {
    fn drop(&mut self) {
        self.0.active.fetch_sub(1, Ordering::SeqCst);
        self.0.idle.notify_one();
    }
}
pub(crate) struct TrackedChunk {
    pub(crate) data: Vec<u8>,
    probe: Arc<BodyProbe>,
}
impl AsRef<[u8]> for TrackedChunk {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}
impl Drop for TrackedChunk {
    fn drop(&mut self) {
        self.probe.live.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Exercise the same pull body without a socket's already delivered buffers.
pub fn body(
    source: Arc<SelectedMedia>,
    permit: room_server::MediaPermit,
) -> Result<axum::body::Body, StreamError> {
    let lease = ReaderLimits::default().try_acquire(&permit)?;
    let len = source.descriptor(0).size_bytes.get();
    Ok(crate::body::media_body(
        source,
        permit,
        lease,
        tokio_util::sync::CancellationToken::new(),
        0,
        len,
    ))
}
