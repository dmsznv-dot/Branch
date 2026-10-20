use std::time::Instant;

use shared_protocol::{ClockEpochId, HostTime, WireU64};

pub trait Clock: Send + Sync {
    fn now(&self) -> HostTime;
}

/// Milliseconds since construction, scoped to the supplied host-clock epoch.
pub struct MonotonicClock {
    epoch: ClockEpochId,
    origin: Instant,
}

impl MonotonicClock {
    pub fn new(epoch: ClockEpochId) -> Self {
        Self {
            epoch,
            origin: Instant::now(),
        }
    }
}

impl Clock for MonotonicClock {
    fn now(&self) -> HostTime {
        HostTime {
            epoch: self.epoch,
            ticks_ms: WireU64::new(
                self.origin.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
            ),
        }
    }
}

/// Shared deterministic clock for tests. Advances saturate rather than wrap.
#[cfg(feature = "test-support")]
pub struct ManualClock {
    epoch: ClockEpochId,
    ticks_ms: std::sync::atomic::AtomicU64,
}

#[cfg(feature = "test-support")]
impl ManualClock {
    pub fn new(now: HostTime) -> Self {
        Self {
            epoch: now.epoch,
            ticks_ms: std::sync::atomic::AtomicU64::new(now.ticks_ms.get()),
        }
    }

    pub fn advance_ms(&self, delta: u64) {
        // try_update retries concurrent advances without losing increments.
        let _ = self.ticks_ms.try_update(
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
            |ticks| Some(ticks.saturating_add(delta)),
        );
    }
}

#[cfg(feature = "test-support")]
impl Clock for ManualClock {
    fn now(&self) -> HostTime {
        HostTime {
            epoch: self.epoch,
            ticks_ms: WireU64::new(self.ticks_ms.load(std::sync::atomic::Ordering::SeqCst)),
        }
    }
}
