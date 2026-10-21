//! Separate-process decode probe. stdout contains fixed event names and typed
//! numeric/codec fields only; paths, titles, URIs and mpv logs are never printed.
use media_engine::{
    EngineCommand, EngineError, EngineHandle, EngineMode, PlayerStatus, SelectedFile, TrackKind,
};
use shared_protocol::Speed;
use std::{path::PathBuf, time::Duration};
#[tokio::main]
async fn main() -> std::process::ExitCode {
    let result = run().await;
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            println!("error code={error}");
            std::process::ExitCode::FAILURE
        }
    }
}
async fn run() -> Result<(), EngineError> {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--file")) {
        return Err(EngineError::InvalidCommand);
    }
    let path = PathBuf::from(args.next().ok_or(EngineError::InvalidCommand)?);
    if args.next().as_deref() != Some(std::ffi::OsStr::new("--headless")) || args.next().is_some() {
        return Err(EngineError::InvalidCommand);
    }
    let file = SelectedFile::open(&path)?;
    let engine = EngineHandle::start(EngineMode::Headless).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let result = tokio::time::timeout_at(deadline, verify(&engine, file))
        .await
        .map_err(|_| EngineError::DecoderFailed)
        .and_then(|r| r);
    let shutdown = engine.shutdown().await;
    result?;
    shutdown?;
    println!("shutdown");
    Ok(())
}
async fn verify(engine: &EngineHandle, file: SelectedFile) -> Result<(), EngineError> {
    let generation = engine.open(file).await?;
    let mut states = engine.subscribe();
    loop {
        let state = states.borrow().clone();
        let d = engine.diagnostics();
        if let Some(error) = d.failure {
            return Err(error);
        }
        if d.width > 0 && d.height > 0 && d.estimated_frame_number >= 2 && state.position_ms >= 100
        {
            let decoder = state.decoder.ok_or(EngineError::UnsupportedMedia)?;
            if decoder.hwdec.is_some() || decoder.video_output != "null" {
                return Err(EngineError::DecoderFailed);
            }
            println!(
                "decoded codec={} width={} height={} frames={} position_ms={} duration_ms={} audio_tracks={} subtitle_tracks={}",
                decoder.codec,
                d.width,
                d.height,
                d.estimated_frame_number,
                state.position_ms,
                state.duration_ms.ok_or(EngineError::UnsupportedMedia)?,
                state
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Audio)
                    .count(),
                state
                    .tracks
                    .iter()
                    .filter(|t| t.kind == TrackKind::Subtitle)
                    .count()
            );
            break;
        }
        states
            .changed()
            .await
            .map_err(|_| EngineError::DecoderFailed)?;
    }
    engine
        .command(generation, EngineCommand::SetPaused(true))
        .await?;
    while states.borrow().status != PlayerStatus::Paused {
        states
            .changed()
            .await
            .map_err(|_| EngineError::DecoderFailed)?;
    }
    println!("paused");
    engine
        .command(generation, EngineCommand::SeekToMs(1_000))
        .await?;
    while !(900..=1100).contains(&states.borrow().position_ms) {
        states
            .changed()
            .await
            .map_err(|_| EngineError::DecoderFailed)?;
    }
    println!("seek position_ms={}", states.borrow().position_ms);
    for speed in [
        Speed::Half,
        Speed::ThreeQuarters,
        Speed::Normal,
        Speed::FiveQuarters,
        Speed::ThreeHalves,
        Speed::Double,
    ] {
        engine
            .command(generation, EngineCommand::SetSpeed(speed))
            .await?;
        while states.borrow().speed != speed {
            states
                .changed()
                .await
                .map_err(|_| EngineError::DecoderFailed)?;
        }
        let (numerator, denominator) = speed.ratio();
        println!("speed numerator={numerator} denominator={denominator}");
    }
    // Invalid IDs cannot reach native commands even after valid track discovery.
    if engine
        .command(generation, EngineCommand::SelectAudio(i64::MAX))
        .await
        != Err(EngineError::InvalidCommand)
    {
        return Err(EngineError::DecoderFailed);
    }
    Ok(())
}
