use std::{error::Error, fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};

use crate::IdParseError;

fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex<const N: usize>(text: &str) -> Result<[u8; N], IdParseError> {
    if text.len() != N * 2 {
        return Err(IdParseError::WrongLength {
            expected: N * 2,
            actual: text.len(),
        });
    }

    fn nibble(value: u8, index: usize) -> Result<u8, IdParseError> {
        match value {
            b'0'..=b'9' => Ok(value - b'0'),
            b'a'..=b'f' => Ok(value - b'a' + 10),
            b'A'..=b'F' => Ok(value - b'A' + 10),
            _ => Err(IdParseError::InvalidHex { index }),
        }
    }

    let source = text.as_bytes();
    let mut result = [0_u8; N];
    for (index, output) in result.iter_mut().enumerate() {
        let high = nibble(source[index * 2], index * 2)?;
        let low = nibble(source[index * 2 + 1], index * 2 + 1)?;
        *output = (high << 4) | low;
    }
    Ok(result)
}

macro_rules! stable_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name([u8; 16]);

        impl $name {
            pub const NIL: Self = Self([0; 16]);

            pub const fn from_bytes(bytes: [u8; 16]) -> Self {
                Self(bytes)
            }

            pub const fn from_u128(value: u128) -> Self {
                Self(value.to_be_bytes())
            }

            pub const fn as_bytes(&self) -> &[u8; 16] {
                &self.0
            }

            pub fn is_nil(self) -> bool {
                self.0 == [0; 16]
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&encode_hex(&self.0))
            }
        }

        impl FromStr for $name {
            type Err = IdParseError;

            fn from_str(text: &str) -> Result<Self, Self::Err> {
                decode_hex(text).map(Self)
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(&self.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let text = String::deserialize(deserializer)?;
                text.parse().map_err(de::Error::custom)
            }
        }
    };
}

stable_id!(ProjectId);
stable_id!(FrameId);
stable_id!(CaptureClockId);
stable_id!(TrackId);
stable_id!(OverlayId);
stable_id!(TransitionId);

/// A BLAKE3 content digest. Unlike the entity IDs above this is derived from
/// immutable bytes, never generated randomly.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct AssetId([u8; 32]);

impl AssetId {
    pub const fn from_digest(digest: [u8; 32]) -> Self {
        Self(digest)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn is_zero(self) -> bool {
        self.0 == [0; 32]
    }
}

impl fmt::Display for AssetId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&encode_hex(&self.0))
    }
}

impl FromStr for AssetId {
    type Err = IdParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        decode_hex(text).map(Self)
    }
}

impl Serialize for AssetId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for AssetId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(de::Error::custom)
    }
}

// Assert error types used by serde remain conventional errors.
const _: fn() = || {
    fn assert_error<T: Error>() {}
    assert_error::<IdParseError>();
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_fixed_width_lowercase_hex_in_json() {
        let id = FrameId::from_u128(0xabcd);
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, r#""0000000000000000000000000000abcd""#);
        assert_eq!(serde_json::from_str::<FrameId>(&json).unwrap(), id);
    }

    #[test]
    fn asset_ids_round_trip() {
        let id = AssetId::from_digest([0x5a; 32]);
        assert_eq!(id.to_string().len(), 64);
        assert_eq!(id.to_string().parse::<AssetId>().unwrap(), id);
    }
}
