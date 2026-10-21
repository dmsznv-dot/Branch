pub mod geometry;
#[cfg(all(
    feature = "native-host",
    any(target_os = "macos", target_os = "windows")
))]
mod host;
pub mod lifecycle;
#[cfg(all(feature = "native-host", target_os = "macos"))]
mod macos;
#[cfg(all(feature = "native-host", target_os = "windows"))]
mod windows;
#[cfg(all(
    feature = "native-host",
    any(target_os = "macos", target_os = "windows")
))]
pub use host::NativeSurface;
#[cfg(all(
    feature = "native-host",
    not(any(target_os = "macos", target_os = "windows"))
))]
pub struct NativeSurface;
#[cfg(all(
    feature = "native-host",
    not(any(target_os = "macos", target_os = "windows"))
))]
impl NativeSurface {
    pub fn errors(&self) -> tokio::sync::watch::Receiver<Option<media_engine::EngineError>> {
        tokio::sync::watch::channel(None).1
    }
    pub fn has_retained_resources() -> bool {
        false
    }
    pub fn attach(
        _window: &tauri::Window,
        _endpoint: media_engine::RenderEndpoint,
    ) -> Result<Self, media_engine::EngineError> {
        Err(media_engine::EngineError::Unavailable)
    }
    pub fn resize(
        &self,
        geometry: geometry::SurfaceGeometry,
    ) -> Result<(), media_engine::EngineError> {
        geometry.physical()?;
        Err(media_engine::EngineError::Unavailable)
    }
    pub async fn shutdown(self) -> Result<(), media_engine::EngineError> {
        Err(media_engine::EngineError::Unavailable)
    }
}
