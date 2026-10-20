use serde::{Deserialize, Deserializer, Serialize};

use crate::{CommandId, HostTime, MediaId, RoomId, Speed, WireU64};

/// Only the authoritative server applies these global actions.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum PlaybackCommand {
    Play,
    Pause,
    Seek { position_ms: WireU64 },
    SetSpeed { speed: Speed },
}

// Serde's internally tagged unit variants ignore remaining map entries even
// with deny_unknown_fields. Empty struct variants enforce strictness while the
// public Rust API retains the natural Play/Pause unit variants.
impl<'de> Deserialize<'de> for PlaybackCommand {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            tag = "type",
            rename_all = "camelCase",
            rename_all_fields = "camelCase",
            deny_unknown_fields
        )]
        enum Input {
            Play {},
            Pause {},
            Seek { position_ms: WireU64 },
            SetSpeed { speed: Speed },
        }

        Ok(match Input::deserialize(deserializer)? {
            Input::Play {} => Self::Play,
            Input::Pause {} => Self::Pause,
            Input::Seek { position_ms } => Self::Seek { position_ms },
            Input::SetSpeed { speed } => Self::SetSpeed { speed },
        })
    }
}

/// The actor is supplied separately by the authenticated transport.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CommandEnvelope {
    pub room_id: RoomId,
    pub media_id: MediaId,
    pub command_id: CommandId,
    /// Client estimate of the host's monotonic time.
    pub issued_at: HostTime,
    pub action: PlaybackCommand,
}
