use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};
use uuid::{Uuid, Variant, Version};

use crate::ProtocolError;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn parse(input: &str) -> Result<Self, ProtocolError> {
                let uuid = Uuid::parse_str(input).map_err(|_| ProtocolError::InvalidUuid)?;
                if uuid.get_version() != Some(Version::Random)
                    || uuid.get_variant() != Variant::RFC4122
                {
                    return Err(ProtocolError::InvalidUuid);
                }
                Ok(Self(uuid))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.collect_str(self)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let input = String::deserialize(deserializer)?;
                Self::parse(&input).map_err(D::Error::custom)
            }
        }
    };
}

uuid_id!(RoomId);
uuid_id!(MediaId);
uuid_id!(ParticipantId);
uuid_id!(CommandId);
uuid_id!(ClockEpochId);
