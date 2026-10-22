use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as DeError};

use crate::{MediaId, Playback, RoomId, WireU64};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlMode {
    Everyone,
    HostOnly,
}

/// SHA-256 of the complete local media file. It identifies the bytes used by
/// each participant for synchronized playback; Debug intentionally redacts it.
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
pub struct MediaFingerprint([u8; 32]);

impl MediaFingerprint {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn expose(&self) -> String {
        let mut encoded = String::with_capacity(64);
        for byte in self.0 {
            use fmt::Write as _;
            write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
        }
        encoded
    }
}

impl FromStr for MediaFingerprint {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("invalid media fingerprint");
        }
        let mut bytes = [0; 32];
        for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(chunk).map_err(|_| "invalid media fingerprint")?;
            bytes[index] = u8::from_str_radix(pair, 16).map_err(|_| "invalid media fingerprint")?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for MediaFingerprint {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.expose())
    }
}

impl<'de> Deserialize<'de> for MediaFingerprint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_str(&String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl fmt::Debug for MediaFingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MediaFingerprint([REDACTED])")
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MediaDescriptor {
    pub media_id: MediaId,
    pub size_bytes: WireU64,
    pub duration_ms: WireU64,
    pub fingerprint: MediaFingerprint,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RoomSnapshot {
    pub room_id: RoomId,
    pub media: MediaDescriptor,
    pub revision: WireU64,
    pub playback: Playback,
    pub control: ControlMode,
}
