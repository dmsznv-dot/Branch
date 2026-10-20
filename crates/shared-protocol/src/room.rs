use serde::{Deserialize, Serialize};

use crate::{MediaId, Playback, RoomId, WireU64};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlMode {
    Everyone,
    HostOnly,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaDescriptor {
    pub media_id: MediaId,
    pub size_bytes: WireU64,
    pub duration_ms: WireU64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomSnapshot {
    pub room_id: RoomId,
    pub media: MediaDescriptor,
    pub revision: WireU64,
    pub playback: Playback,
    pub control: ControlMode,
}
