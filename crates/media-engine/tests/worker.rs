use media_engine::*;
#[path = "../src/mpv/state.rs"]
mod state;

#[test]
fn stale_generation_is_discarded() {
    let mut state = state::State::new();
    let a = PlaybackGeneration::new();
    let b = PlaybackGeneration::new();
    state.open(a);
    state.open(b);
    for event in [
        state::Event::Ended,
        state::Event::Failed,
        state::Event::Position(9.0),
    ] {
        state.event(a, event).unwrap();
    }
    assert_eq!(state.snapshot.generation, b);
    assert_eq!(state.snapshot.status, PlayerStatus::Loading);
    assert_eq!(state.snapshot.position_ms, 0);
}
