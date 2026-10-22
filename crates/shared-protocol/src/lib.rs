//! Shared wire contracts for the watch-together core.
//!
//! Counter values are decimal strings to preserve precision across JavaScript
//! clients. Command identity comes from the authenticated transport, never JSON.

mod command;
mod error;
mod ids;
mod playback;
mod room;
mod speed;
mod wire_u64;

pub use command::{CommandEnvelope, PlaybackCommand};
pub use error::{ErrorCode, ProtocolError};
pub use ids::{ClockEpochId, CommandId, MediaId, ParticipantId, RoomId};
pub use playback::{HostTime, Playback, PlaybackStatus};
pub use room::{ControlMode, MediaDescriptor, MediaFingerprint, RoomSnapshot};
pub use speed::Speed;
pub use wire_u64::WireU64;
