//! Internal deterministic command/event boundary, also used by integration doubles.
use crate::{EngineCommand, EngineError, EngineSnapshot, PlaybackGeneration, PlayerStatus};
use shared_protocol::Speed;

#[allow(dead_code)]
pub enum Event {
    Position(f64),
    Ended,
    Failed,
}
pub struct State {
    pub snapshot: EngineSnapshot,
}
impl State {
    pub fn new() -> Self {
        Self {
            snapshot: EngineSnapshot {
                generation: PlaybackGeneration::new(),
                status: PlayerStatus::Idle,
                position_ms: 0,
                duration_ms: None,
                speed: Speed::Normal,
                volume: 100,
                muted: false,
                tracks: vec![],
                decoder: None,
            },
        }
    }
    pub fn open(&mut self, generation: PlaybackGeneration) {
        *self = Self::new();
        self.snapshot.generation = generation;
        self.snapshot.status = PlayerStatus::Loading;
    }
    pub fn event(
        &mut self,
        generation: PlaybackGeneration,
        event: Event,
    ) -> Result<(), EngineError> {
        if generation != self.snapshot.generation {
            return Ok(());
        }
        match event {
            Event::Position(s) => self.snapshot.position_ms = milliseconds(s)?,
            Event::Ended => self.snapshot.status = PlayerStatus::Ended,
            Event::Failed => self.snapshot.status = PlayerStatus::Failed,
        }
        Ok(())
    }
    #[allow(dead_code)]
    pub fn validate(
        &self,
        generation: PlaybackGeneration,
        command: EngineCommand,
    ) -> Result<Option<EngineCommand>, EngineError> {
        if generation != self.snapshot.generation {
            return Ok(None);
        }
        command.validate()?;
        match command {
            EngineCommand::SelectAudio(id)
                if !self
                    .snapshot
                    .tracks
                    .iter()
                    .any(|t| t.kind == crate::TrackKind::Audio && t.id == id) =>
            {
                Err(EngineError::InvalidCommand)
            }
            EngineCommand::SelectSubtitle(Some(id))
                if !self
                    .snapshot
                    .tracks
                    .iter()
                    .any(|t| t.kind == crate::TrackKind::Subtitle && t.id == id) =>
            {
                Err(EngineError::InvalidCommand)
            }
            EngineCommand::SeekToMs(ms) => Ok(Some(EngineCommand::SeekToMs(
                self.snapshot.duration_ms.map_or(ms, |d| ms.min(d)),
            ))),
            c => Ok(Some(c)),
        }
    }
}

#[allow(dead_code)]
pub fn milliseconds(seconds: f64) -> Result<u64, EngineError> {
    if !seconds.is_finite() || seconds < 0.0 || seconds * 1000.0 >= u64::MAX as f64 {
        return Err(EngineError::InvalidCommand);
    }
    Ok((seconds * 1000.0).round() as u64)
}
