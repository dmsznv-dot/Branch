//! Typed local player commands and one explicitly selected file descriptor.
mod error;
mod model;
mod source;

pub use error::EngineError;
pub use model::{
    DecoderInfo, EngineCommand, EngineSnapshot, PlaybackGeneration, PlayerStatus, Track, TrackKind,
};
pub use source::SelectedFile;
