use shared_protocol::Speed;
use uuid::Uuid;

use crate::EngineError;

/// Fresh for every selection; track IDs and decoder events belong to this generation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct PlaybackGeneration(Uuid);

impl PlaybackGeneration {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for PlaybackGeneration {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for PlaybackGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Track {
    pub id: i64,
    pub kind: TrackKind,
    pub title: Option<String>,
    pub language: Option<String>,
    pub selected: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrackKind {
    Audio,
    Subtitle,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EngineCommand {
    SetPaused(bool),
    SeekToMs(u64),
    SetSpeed(Speed),
    SetVolume(u8),
    SetMuted(bool),
    SelectAudio(i64),
    SelectSubtitle(Option<i64>),
    Stop,
}

impl EngineCommand {
    /// Validate values independent of decoder state. Track IDs and seek bounds
    /// must additionally be checked against the current generation by the worker.
    pub fn validate(&self) -> Result<(), EngineError> {
        match self {
            Self::SetVolume(volume) if *volume > 100 => Err(EngineError::InvalidCommand),
            _ => Ok(()),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlayerStatus {
    Idle,
    Loading,
    Paused,
    Playing,
    Buffering,
    Ended,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EngineSnapshot {
    pub generation: PlaybackGeneration,
    pub status: PlayerStatus,
    pub position_ms: u64,
    pub duration_ms: Option<u64>,
    pub speed: Speed,
    pub volume: u8,
    pub muted: bool,
    pub tracks: Vec<Track>,
    pub decoder: Option<DecoderInfo>,
}

/// Observed decoder properties, rather than an inference from requested settings.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecoderInfo {
    pub codec: String,
    pub hwdec: Option<String>,
    pub video_output: String,
}
