use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::ProtocolError;

/// An unsigned counter encoded as a canonical decimal JSON string.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct WireU64(u64);

impl WireU64 {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    fn parse(input: &str) -> Result<Self, ProtocolError> {
        if input.is_empty() || input.len() > 20 || (input.len() > 1 && input.starts_with('0')) {
            return Err(ProtocolError::InvalidWireU64);
        }
        let mut value = 0_u64;
        for byte in input.bytes() {
            if !byte.is_ascii_digit() {
                return Err(ProtocolError::InvalidWireU64);
            }
            value = value
                .checked_mul(10)
                .and_then(|v| v.checked_add(u64::from(byte - b'0')))
                .ok_or(ProtocolError::InvalidWireU64)?;
        }
        Ok(Self(value))
    }
}

impl Serialize for WireU64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for WireU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let input = String::deserialize(deserializer)?;
        Self::parse(&input).map_err(D::Error::custom)
    }
}
