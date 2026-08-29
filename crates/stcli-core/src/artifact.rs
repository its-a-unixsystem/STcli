use std::{collections::HashMap, fmt, str::FromStr};

use rusqlite::{OptionalExtension, params};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use thiserror::Error;

use crate::{
    ContentHash, Store,
    identity::{artifact_revision_hash, canonical_json_hash, hash_parts},
    storage::{StorageError, append_event},
};

const ARTIFACT_SEMANTIC_DOMAIN: &str = "stcli:artifact-semantic:v1";
const CONTENT_BLOB_DOMAIN: &str = "stcli:content-blob:v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ArtifactKind {
    CharacterCardV1,
    CharacterCardV2,
    Lorebook,
    ChatCompletionPreset,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CharacterCardV1 => "character-card-v1",
            Self::CharacterCardV2 => "character-card-v2",
            Self::Lorebook => "lorebook",
            Self::ChatCompletionPreset => "chat-completion-preset",
        }
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ArtifactKind {
    type Err = ArtifactError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "character-card-v1" => Ok(Self::CharacterCardV1),
            "character-card-v2" => Ok(Self::CharacterCardV2),
            "lorebook" => Ok(Self::Lorebook),
            "chat-completion-preset" => Ok(Self::ChatCompletionPreset),
            _ => Err(ArtifactError::UnknownStoredKind(value.to_owned())),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactRecord {
    pub revision_hash: ContentHash,
    pub kind: ArtifactKind,
    pub source_format: String,
    pub semantic_hash: ContentHash,
    pub source_blob_hash: ContentHash,
    pub imported_event_id: String,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedArtifact {
    pub kind: ArtifactKind,
    pub semantic: Value,
    pub greetings: Vec<String>,
}

pub fn artifact_semantic_hash(value: &Value) -> Result<crate::ContentHash, serde_json::Error> {
    canonical_json_hash(ARTIFACT_SEMANTIC_DOMAIN, value)
}

pub fn content_blob_hash(source: &[u8]) -> crate::ContentHash {
    hash_parts(CONTENT_BLOB_DOMAIN, &[source])
}

impl Store {
    pub fn import_artifact(&mut self, source: &[u8]) -> Result<ArtifactRecord, ArtifactError> {
        let decoded = decode_artifact(source)?;
        let source_format = "json";
        let revision_hash = artifact_revision_hash(decoded.kind.as_str(), source_format, source);
        let semantic_hash =
            artifact_semantic_hash(&decoded.semantic).map_err(ArtifactError::Canonicalize)?;
        let source_blob_hash = content_blob_hash(source);

        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        Store::put_blob(&transaction, &source_blob_hash.to_string(), source)?;
        let payload = serde_json::json!({
            "revision_hash": revision_hash,
            "artifact_kind": decoded.kind,
            "source_format": source_format,
            "semantic_hash": semantic_hash,
            "source_blob_hash": source_blob_hash,
        });
        let event = append_event(&transaction, None, "artifact.imported", &payload)?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO artifact_revisions(revision_hash, artifact_kind, source_format, semantic_hash, source_blob_hash, imported_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    revision_hash.to_string(),
                    decoded.kind.as_str(),
                    source_format,
                    semantic_hash.to_string(),
                    source_blob_hash.to_string(),
                    event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        Store::add_blob_reference(
            &transaction,
            "artifact-revision",
            &revision_hash.to_string(),
            &source_blob_hash.to_string(),
        )?;
        transaction.commit().map_err(StorageError::Sqlite)?;

        Ok(ArtifactRecord {
            revision_hash,
            kind: decoded.kind,
            source_format: source_format.to_owned(),
            semantic_hash,
            source_blob_hash,
            imported_event_id: event.event_id.to_string(),
        })
    }

    pub fn artifact(
        &self,
        revision_hash: &ContentHash,
    ) -> Result<Option<ArtifactRecord>, ArtifactError> {
        self.connection
            .query_row(
                "SELECT revision_hash, artifact_kind, source_format, semantic_hash, source_blob_hash, imported_event_id FROM artifact_revisions WHERE revision_hash = ?1",
                [revision_hash.to_string()],
                decode_artifact_record,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(ArtifactError::Storage)
    }

    pub fn artifacts(&self) -> Result<Vec<ArtifactRecord>, ArtifactError> {
        let mut statement = self
            .connection
            .prepare("SELECT revision_hash, artifact_kind, source_format, semantic_hash, source_blob_hash, imported_event_id FROM artifact_revisions ORDER BY rowid")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([], decode_artifact_record)
            .map_err(StorageError::Sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(ArtifactError::Storage)
    }

    pub fn export_artifact(&self, revision_hash: &ContentHash) -> Result<Vec<u8>, ArtifactError> {
        let record = self
            .artifact(revision_hash)?
            .ok_or_else(|| ArtifactError::NotFound(revision_hash.clone()))?;
        self.blob(&record.source_blob_hash.to_string())?
            .ok_or_else(|| ArtifactError::MissingBlob(record.source_blob_hash))
    }

    pub fn decoded_artifact(
        &self,
        revision_hash: &ContentHash,
    ) -> Result<DecodedArtifact, ArtifactError> {
        let source = self.export_artifact(revision_hash)?;
        decode_artifact(&source)
    }
}

pub fn decode_artifact(source: &[u8]) -> Result<DecodedArtifact, ArtifactError> {
    if source.len() > crate::limits::MAX_ARTIFACT_BYTES {
        return Err(ArtifactError::SourceTooLarge {
            size: source.len(),
            limit: crate::limits::MAX_ARTIFACT_BYTES,
        });
    }
    let semantic = decode_unique_json(source)?;
    let object = semantic.as_object().ok_or(ArtifactError::ExpectedObject)?;
    let kind = detect_kind(object)?;
    let greetings = match kind {
        ArtifactKind::CharacterCardV1 => greeting_values(object),
        ArtifactKind::CharacterCardV2 => object
            .get("data")
            .and_then(Value::as_object)
            .map(greeting_values)
            .unwrap_or_default(),
        ArtifactKind::Lorebook | ArtifactKind::ChatCompletionPreset => Vec::new(),
    };
    Ok(DecodedArtifact {
        kind,
        semantic,
        greetings,
    })
}

pub fn decode_unique_json(source: &[u8]) -> Result<Value, ArtifactError> {
    let mut deserializer = serde_json::Deserializer::from_slice(source);
    let value =
        serde_path_to_error::deserialize::<_, UniqueValue>(&mut deserializer).map_err(|error| {
            ArtifactError::InvalidJson {
                path: error.path().to_string(),
                message: error.inner().to_string(),
            }
        })?;
    deserializer
        .end()
        .map_err(|error| ArtifactError::InvalidJson {
            path: String::new(),
            message: error.to_string(),
        })?;
    Ok(value.0)
}

fn detect_kind(object: &Map<String, Value>) -> Result<ArtifactKind, ArtifactError> {
    if object.get("spec").and_then(Value::as_str) == Some("chara_card_v2") {
        if object.get("data").and_then(Value::as_object).is_none() {
            return Err(ArtifactError::MissingField("data"));
        }
        return Ok(ArtifactKind::CharacterCardV2);
    }
    let v1_fields = [
        "name",
        "description",
        "personality",
        "scenario",
        "first_mes",
        "mes_example",
    ];
    if v1_fields.iter().all(|field| object.contains_key(*field)) {
        return Ok(ArtifactKind::CharacterCardV1);
    }
    if object.contains_key("entries") {
        return Ok(ArtifactKind::Lorebook);
    }
    if object.contains_key("prompts") && object.contains_key("prompt_order") {
        return Ok(ArtifactKind::ChatCompletionPreset);
    }
    Err(ArtifactError::UnknownFormat)
}

fn greeting_values(object: &Map<String, Value>) -> Vec<String> {
    object
        .get("first_mes")
        .and_then(Value::as_str)
        .into_iter()
        .chain(
            object
                .get("alternate_greetings")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str),
        )
        .map(str::to_owned)
        .collect()
}

fn decode_artifact_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactRecord> {
    let revision_hash: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let semantic_hash: String = row.get(3)?;
    let source_blob_hash: String = row.get(4)?;
    Ok(ArtifactRecord {
        revision_hash: revision_hash
            .parse()
            .map_err(|error| conversion_error(0, error))?,
        kind: kind.parse().map_err(|error| conversion_error(1, error))?,
        source_format: row.get(2)?,
        semantic_hash: semantic_hash
            .parse()
            .map_err(|error| conversion_error(3, error))?,
        source_blob_hash: source_blob_hash
            .parse()
            .map_err(|error| conversion_error(4, error))?,
        imported_event_id: row.get(5)?,
    })
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(Number::from(value))))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        self.visit_string(value.to_owned())
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut seen = HashMap::new();
        while let Some(key) = object.next_key::<String>()? {
            if seen.insert(key.clone(), ()).is_some() {
                return Err(de::Error::custom(format!("duplicate object key '{key}'")));
            }
            let value = object.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

#[derive(Debug, Error)]
pub enum ArtifactError {
    #[error("artifact storage failed: {0}")]
    Storage(#[from] StorageError),
    #[error("invalid JSON at '{path}': {message}")]
    InvalidJson { path: String, message: String },
    #[error("artifact JSON must contain an object at the root")]
    ExpectedObject,
    #[error("artifact is missing required field '{0}'")]
    MissingField(&'static str),
    #[error("JSON does not match a supported Phase 1 artifact format")]
    UnknownFormat,
    #[error("artifact source exceeds {limit} byte limit ({size} bytes)")]
    SourceTooLarge { size: usize, limit: usize },
    #[error("stored artifact kind '{0}' is unknown")]
    UnknownStoredKind(String),
    #[error("artifact revision {0} was not found")]
    NotFound(ContentHash),
    #[error("artifact source blob {0} is missing")]
    MissingBlob(ContentHash),
    #[error("artifact canonicalization failed: {0}")]
    Canonicalize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const CARD: &str = r#"{
        "spec":"chara_card_v2",
        "spec_version":"2.0",
        "data":{
            "name":"Alice",
            "description":"A librarian.",
            "personality":"Curious",
            "scenario":"An old library",
            "first_mes":"Welcome.",
            "mes_example":"",
            "alternate_greetings":["You came back."],
            "plugins":{"unknown":{"value":1}}
        }
    }"#;

    #[test]
    fn duplicate_keys_include_the_nested_path() {
        let error = decode_unique_json(br#"{"data":{"name":"Alice","name":"Bob"}}"#).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("data"));
        assert!(message.contains("duplicate object key 'name'"));
    }

    #[test]
    fn v2_card_exposes_default_and_alternate_greetings() {
        let artifact = decode_artifact(CARD.as_bytes()).unwrap();
        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV2);
        assert_eq!(artifact.greetings, ["Welcome.", "You came back."]);
    }

    #[test]
    fn imported_artifact_exports_original_bytes_after_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let revision = {
            let mut store = Store::open(&path).unwrap();
            store
                .import_artifact(CARD.as_bytes())
                .unwrap()
                .revision_hash
        };
        let store = Store::open(path).unwrap();
        assert_eq!(store.export_artifact(&revision).unwrap(), CARD.as_bytes());
        assert_eq!(
            store.decoded_artifact(&revision).unwrap().greetings.len(),
            2
        );
    }

    #[test]
    fn oversized_artifact_is_rejected() {
        let oversized = vec![b' '; crate::limits::MAX_ARTIFACT_BYTES + 1];
        let error = decode_artifact(&oversized).unwrap_err();
        assert!(
            error.to_string().contains("byte limit"),
            "expected SourceTooLarge, got: {error}"
        );
    }

    #[test]
    fn reformatting_creates_a_new_revision_with_same_semantic_hash() {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let compact =
            serde_jcs::to_vec(&decode_artifact(CARD.as_bytes()).unwrap().semantic).unwrap();
        let first = store.import_artifact(CARD.as_bytes()).unwrap();
        let second = store.import_artifact(&compact).unwrap();
        assert_ne!(first.revision_hash, second.revision_hash);
        assert_eq!(first.semantic_hash, second.semantic_hash);
    }
}
