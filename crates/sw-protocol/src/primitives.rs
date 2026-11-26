use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use thiserror::Error;
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Counter(pub u64);

impl fmt::Display for Counter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Debug, Error)]
pub enum InvalidValue {
    #[error("expected a canonical unsigned decimal counter in the u64 range")]
    Counter,
    #[error("expected a cursor in generation:sequence form")]
    Cursor,
    #[error("reason must contain 1 to 512 non-control characters")]
    Reason,
    #[error(
        "idempotency key must contain 1 to 128 ASCII letters, digits, dots, underscores, or hyphens"
    )]
    IdempotencyKey,
}

impl FromStr for Counter {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.is_empty()
            || value.len() > 20
            || (value.starts_with('0') && value.len() > 1)
            || !value.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(InvalidValue::Counter);
        }
        value.parse().map(Self).map_err(|_| InvalidValue::Counter)
    }
}

impl Serialize for Counter {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Counter {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Cursor {
    pub generation: Uuid,
    pub sequence: Counter,
}

impl fmt::Display for Cursor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.generation, self.sequence)
    }
}

impl FromStr for Cursor {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (generation, sequence) = value.split_once(':').ok_or(InvalidValue::Cursor)?;
        let id = Uuid::parse_str(generation).map_err(|_| InvalidValue::Cursor)?;
        if id.hyphenated().encode_lower(&mut Uuid::encode_buffer()) != generation {
            return Err(InvalidValue::Cursor);
        }
        Ok(Self {
            generation: id,
            sequence: sequence.parse()?,
        })
    }
}

impl Serialize for Cursor {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Cursor {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer)?
            .parse()
            .map_err(de::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(try_from = "String", into = "String")]
pub struct Reason(String);

impl TryFrom<String> for Reason {
    type Error = InvalidValue;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value.trim().is_empty()
            || value.chars().count() > 512
            || value.chars().any(char::is_control)
        {
            return Err(InvalidValue::Reason);
        }
        Ok(Self(value))
    }
}

impl From<Reason> for String {
    fn from(value: Reason) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl FromStr for IdempotencyKey {
    type Err = InvalidValue;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if !(1..=128).contains(&value.len())
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(InvalidValue::IdempotencyKey);
        }
        Ok(Self(value.to_owned()))
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct UtcTimestamp(#[serde(with = "time::serde::rfc3339")] OffsetDateTime);

impl UtcTimestamp {
    pub fn from_unix_millis(value: i64) -> Result<Self, time::error::ComponentRange> {
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(value) * 1_000_000).map(Self)
    }

    pub fn unix_millis(self) -> i64 {
        (self.0.unix_timestamp_nanos() / 1_000_000) as i64
    }
}

#[cfg(feature = "schema")]
mod schemas {
    use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
    use std::borrow::Cow;

    use super::*;

    impl JsonSchema for Counter {
        fn schema_name() -> Cow<'static, str> {
            "Counter".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({"type": "string", "pattern": "^(0|[1-9][0-9]{0,19})$", "description": "Canonical decimal u64 in 0..=18446744073709551615. Range is checked by the parser."})
        }
    }

    impl JsonSchema for Cursor {
        fn schema_name() -> Cow<'static, str> {
            "Cursor".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({"type": "string", "pattern": "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}:(0|[1-9][0-9]{0,19})$"})
        }
    }

    impl JsonSchema for Reason {
        fn schema_name() -> Cow<'static, str> {
            "Reason".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({"type": "string", "minLength": 1, "maxLength": 512, "pattern": "\\S"})
        }
    }

    impl JsonSchema for UtcTimestamp {
        fn schema_name() -> Cow<'static, str> {
            "UtcTimestamp".into()
        }
        fn json_schema(_: &mut SchemaGenerator) -> Schema {
            json_schema!({"type": "string", "format": "date-time"})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u64_counters_round_trip_as_canonical_strings() {
        let number = Counter(u64::MAX);
        let encoded = serde_json::to_string(&number).unwrap();
        assert_eq!(encoded, "\"18446744073709551615\"");
        assert_eq!(serde_json::from_str::<Counter>(&encoded).unwrap(), number);
        for invalid in [
            "1",
            "\"01\"",
            "\"+4\"",
            "\"-1\"",
            "\"18446744073709551616\"",
        ] {
            assert!(serde_json::from_str::<Counter>(invalid).is_err());
        }
    }

    #[test]
    fn cursor_carries_the_generation_and_exact_sequence() {
        let cursor = Cursor {
            generation: Uuid::nil(),
            sequence: Counter(1 << 61),
        };
        assert_eq!(cursor.to_string().parse::<Cursor>().unwrap(), cursor);
        assert!(
            "00000000000000000000000000000000:0"
                .parse::<Cursor>()
                .is_err()
        );
        assert!(
            "00000000-0000-0000-0000-000000000000:0:0"
                .parse::<Cursor>()
                .is_err()
        );
    }
}
