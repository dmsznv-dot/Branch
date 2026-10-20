use shared_protocol::{PlaybackStatus, Speed};

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    Keep,
    Rate { effective_speed: f64 },
    Seek { position_ms: u64 },
}

/// Produces a local player adjustment, never an authoritative command.
pub fn correction(
    target_ms: u64,
    observed_ms: u64,
    status: PlaybackStatus,
    base: Speed,
    effective_speed: f64,
) -> Correction {
    let drift = target_ms.abs_diff(observed_ms);
    if drift > 500 || (status == PlaybackStatus::Paused && drift > 100) {
        return Correction::Seek {
            position_ms: target_ms,
        };
    }
    if status == PlaybackStatus::Paused {
        return Correction::Keep;
    }
    let base_speed = base.as_f64();
    if drift > 100 {
        return Correction::Rate {
            effective_speed: base_speed * if target_ms > observed_ms { 1.02 } else { 0.98 },
        };
    }
    if effective_speed != base_speed {
        return Correction::Rate {
            effective_speed: base_speed,
        };
    }
    Correction::Keep
}
