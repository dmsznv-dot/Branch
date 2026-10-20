use serde::{Deserialize, Serialize};

use crate::{ClockEpochId, Speed, WireU64};

/// Monotonic milliseconds in a particular host-clock epoch, never wall time.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HostTime {
    pub epoch: ClockEpochId,
    pub ticks_ms: WireU64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlaybackStatus {
    Paused,
    Playing,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Playback {
    pub status: PlaybackStatus,
    pub position_ms: WireU64,
    pub speed: Speed,
    pub updated_at: HostTime,
}
