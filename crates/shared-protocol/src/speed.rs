use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error};

use crate::ProtocolError;

/// The six allowed global speeds, stored exactly as rational values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Speed {
    Half,
    ThreeQuarters,
    Normal,
    FiveQuarters,
    ThreeHalves,
    Double,
}

impl Speed {
    pub const fn ratio(self) -> (u32, u32) {
        match self {
            Self::Half => (1, 2),
            Self::ThreeQuarters => (3, 4),
            Self::Normal => (1, 1),
            Self::FiveQuarters => (5, 4),
            Self::ThreeHalves => (3, 2),
            Self::Double => (2, 1),
        }
    }

    pub fn as_f64(self) -> f64 {
        let (numerator, denominator) = self.ratio();
        f64::from(numerator) / f64::from(denominator)
    }
}

impl TryFrom<f64> for Speed {
    type Error = ProtocolError;

    fn try_from(value: f64) -> Result<Self, Self::Error> {
        match value {
            0.5 => Ok(Self::Half),
            0.75 => Ok(Self::ThreeQuarters),
            1.0 => Ok(Self::Normal),
            1.25 => Ok(Self::FiveQuarters),
            1.5 => Ok(Self::ThreeHalves),
            2.0 => Ok(Self::Double),
            _ => Err(ProtocolError::InvalidSpeed),
        }
    }
}

impl Serialize for Speed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for Speed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = f64::deserialize(deserializer)?;
        Self::try_from(value).map_err(D::Error::custom)
    }
}
