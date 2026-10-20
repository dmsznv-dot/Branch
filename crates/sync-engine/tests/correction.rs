use shared_protocol::{PlaybackStatus, Speed};
use sync_engine::{Correction, correction};

#[test]
fn correction_boundaries() {
    for drift in [0, 100, 101, 500, 501] {
        for behind in [true, false] {
            let target = 1000;
            let observed = if behind {
                target - drift
            } else {
                target + drift
            };
            let expected = match drift {
                0..=100 => Correction::Keep,
                101..=500 => Correction::Rate {
                    effective_speed: 1.5 * if behind { 1.02 } else { 0.98 },
                },
                _ => Correction::Seek {
                    position_ms: target,
                },
            };
            assert_eq!(
                correction(
                    target,
                    observed,
                    PlaybackStatus::Playing,
                    Speed::ThreeHalves,
                    1.5
                ),
                expected
            );
        }
    }
}

#[test]
fn converged_playback_restores_base_speed() {
    for speed in [
        Speed::Half,
        Speed::ThreeQuarters,
        Speed::Normal,
        Speed::FiveQuarters,
        Speed::ThreeHalves,
        Speed::Double,
    ] {
        for (target, observed) in [(1000, 1000), (1000, 900), (1000, 1100)] {
            assert_eq!(
                correction(
                    target,
                    observed,
                    PlaybackStatus::Playing,
                    speed,
                    speed.as_f64() * 1.02
                ),
                Correction::Rate {
                    effective_speed: speed.as_f64()
                }
            );
        }
    }
}

#[test]
fn paused_seeks_without_rate_correction() {
    for (target, observed, expected) in [
        (1000, 900, Correction::Keep),
        (1000, 1100, Correction::Keep),
        (1000, 899, Correction::Seek { position_ms: 1000 }),
        (1000, 1101, Correction::Seek { position_ms: 1000 }),
        (1000, 1501, Correction::Seek { position_ms: 1000 }),
    ] {
        assert_eq!(
            correction(
                target,
                observed,
                PlaybackStatus::Paused,
                Speed::Normal,
                1.02
            ),
            expected
        );
    }
}

#[test]
fn correction_handles_unsigned_extremes() {
    for (target, observed) in [(0, u64::MAX), (u64::MAX, 0)] {
        assert_eq!(
            correction(
                target,
                observed,
                PlaybackStatus::Playing,
                Speed::Normal,
                1.0
            ),
            Correction::Seek {
                position_ms: target
            }
        );
    }
}
