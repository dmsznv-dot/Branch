#[cfg(feature = "native-mpv")]
#[allow(dead_code)]
mod ffi;
#[allow(dead_code)]
mod state;
#[allow(dead_code)]
mod stream;

#[cfg(not(feature = "native-mpv"))]
use crate::{EngineCommand, EngineError, EngineSnapshot, PlaybackGeneration, SelectedFile};
#[cfg(not(feature = "native-mpv"))]
use tokio::sync::watch;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineMode {
    Headless,
    Native,
}
#[cfg(feature = "native-mpv")]
mod policy;
#[cfg(feature = "native-mpv")]
mod render;
#[cfg(feature = "native-mpv")]
mod worker;
#[cfg(feature = "native-mpv")]
pub use render::{GlResolver, MpvRenderer, RenderEndpoint, RenderTarget};
#[cfg(feature = "native-mpv")]
pub use worker::{DecodeDiagnostics, EngineHandle};
#[cfg(not(feature = "native-mpv"))]
pub struct EngineHandle;
#[cfg(not(feature = "native-mpv"))]
impl EngineHandle {
    pub async fn start(_mode: EngineMode) -> Result<Self, EngineError> {
        Err(EngineError::Unavailable)
    }
    pub fn submit_open(&self, _file: SelectedFile) -> Result<OpenRequest, EngineError> {
        Err(EngineError::Unavailable)
    }
    pub async fn open(&self, file: SelectedFile) -> Result<PlaybackGeneration, EngineError> {
        self.submit_open(file)?.wait().await
    }
    pub async fn command(
        &self,
        _generation: PlaybackGeneration,
        _command: EngineCommand,
    ) -> Result<(), EngineError> {
        Err(EngineError::Unavailable)
    }
    pub fn subscribe(&self) -> watch::Receiver<EngineSnapshot> {
        watch::channel(state::State::new().snapshot).1
    }
    pub async fn shutdown(self) -> Result<(), EngineError> {
        Ok(())
    }
}

/// An owned, generation-specific reply. Waiting does not borrow EngineHandle,
/// so a newer selection can be submitted while an earlier load is pending.
pub struct OpenRequest {
    receiver: tokio::sync::oneshot::Receiver<Result<crate::PlaybackGeneration, crate::EngineError>>,
}
impl OpenRequest {
    pub async fn wait(self) -> Result<crate::PlaybackGeneration, crate::EngineError> {
        self.receiver
            .await
            .map_err(|_| crate::EngineError::Cancelled)?
    }
}
