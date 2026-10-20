use std::collections::VecDeque;

use shared_protocol::ClockEpochId;

use crate::SyncError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockSample {
    pub client_send_ms: u64,
    pub host_recv_ms: u64,
    pub host_send_ms: u64,
    pub client_recv_ms: u64,
    pub host_epoch: ClockEpochId,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ClockEstimate {
    /// Add to client monotonic ticks to obtain host ticks in `host_epoch`.
    pub offset_ms: i128,
    pub rtt_ms: u64,
    pub host_epoch: ClockEpochId,
}

/// Selects the minimum network RTT from the last eight valid observations.
#[derive(Default)]
pub struct ClockEstimator {
    samples: VecDeque<ClockEstimate>,
}

impl ClockEstimator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn observe(&mut self, sample: ClockSample) -> Result<(), SyncError> {
        let client_elapsed = sample
            .client_recv_ms
            .checked_sub(sample.client_send_ms)
            .ok_or(SyncError::InvalidClockSample)?;
        let processing = sample
            .host_send_ms
            .checked_sub(sample.host_recv_ms)
            .ok_or(SyncError::InvalidClockSample)?;
        let rtt_ms = client_elapsed
            .checked_sub(processing)
            .ok_or(SyncError::InvalidClockSample)?;
        if rtt_ms > 2000 {
            return Err(SyncError::RoundTripTooHigh);
        }
        let offset_ms = ((i128::from(sample.host_recv_ms) - i128::from(sample.client_send_ms))
            + (i128::from(sample.host_send_ms) - i128::from(sample.client_recv_ms)))
            / 2;

        // Validate before replacing the current epoch's usable observations.
        if self
            .samples
            .back()
            .is_some_and(|previous| previous.host_epoch != sample.host_epoch)
        {
            self.samples.clear();
        }
        if self.samples.len() == 8 {
            self.samples.pop_front();
        }
        self.samples.push_back(ClockEstimate {
            offset_ms,
            rtt_ms,
            host_epoch: sample.host_epoch,
        });
        Ok(())
    }

    pub fn estimate(&self) -> Option<ClockEstimate> {
        // Prefer the newest observation when RTTs tie.
        self.samples
            .iter()
            .rev()
            .min_by_key(|sample| sample.rtt_ms)
            .copied()
    }
}
