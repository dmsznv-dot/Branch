use crate::RoomError;

pub(crate) const SESSION_TTL_MS: u64 = 300_000;
pub(crate) const COMMAND_TTL_MS: u64 = 120_000;
pub(crate) const FUTURE_ALLOWANCE_MS: u64 = 2_000;
pub(crate) const DEDUP_CAPACITY: usize = 256;

#[derive(Default)]
pub(crate) struct FixedWindow {
    window: u64,
    count: u32,
}
impl FixedWindow {
    pub(crate) fn take(&mut self, now: u64, period: u64, max: u32) -> Result<(), RoomError> {
        let window = now / period;
        if self.window != window {
            self.window = window;
            self.count = 0;
        }
        if self.count >= max {
            return Err(RoomError::RateLimited);
        }
        self.count = self
            .count
            .checked_add(1)
            .ok_or(RoomError::CounterExhausted)?;
        Ok(())
    }
}

pub(crate) fn validate_name(name: &str) -> Result<(), RoomError> {
    if name.is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        return Err(RoomError::InvalidInput);
    }
    Ok(())
}
