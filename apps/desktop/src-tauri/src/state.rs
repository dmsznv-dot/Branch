use media_engine::{EngineCommand, EngineError, PlaybackGeneration};
use serde::Deserialize;
#[derive(Debug, Deserialize)]
#[serde(tag = "type", content = "value", deny_unknown_fields)]
pub enum PlayerCommandDto {
    SetPaused(bool),
    SeekToMs(shared_protocol::WireU64),
    SetSpeed(shared_protocol::Speed),
    SetVolume(u8),
    SetMuted(bool),
    SelectAudio(String),
    SelectSubtitle(Option<String>),
    Stop,
}
pub fn authorize(
    label: &str,
    active: PlaybackGeneration,
    generation: &str,
) -> Result<(), EngineError> {
    if label != "main" || active.to_string() != generation {
        return Err(EngineError::InvalidCommand);
    }
    Ok(())
}
impl PlayerCommandDto {
    pub fn into_command(self) -> Result<EngineCommand, EngineError> {
        fn track(id: String) -> Result<i64, EngineError> {
            let n = id.parse::<i64>().map_err(|_| EngineError::InvalidCommand)?;
            if n.to_string() != id {
                return Err(EngineError::InvalidCommand);
            }
            Ok(n)
        }
        let command = match self {
            Self::SetPaused(v) => EngineCommand::SetPaused(v),
            Self::SeekToMs(v) => EngineCommand::SeekToMs(v.get()),
            Self::SetSpeed(v) => EngineCommand::SetSpeed(v),
            Self::SetVolume(v) => EngineCommand::SetVolume(v),
            Self::SetMuted(v) => EngineCommand::SetMuted(v),
            Self::SelectAudio(v) => EngineCommand::SelectAudio(track(v)?),
            Self::SelectSubtitle(v) => EngineCommand::SelectSubtitle(v.map(track).transpose()?),
            Self::Stop => EngineCommand::Stop,
        };
        command.validate()?;
        Ok(command)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ipc_is_narrow() {
        assert!(
            serde_json::from_str::<PlayerCommandDto>(r#"{"type":"command","value":"loadfile"}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<PlayerCommandDto>(r#"{"type":"Stop","path":"secret"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<PlayerCommandDto>(r#"{"type":"SeekToMs","value":42}"#).is_err()
        );
        assert!(
            serde_json::from_str::<PlayerCommandDto>(r#"{"type":"SetSpeed","value":3.0}"#).is_err()
        );
        let a = PlaybackGeneration::new();
        let b = PlaybackGeneration::new();
        assert_eq!(
            authorize("main", a, &b.to_string()),
            Err(EngineError::InvalidCommand)
        );
        assert_eq!(
            authorize("other", a, &a.to_string()),
            Err(EngineError::InvalidCommand)
        );
        assert_eq!(authorize("main", a, &a.to_string()), Ok(()));
    }
    #[test]
    fn numeric_values_checked_again() {
        assert_eq!(
            PlayerCommandDto::SetVolume(101).into_command(),
            Err(EngineError::InvalidCommand)
        );
        assert_eq!(
            PlayerCommandDto::SelectAudio("9223372036854775808".into()).into_command(),
            Err(EngineError::InvalidCommand)
        );
        assert_eq!(
            PlayerCommandDto::SeekToMs(shared_protocol::WireU64::new(42)).into_command(),
            Ok(EngineCommand::SeekToMs(42))
        );
    }
}

use media_engine::{EngineHandle, EngineSnapshot, PlayerStatus, TrackKind};
use serde::{Serialize, Serializer};
use shared_protocol::{Speed, WireU64};
#[derive(Clone, Debug, Serialize)]
pub struct PublicError {
    #[serde(serialize_with = "error_code")]
    pub code: EngineError,
}
fn error_code<S: Serializer>(code: &EngineError, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&code.to_string())
}
impl From<EngineError> for PublicError {
    fn from(code: EngineError) -> Self {
        Self { code }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct TrackView {
    pub id: String,
    pub kind: &'static str,
    pub title: Option<String>,
    pub language: Option<String>,
    pub selected: bool,
}
#[derive(Clone, Debug, Serialize)]
pub struct DecoderView {
    pub codec: String,
    pub hwdec: Option<String>,
    pub video_output: String,
}
#[derive(Clone, Debug, Serialize)]
pub struct PlayerView {
    pub generation: String,
    pub basename: Option<String>,
    pub local: bool,
    pub status: &'static str,
    pub position_ms: WireU64,
    pub duration_ms: Option<WireU64>,
    pub speed: Speed,
    pub volume: u8,
    pub muted: bool,
    pub tracks: Vec<TrackView>,
    pub decoder: Option<DecoderView>,
    pub unavailable: bool,
}
impl PlayerView {
    pub fn idle(unavailable: bool) -> Self {
        Self {
            generation: PlaybackGeneration::new().to_string(),
            basename: None,
            local: true,
            status: "Idle",
            position_ms: WireU64::new(0),
            duration_ms: None,
            speed: Speed::Normal,
            volume: 100,
            muted: false,
            tracks: vec![],
            decoder: None,
            unavailable,
        }
    }
    pub fn snapshot(snapshot: &EngineSnapshot, basename: Option<String>) -> Self {
        Self {
            generation: snapshot.generation.to_string(),
            basename,
            local: true,
            status: match snapshot.status {
                PlayerStatus::Idle => "Idle",
                PlayerStatus::Loading => "Loading",
                PlayerStatus::Paused => "Paused",
                PlayerStatus::Playing => "Playing",
                PlayerStatus::Buffering => "Buffering",
                PlayerStatus::Ended => "Ended",
                PlayerStatus::Failed => "Failed",
            },
            position_ms: WireU64::new(snapshot.position_ms),
            duration_ms: snapshot.duration_ms.map(WireU64::new),
            speed: snapshot.speed,
            volume: snapshot.volume,
            muted: snapshot.muted,
            tracks: snapshot
                .tracks
                .iter()
                .map(|t| TrackView {
                    id: t.id.to_string(),
                    kind: match t.kind {
                        TrackKind::Audio => "audio",
                        TrackKind::Subtitle => "subtitle",
                    },
                    title: t.title.clone(),
                    language: t.language.clone(),
                    selected: t.selected,
                })
                .collect(),
            decoder: snapshot.decoder.as_ref().map(|d| DecoderView {
                codec: d.codec.clone(),
                hwdec: d.hwdec.clone(),
                video_output: d.video_output.clone(),
            }),
            unavailable: false,
        }
    }
}
/// Engine, active generation and display name stay in Rust. Serializing an open
/// with commands prevents commands for the old selection entering the new core.
pub struct PlayerController {
    pub engine: Option<EngineHandle>,
    pub active: Option<PlaybackGeneration>,
    pub view: PlayerView,
    selection_serial: u64,
    surface_failed: bool,
}
impl PlayerController {
    pub fn unavailable() -> Self {
        Self {
            selection_serial: 0,
            surface_failed: false,
            engine: None,
            active: None,
            view: PlayerView::idle(true),
        }
    }
    pub fn new(engine: EngineHandle) -> Self {
        let snapshot = engine.subscribe().borrow().clone();
        let active = Some(snapshot.generation);
        Self {
            selection_serial: 0,
            surface_failed: false,
            engine: Some(engine),
            active,
            view: PlayerView::snapshot(&snapshot, None),
        }
    }
    pub fn surface_unavailable(&mut self) {
        self.surface_failed = true;
        self.view.status = "Failed";
        self.view.unavailable = true;
    }
    pub fn update(&mut self, snapshot: &EngineSnapshot) -> bool {
        if self.surface_failed || self.active != Some(snapshot.generation) {
            return false;
        }
        self.view = PlayerView::snapshot(snapshot, self.view.basename.clone());
        true
    }
    pub async fn command(
        &mut self,
        label: &str,
        generation: &str,
        dto: PlayerCommandDto,
    ) -> Result<(), EngineError> {
        if self.surface_failed {
            return Err(EngineError::Unavailable);
        }
        let active = self.active.ok_or(EngineError::Unavailable)?;
        authorize(label, active, generation)?;
        self.engine
            .as_ref()
            .ok_or(EngineError::Unavailable)?
            .command(active, dto.into_command()?)
            .await
    }
    pub fn reserve_selection(&mut self) -> Result<u64, EngineError> {
        self.selection_serial = self
            .selection_serial
            .checked_add(1)
            .ok_or(EngineError::Busy)?;
        Ok(self.selection_serial)
    }
    pub fn complete_selection(
        &mut self,
        serial: u64,
        basename: String,
        result: Result<PlaybackGeneration, EngineError>,
        snapshot: &EngineSnapshot,
    ) -> Result<PlayerView, EngineError> {
        if self.surface_failed {
            return Err(EngineError::Unavailable);
        }
        if serial != self.selection_serial
            || result.is_ok_and(|generation| generation != snapshot.generation)
        {
            return Err(EngineError::Cancelled);
        }
        self.active = Some(snapshot.generation);
        self.view = PlayerView::snapshot(snapshot, Some(basename));
        result?;
        Ok(self.view.clone())
    }
    pub fn begin_open(
        &mut self,
        file: media_engine::SelectedFile,
    ) -> Result<(u64, String, media_engine::OpenRequest), EngineError> {
        if self.surface_failed {
            return Err(EngineError::Unavailable);
        }
        if self.selection_serial == u64::MAX {
            return Err(EngineError::Busy);
        }
        let basename = file.display_name().to_owned();
        let request = self
            .engine
            .as_ref()
            .ok_or(EngineError::Unavailable)?
            .submit_open(file)?;
        let serial = self.reserve_selection()?;
        Ok((serial, basename, request))
    }
    pub fn finish_open(
        &mut self,
        serial: u64,
        basename: String,
        result: Result<PlaybackGeneration, EngineError>,
    ) -> Result<PlayerView, EngineError> {
        let snapshot = self
            .engine
            .as_ref()
            .ok_or(EngineError::Unavailable)?
            .subscribe()
            .borrow()
            .clone();
        self.complete_selection(serial, basename, result, &snapshot)
    }
}

/// Borrowed engine requests must release the controller before close takes the
/// engine to cancel native reads. Dropping an RPC future does not free the core.
pub async fn until_closed<T>(
    mut close: tokio::sync::watch::Receiver<bool>,
    request: impl std::future::Future<Output = Result<T, EngineError>>,
) -> Result<T, EngineError> {
    if *close.borrow() {
        return Err(EngineError::Cancelled);
    }
    tokio::select! {biased; _=close.wait_for(|closed|*closed)=>Err(EngineError::Cancelled),result=request=>result}
}
#[cfg(test)]
mod close_request_tests {
    use super::*;
    #[tokio::test]
    async fn close_interrupts_pending_open_and_releases_controller() {
        let controller = std::sync::Arc::new(tokio::sync::Mutex::new(()));
        let held = controller.clone();
        let (close, receive) = tokio::sync::watch::channel(false);
        let (ready, started) = tokio::sync::oneshot::channel();
        let pending = tokio::spawn(async move {
            let _guard = held.lock().await;
            let _ = ready.send(());
            until_closed(receive, std::future::pending::<Result<(), EngineError>>()).await
        });
        started.await.unwrap();
        close.send_replace(true);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_millis(100), pending)
                .await
                .unwrap()
                .unwrap(),
            Err(EngineError::Cancelled)
        );
        assert!(
            controller.try_lock().is_ok(),
            "close must acquire engine ownership without waiting for FILE_LOADED"
        );
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    fn snapshot(generation: PlaybackGeneration) -> EngineSnapshot {
        EngineSnapshot {
            generation,
            status: PlayerStatus::Paused,
            position_ms: 0,
            duration_ms: None,
            speed: Speed::Normal,
            volume: 100,
            muted: false,
            tracks: vec![],
            decoder: None,
        }
    }
    #[test]
    fn stale_a_completion_and_events_cannot_replace_selected_b() {
        let mut player = PlayerController::unavailable();
        let a = player.reserve_selection().unwrap();
        let b = player.reserve_selection().unwrap();
        let ga = PlaybackGeneration::new();
        let gb = PlaybackGeneration::new();
        player
            .complete_selection(b, "B.mkv".into(), Ok(gb), &snapshot(gb))
            .unwrap();
        assert!(matches!(
            player.complete_selection(a, "A.mkv".into(), Ok(ga), &snapshot(ga)),
            Err(EngineError::Cancelled)
        ));
        assert_eq!(player.view.basename.as_deref(), Some("B.mkv"));
        assert_eq!(player.active, Some(gb));
        assert!(!player.update(&snapshot(ga)));
        assert_eq!(player.view.generation, gb.to_string());
    }
}

#[cfg(test)]
mod surface_failure_tests {
    use super::*;
    #[test]
    fn surface_failure_remains_failed_after_decoder_event() {
        let mut player = PlayerController::unavailable();
        let g = PlaybackGeneration::new();
        let snapshot = EngineSnapshot {
            generation: g,
            status: PlayerStatus::Playing,
            position_ms: 0,
            duration_ms: None,
            speed: Speed::Normal,
            volume: 100,
            muted: false,
            tracks: vec![],
            decoder: None,
        };
        player.active = Some(g);
        player.view = PlayerView::snapshot(&snapshot, Some("movie.mkv".into()));
        player.surface_unavailable();
        assert!(!player.update(&snapshot));
        assert_eq!(player.view.status, "Failed");
        assert!(player.view.unavailable);
    }
}
