//! Typed local player commands and one explicitly selected file descriptor.
mod error;
mod model;
mod source;

pub use error::EngineError;
pub use model::{
    DecoderInfo, EngineCommand, EngineSnapshot, PlaybackGeneration, PlayerStatus, Track, TrackKind,
};
pub use source::SelectedFile;
mod mpv;
pub use mpv::{EngineHandle, EngineMode, OpenRequest};

#[cfg(feature = "native-mpv")]
pub use mpv::{DecodeDiagnostics, GlResolver, MpvRenderer, RenderEndpoint, RenderTarget};
