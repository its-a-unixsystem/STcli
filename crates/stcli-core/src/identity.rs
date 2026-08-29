use std::{fmt, str::FromStr};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use ulid::Ulid;

const ARTIFACT_REVISION_DOMAIN: &str = "stcli:artifact-revision:v1";
const SESSION_PROJECTION_DOMAIN: &str = "stcli:session-projection:v1";

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "sha256:")?;
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for ContentHash {
    type Err = ParseContentHashError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let hex = value
            .strip_prefix("sha256:")
            .ok_or(ParseContentHashError::MissingPrefix)?;
        if hex.len() != 64 {
            return Err(ParseContentHashError::InvalidLength(hex.len()));
        }

        let mut bytes = [0_u8; 32];
        for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
            let pair = std::str::from_utf8(pair).expect("hex pairs are valid UTF-8");
            bytes[index] =
                u8::from_str_radix(pair, 16).map_err(|_| ParseContentHashError::InvalidHex)?;
        }
        Ok(Self(bytes))
    }
}

impl Serialize for ContentHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        value.parse().map_err(de::Error::custom)
    }
}

#[derive(Debug, Error)]
pub enum ParseContentHashError {
    #[error("content hash must start with 'sha256:'")]
    MissingPrefix,
    #[error("SHA-256 hex payload must contain 64 characters, found {0}")]
    InvalidLength(usize),
    #[error("content hash contains non-hexadecimal characters")]
    InvalidHex,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(transparent)]
pub struct EntityId(Ulid);

impl EntityId {
    pub fn new() -> Self {
        Self(Ulid::new())
    }

    pub fn from_ulid(value: Ulid) -> Self {
        Self(value)
    }

    pub fn into_ulid(self) -> Ulid {
        self.0
    }
}

impl Default for EntityId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for EntityId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for EntityId {
    type Err = ulid::DecodeError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(Self)
    }
}

pub fn artifact_revision_hash(kind: &str, source_format: &str, source: &[u8]) -> ContentHash {
    hash_parts(
        ARTIFACT_REVISION_DOMAIN,
        &[kind.as_bytes(), source_format.as_bytes(), source],
    )
}

pub fn canonical_json(value: &Value) -> Result<Vec<u8>, serde_json::Error> {
    serde_jcs::to_vec(value)
}

pub fn canonical_json_hash(domain: &str, value: &Value) -> Result<ContentHash, serde_json::Error> {
    let canonical = canonical_json(value)?;
    Ok(hash_parts(domain, &[&canonical]))
}

pub fn session_projection_hash(value: &Value) -> Result<ContentHash, serde_json::Error> {
    canonical_json_hash(SESSION_PROJECTION_DOMAIN, value)
}

pub fn hash_parts(domain: &str, parts: &[&[u8]]) -> ContentHash {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update([0]);
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part);
    }
    ContentHash::new(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;
    use serde_json::json;
    use serde_json::{Number, Value};

    fn json_value() -> impl Strategy<Value = Value> {
        let leaf = prop_oneof![
            Just(Value::Null),
            any::<bool>().prop_map(Value::Bool),
            any::<i64>().prop_map(|value| Value::Number(Number::from(value))),
            ".{0,32}".prop_map(Value::String),
        ];
        leaf.prop_recursive(4, 64, 8, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 0..8).prop_map(Value::Array),
                prop::collection::btree_map("[a-zA-Z0-9_]{1,12}", inner, 0..8)
                    .prop_map(|entries| Value::Object(entries.into_iter().collect())),
            ]
        })
    }

    #[test]
    fn canonical_json_orders_object_members() {
        let value = json!({"z": 1, "a": 2});
        assert_eq!(canonical_json(&value).unwrap(), br#"{"a":2,"z":1}"#);
    }

    #[test]
    fn content_hash_round_trips_through_text() {
        let hash = artifact_revision_hash("character-card", "json", b"{}");
        assert_eq!(hash.to_string().parse::<ContentHash>().unwrap(), hash);
    }

    #[test]
    fn domain_separation_changes_hashes() {
        let value = json!({"a": 1});
        let first = canonical_json_hash("stcli:first:v1", &value).unwrap();
        let second = canonical_json_hash("stcli:second:v1", &value).unwrap();
        assert_ne!(first, second);
    }

    proptest! {
        #[test]
        fn canonical_json_is_a_serialization_fixpoint(value in json_value()) {
            let first = canonical_json(&value).unwrap();
            let reparsed = serde_json::from_slice::<Value>(&first).unwrap();
            let second = canonical_json(&reparsed).unwrap();

            prop_assert_eq!(first, second);
        }

        #[test]
        fn canonical_json_sorts_flat_object_keys_independently(
            entries in prop::collection::btree_map("[a-zA-Z0-9_]{1,12}", ".{0,16}", 2..8)
        ) {
            let value = Value::Object(
                entries.iter().map(|(k, v)| (k.clone(), Value::String(v.clone()))).collect(),
            );
            let canonical = String::from_utf8(canonical_json(&value).unwrap()).unwrap();
            let mut sorted = entries.into_iter().collect::<Vec<_>>();
            sorted.sort_by(|(a, _), (b, _)| a.cmp(b));
            let expected = format!(
                "{{{}}}",
                sorted
                    .iter()
                    .map(|(key, value)| format!(
                        "{}:{}",
                        serde_json::to_string(key).unwrap(),
                        serde_json::to_string(value).unwrap()
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            );

            prop_assert_eq!(canonical, expected);
        }
        #[test]
        fn hash_domains_remain_separated_for_equivalent_payload_bytes(
            payload in prop::collection::vec(any::<u8>(), 0..256)
        ) {
            let value = Value::Array(
                payload.into_iter().map(|byte| Value::Number(byte.into())).collect()
            );
            prop_assert_ne!(
                canonical_json_hash("stcli:property:first:v1", &value).unwrap(),
                canonical_json_hash("stcli:property:second:v1", &value).unwrap(),
            );
        }
    }
}
