/// Stable public failures; no operating-system or decoder text crosses this boundary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineError {
    Unavailable,
    UnsupportedMedia,
    InvalidCommand,
    Busy,
    SourceChanged,
    Cancelled,
    DecoderFailed,
    ShutdownTimeout,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Unavailable => "Unavailable",
            Self::UnsupportedMedia => "UnsupportedMedia",
            Self::InvalidCommand => "InvalidCommand",
            Self::Busy => "Busy",
            Self::SourceChanged => "SourceChanged",
            Self::Cancelled => "Cancelled",
            Self::DecoderFailed => "DecoderFailed",
            Self::ShutdownTimeout => "ShutdownTimeout",
        })
    }
}

impl std::error::Error for EngineError {}
