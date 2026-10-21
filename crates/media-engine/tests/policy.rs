use media_engine::*;
#[path = "../src/mpv/state.rs"]
mod state;

#[test]
fn typed_commands_only() {
    let mut state = state::State::new();
    let generation = PlaybackGeneration::new();
    state.open(generation);
    assert_eq!(
        state.validate(generation, EngineCommand::SetVolume(101)),
        Err(EngineError::InvalidCommand)
    );
    assert_eq!(
        state.validate(generation, EngineCommand::SelectAudio(7)),
        Err(EngineError::InvalidCommand)
    );
    assert_eq!(
        state.validate(generation, EngineCommand::SelectSubtitle(Some(7))),
        Err(EngineError::InvalidCommand)
    );
    for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1.0] {
        assert_eq!(
            state.event(generation, state::Event::Position(value)),
            Err(EngineError::InvalidCommand)
        );
    }
    state.snapshot.duration_ms = Some(10_000);
    assert_eq!(
        state.validate(generation, EngineCommand::SeekToMs(20_000)),
        Ok(Some(EngineCommand::SeekToMs(10_000)))
    );
    state.snapshot.duration_ms = None;
    assert_eq!(
        state.validate(generation, EngineCommand::SeekToMs(20_000)),
        Ok(Some(EngineCommand::SeekToMs(20_000)))
    );
    assert_eq!(
        state.validate(PlaybackGeneration::new(), EngineCommand::Stop),
        Ok(None)
    );
}
