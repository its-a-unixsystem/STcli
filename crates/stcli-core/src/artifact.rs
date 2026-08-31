mod container;

use std::{
    borrow::Cow,
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
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
    CharacterCardV3,
    Lorebook,
    ChatCompletionPreset,
}

impl ArtifactKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CharacterCardV1 => "character-card-v1",
            Self::CharacterCardV2 => "character-card-v2",
            Self::CharacterCardV3 => "character-card-v3",
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
            "character-card-v3" => Ok(Self::CharacterCardV3),
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

#[derive(Clone, Debug, Serialize)]
pub struct ArtifactBundle {
    pub primary: ArtifactRecord,
    pub supplementary_artifacts: Vec<ArtifactRecord>,
    pub asset_count: usize,
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

#[derive(Clone, Debug, PartialEq)]
pub struct PresetPatch {
    pub preset_name: String,
    pub temperature: f64,
    pub max_context: u64,
    pub max_tokens: u64,
    pub use_sysprompt: bool,
}

pub fn clone_and_patch_preset(source: &[u8], patch: PresetPatch) -> Result<Vec<u8>, ArtifactError> {
    let mut decoded = decode_artifact(source)?;
    if decoded.kind != ArtifactKind::ChatCompletionPreset {
        return Err(ArtifactError::ChatCompletionPresetRequired(decoded.kind));
    }
    let object = decoded
        .semantic
        .as_object_mut()
        .ok_or(ArtifactError::ExpectedObject)?;
    object.insert("preset_name".to_owned(), Value::String(patch.preset_name));
    object.insert(
        "temperature".to_owned(),
        Value::Number(
            Number::from_f64(patch.temperature)
                .ok_or(ArtifactError::InvalidPresetTemperature(patch.temperature))?,
        ),
    );
    object.insert(
        "max_context".to_owned(),
        Value::Number(patch.max_context.into()),
    );
    object.insert(
        "openai_max_context".to_owned(),
        Value::Number(patch.max_context.into()),
    );
    object.insert(
        "openai_max_tokens".to_owned(),
        Value::Number(patch.max_tokens.into()),
    );
    object.insert("use_sysprompt".to_owned(), Value::Bool(patch.use_sysprompt));
    serde_json::to_vec_pretty(&decoded.semantic).map_err(ArtifactError::Canonicalize)
}

pub(crate) fn artifact_source_blob_hash(
    source: &[u8],
) -> Result<crate::ContentHash, ArtifactError> {
    Ok(content_blob_hash(&artifact_payload(source)?))
}

impl Store {
    pub fn import_artifact(&mut self, source: &[u8]) -> Result<ArtifactRecord, ArtifactError> {
        if container::is_charx(source) {
            return self
                .import_artifact_bundle(source)
                .map(|bundle| bundle.primary);
        }
        self.import_single_artifact(source)
    }

    pub fn import_artifact_bundle(
        &mut self,
        source: &[u8],
    ) -> Result<ArtifactBundle, ArtifactError> {
        if container::is_charx(source) {
            return self.import_charx(source);
        }
        let asset_count =
            usize::from(source.starts_with(container::PNG_SIGNATURE) || container::is_webp(source));
        Ok(ArtifactBundle {
            primary: self.import_single_artifact(source)?,
            supplementary_artifacts: Vec::new(),
            asset_count,
        })
    }

    fn import_single_artifact(&mut self, source: &[u8]) -> Result<ArtifactRecord, ArtifactError> {
        let artifact_payload = artifact_payload(source)?;
        let decoded = decode_artifact_payload(&artifact_payload)?;
        validate_webp_card_kind(source, &decoded)?;
        let (source_format, avatar_path) = if source.starts_with(container::PNG_SIGNATURE) {
            ("png", Some("avatar.png"))
        } else if container::is_webp(source) {
            ("webp", Some("avatar.webp"))
        } else {
            ("json", None)
        };
        let assets_root = avatar_path.map(|_| self.assets_root().to_owned());

        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let record = insert_artifact_revision(
            &transaction,
            source,
            &artifact_payload,
            &decoded,
            source_format,
        )?;
        if let (Some(root), Some(avatar_path)) = (assets_root.as_deref(), avatar_path) {
            let avatar = Store::put_asset_in_transaction(&transaction, root, source)?;
            Store::add_asset_reference_in_transaction(
                &transaction,
                "artifact-revision",
                &record.revision_hash.to_string(),
                &avatar.hash,
                avatar_path,
            )?;
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(record)
    }

    fn import_charx(&mut self, source: &[u8]) -> Result<ArtifactBundle, ArtifactError> {
        let mut archive = container::extract_charx(source)?;
        let primary_decoded = decode_artifact_payload(&archive.card_json)?;
        validate_character_card_v3(&primary_decoded)?;
        validate_charx_asset_references(&primary_decoded, &archive.assets)?;

        if let Some(character_book) = primary_decoded
            .semantic
            .get("data")
            .and_then(|data| data.get("character_book"))
        {
            archive
                .lorebooks
                .push(serde_json::to_vec(character_book).map_err(ArtifactError::Canonicalize)?);
        }
        let supplementary = archive
            .lorebooks
            .iter()
            .map(|source| {
                let decoded = decode_artifact_payload(source)?;
                validate_lorebook(&decoded)?;
                Ok(decoded)
            })
            .collect::<Result<Vec<_>, ArtifactError>>()?;
        for asset in &archive.assets {
            Store::validate_asset(&asset.bytes)?;
        }

        let assets_root = self.assets_root().to_owned();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let mut created_assets = HashSet::new();
        let result = (|| {
            let primary = insert_artifact_revision(
                &transaction,
                &archive.card_json,
                &archive.card_json,
                &primary_decoded,
                "json",
            )?;
            let supplementary_artifacts = archive
                .lorebooks
                .iter()
                .zip(&supplementary)
                .map(|(source, decoded)| {
                    insert_artifact_revision(&transaction, source, source, decoded, "json")
                })
                .collect::<Result<Vec<_>, ArtifactError>>()?;

            for asset in &archive.assets {
                let hash = ContentHash::new(Sha256::digest(&asset.bytes).into());
                if !Store::asset_file_exists(&assets_root, &hash) {
                    created_assets.insert(hash);
                }
                let record =
                    Store::put_asset_in_transaction(&transaction, &assets_root, &asset.bytes)?;
                Store::add_asset_reference_in_transaction(
                    &transaction,
                    "artifact-revision",
                    &primary.revision_hash.to_string(),
                    &record.hash,
                    &asset.logical_path,
                )?;
            }

            Ok(ArtifactBundle {
                primary,
                supplementary_artifacts,
                asset_count: archive.assets.len(),
            })
        })();

        match result {
            Ok(bundle) => {
                if let Err(error) = transaction.commit() {
                    cleanup_assets(&assets_root, &created_assets)?;
                    return Err(StorageError::Sqlite(error).into());
                }
                Ok(bundle)
            }
            Err(error) => {
                drop(transaction);
                cleanup_assets(&assets_root, &created_assets)?;
                Err(error)
            }
        }
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
        let avatar_path = match record.source_format.as_str() {
            "png" => Some("avatar.png"),
            "webp" => Some("avatar.webp"),
            _ => None,
        };
        if let Some(avatar_path) = avatar_path {
            let avatar = self
                .asset_references("artifact-revision", &record.revision_hash.to_string())?
                .into_iter()
                .find(|reference| reference.logical_path == avatar_path)
                .ok_or_else(|| ArtifactError::MissingAvatar(record.revision_hash.clone()))?;
            return self
                .asset_bytes(&avatar.asset_hash)?
                .ok_or(ArtifactError::MissingAsset(avatar.asset_hash));
        }
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

fn insert_artifact_revision(
    transaction: &Transaction<'_>,
    source: &[u8],
    payload: &[u8],
    decoded: &DecodedArtifact,
    source_format: &str,
) -> Result<ArtifactRecord, ArtifactError> {
    let revision_hash = artifact_revision_hash(decoded.kind.as_str(), source_format, source);
    let semantic_hash =
        artifact_semantic_hash(&decoded.semantic).map_err(ArtifactError::Canonicalize)?;
    let source_blob_hash = content_blob_hash(payload);
    Store::put_blob(transaction, &source_blob_hash.to_string(), payload)?;
    let event_payload = serde_json::json!({
        "revision_hash": revision_hash,
        "artifact_kind": decoded.kind,
        "source_format": source_format,
        "semantic_hash": semantic_hash,
        "source_blob_hash": source_blob_hash,
    });
    let event = append_event(transaction, None, "artifact.imported", &event_payload)?;
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
        transaction,
        "artifact-revision",
        &revision_hash.to_string(),
        &source_blob_hash.to_string(),
    )?;
    Ok(ArtifactRecord {
        revision_hash,
        kind: decoded.kind,
        source_format: source_format.to_owned(),
        semantic_hash,
        source_blob_hash,
        imported_event_id: event.event_id.to_string(),
    })
}

fn cleanup_assets(
    assets_root: &std::path::Path,
    hashes: &HashSet<ContentHash>,
) -> Result<(), ArtifactError> {
    for hash in hashes {
        Store::remove_asset_file(assets_root, hash)?;
    }
    Ok(())
}

fn validate_character_card_v3(decoded: &DecodedArtifact) -> Result<(), ArtifactError> {
    if decoded.kind != ArtifactKind::CharacterCardV3 {
        return Err(ArtifactError::CharxCardMustBeV3);
    }
    let object = decoded
        .semantic
        .as_object()
        .ok_or(ArtifactError::ExpectedObject)?;
    if object.get("spec_version").and_then(Value::as_str).is_none() {
        return Err(ArtifactError::MissingField("spec_version"));
    }
    let data = object
        .get("data")
        .and_then(Value::as_object)
        .ok_or(ArtifactError::MissingField("data"))?;
    for field in [
        "name",
        "description",
        "creator",
        "character_version",
        "mes_example",
        "system_prompt",
        "post_history_instructions",
        "first_mes",
        "personality",
        "scenario",
        "creator_notes",
    ] {
        if data.get(field).and_then(Value::as_str).is_none() {
            return Err(ArtifactError::InvalidField(field));
        }
    }
    for field in ["tags", "alternate_greetings", "group_only_greetings"] {
        if !data
            .get(field)
            .and_then(Value::as_array)
            .is_some_and(|values| values.iter().all(Value::is_string))
        {
            return Err(ArtifactError::InvalidField(field));
        }
    }
    if !data.get("extensions").is_some_and(Value::is_object) {
        return Err(ArtifactError::InvalidField("extensions"));
    }
    if let Some(assets) = data.get("assets") {
        let assets = assets
            .as_array()
            .ok_or(ArtifactError::InvalidField("assets"))?;
        for asset in assets {
            let asset = asset
                .as_object()
                .ok_or(ArtifactError::InvalidField("assets"))?;
            if ["type", "uri", "name", "ext"]
                .iter()
                .any(|field| asset.get(*field).and_then(Value::as_str).is_none())
            {
                return Err(ArtifactError::InvalidField("assets"));
            }
        }
    }
    Ok(())
}

fn validate_lorebook(decoded: &DecodedArtifact) -> Result<(), ArtifactError> {
    if decoded.kind != ArtifactKind::Lorebook {
        return Err(ArtifactError::InvalidCharxLorebook);
    }
    let entries = decoded.semantic.get("entries").or_else(|| {
        decoded
            .semantic
            .get("data")
            .and_then(|data| data.get("entries"))
    });
    if !entries.is_some_and(|entries| entries.is_array() || entries.is_object()) {
        return Err(ArtifactError::InvalidCharxLorebook);
    }
    Ok(())
}

fn validate_charx_asset_references(
    card: &DecodedArtifact,
    assets: &[container::CharxAsset],
) -> Result<(), ArtifactError> {
    let paths = assets
        .iter()
        .map(|asset| asset.logical_path.as_str())
        .collect::<HashSet<_>>();
    let Some(declarations) = card
        .semantic
        .get("data")
        .and_then(|data| data.get("assets"))
        .and_then(Value::as_array)
    else {
        return Ok(());
    };
    for declaration in declarations {
        let Some(path) = declaration
            .get("uri")
            .and_then(Value::as_str)
            .and_then(|uri| uri.strip_prefix("embeded://"))
        else {
            continue;
        };
        if container::is_media_path(path) && !paths.contains(path) {
            return Err(ArtifactError::MissingCharxAsset(path.to_owned()));
        }
    }
    Ok(())
}

pub fn decode_artifact(source: &[u8]) -> Result<DecodedArtifact, ArtifactError> {
    let decoded = decode_artifact_payload(&artifact_payload(source)?)?;
    validate_webp_card_kind(source, &decoded)?;
    Ok(decoded)
}

fn validate_webp_card_kind(source: &[u8], decoded: &DecodedArtifact) -> Result<(), ArtifactError> {
    if container::is_webp(source)
        && !matches!(
            decoded.kind,
            ArtifactKind::CharacterCardV2 | ArtifactKind::CharacterCardV3
        )
    {
        return Err(ArtifactError::WebpCardMustBeV2OrV3);
    }
    Ok(())
}

fn artifact_payload(source: &[u8]) -> Result<Cow<'_, [u8]>, ArtifactError> {
    let is_png = source.starts_with(container::PNG_SIGNATURE);
    let is_webp = container::is_webp(source);
    let limit = if is_png || is_webp {
        crate::limits::MAX_ASSET_BYTES
    } else {
        crate::limits::MAX_ARTIFACT_BYTES
    };
    if source.len() > limit {
        return Err(ArtifactError::SourceTooLarge {
            size: source.len(),
            limit,
        });
    }
    let payload = if is_png {
        Cow::Owned(container::extract_png_card(source)?)
    } else if is_webp {
        Cow::Owned(container::extract_webp_card(source)?)
    } else {
        Cow::Borrowed(source)
    };
    if payload.len() > crate::limits::MAX_ARTIFACT_BYTES {
        return Err(ArtifactError::SourceTooLarge {
            size: payload.len(),
            limit: crate::limits::MAX_ARTIFACT_BYTES,
        });
    }
    Ok(payload)
}

fn decode_artifact_payload(payload: &[u8]) -> Result<DecodedArtifact, ArtifactError> {
    let semantic = decode_unique_json(payload)?;
    let object = semantic.as_object().ok_or(ArtifactError::ExpectedObject)?;
    let kind = detect_kind(object)?;
    let greetings = match kind {
        ArtifactKind::CharacterCardV1 => greeting_values(object),
        ArtifactKind::CharacterCardV2 | ArtifactKind::CharacterCardV3 => object
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
    let kind = match object.get("spec").and_then(Value::as_str) {
        Some("chara_card_v3") => Some(ArtifactKind::CharacterCardV3),
        Some("chara_card_v2") => Some(ArtifactKind::CharacterCardV2),
        _ => None,
    };
    if let Some(kind) = kind {
        if object.get("data").and_then(Value::as_object).is_none() {
            return Err(ArtifactError::MissingField("data"));
        }
        return Ok(kind);
    }
    if object.get("spec").and_then(Value::as_str) == Some("lorebook_v3") {
        if object
            .get("data")
            .and_then(Value::as_object)
            .and_then(|data| data.get("entries"))
            .is_none()
        {
            return Err(ArtifactError::MissingField("data.entries"));
        }
        return Ok(ArtifactKind::Lorebook);
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
    #[error("PNG character metadata is not valid base64: {0}")]
    InvalidBase64PngMetadata(base64::DecodeError),
    #[error("compressed PNG character metadata could not be decoded: {0}")]
    InvalidCompressedPngMetadata(std::io::Error),
    #[error("invalid PNG artifact: {0}")]
    InvalidPng(&'static str),
    #[error("PNG does not contain character metadata")]
    MissingPngMetadata,
    #[error("PNG artifact is truncated")]
    TruncatedPng,
    #[error("WebP character metadata is not valid base64: {0}")]
    InvalidBase64WebpMetadata(base64::DecodeError),
    #[error("invalid WebP artifact: {0}")]
    InvalidWebp(&'static str),
    #[error("artifact kind '{0}' is not a Chat Completion preset")]
    ChatCompletionPresetRequired(ArtifactKind),
    #[error("preset temperature '{0}' is not a finite JSON number")]
    InvalidPresetTemperature(f64),
    #[error("invalid WebP EXIF metadata: {0}")]
    InvalidWebpExif(&'static str),
    #[error("invalid WebP XMP metadata: {0}")]
    InvalidWebpXmp(&'static str),
    #[error("WebP does not contain EXIF UserComment or XMP character metadata")]
    MissingWebpMetadata,
    #[error("WebP character metadata description is empty")]
    EmptyWebpDescription,
    #[error("WebP metadata must contain a Character Card V2 or V3 Artifact")]
    WebpCardMustBeV2OrV3,
    #[error("invalid CHARX archive: {0}")]
    InvalidCharx(zip::result::ZipError),
    #[error("failed to decompress CHARX archive: {0}")]
    ReadCharx(std::io::Error),
    #[error("CHARX archive path is unsafe: '{0}'")]
    UnsafeArchivePath(String),
    #[error("CHARX archive contains duplicate path '{0}'")]
    DuplicateArchivePath(String),
    #[error("CHARX archive entry type is unsupported: '{0}'")]
    UnsupportedArchiveEntry(String),
    #[error("encrypted CHARX archives are unsupported")]
    EncryptedCharx,
    #[error("CHARX archive exceeds {limit} byte uncompressed limit ({size} bytes)")]
    CharxTooLarge { size: u64, limit: u64 },
    #[error("CHARX archive is missing root card.json")]
    MissingCharxCard,
    #[error("CHARX card.json must contain a Character Card V3 Artifact")]
    CharxCardMustBeV3,
    #[error("CHARX lorebook JSON does not contain a Lorebook Artifact")]
    InvalidCharxLorebook,
    #[error("CHARX card references missing packaged asset '{0}'")]
    MissingCharxAsset(String),
    #[error("artifact JSON must contain an object at the root")]
    ExpectedObject,
    #[error("artifact is missing required field '{0}'")]
    MissingField(&'static str),
    #[error("artifact field '{0}' is missing or has the wrong type")]
    InvalidField(&'static str),
    #[error("JSON does not match a supported Phase 1 artifact format")]
    UnknownFormat,
    #[error("artifact source exceeds {limit} byte limit ({size} bytes)")]
    SourceTooLarge { size: usize, limit: usize },
    #[error("stored artifact kind '{0}' is unknown")]
    UnknownStoredKind(String),
    #[error("artifact revision {0} was not found")]
    NotFound(ContentHash),
    #[error("image artifact revision {0} is missing its avatar reference")]
    MissingAvatar(ContentHash),
    #[error("image artifact asset {0} is missing")]
    MissingAsset(ContentHash),
    #[error("artifact source blob {0} is missing")]
    MissingBlob(ContentHash),
    #[error("artifact canonicalization failed: {0}")]
    Canonicalize(serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::tempdir;

    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use flate2::{Compression, write::ZlibEncoder};

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

    const V2_TEXT_PAYLOAD: &[u8] = b"chara\0eyJzcGVjIjoiY2hhcmFfY2FyZF92MiIsInNwZWNfdmVyc2lvbiI6IjIuMCIsImRhdGEiOnsibmFtZSI6IlBORyBWMiIsImZpcnN0X21lcyI6IkhlbGxvIFYyIn19";
    const V2_JSON: &[u8] = br#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"iTXt V2","first_mes":"Hello iTXt"}}"#;
    const V3_JSON: &[u8] = br#"{"spec":"chara_card_v3","spec_version":"3.0","data":{"name":"PNG V3","first_mes":"Hello V3"}}"#;

    fn append_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        let mut hasher = crc32fast::Hasher::new();
        hasher.update(kind);
        hasher.update(data);
        png.extend_from_slice(&hasher.finalize().to_be_bytes());
    }

    fn png_with_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        for (kind, data) in chunks {
            append_png_chunk(&mut png, kind, data);
        }
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&[0, 0, 0, 0, 0]).unwrap();
        append_png_chunk(&mut png, b"IDAT", &encoder.finish().unwrap());
        append_png_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn png_with_chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        png_with_chunks(&[(kind, data)])
    }

    fn itxt(keyword: &[u8], compressed: bool, payload: &[u8]) -> Vec<u8> {
        let mut data = keyword.to_vec();
        data.push(0);
        data.push(u8::from(compressed));
        data.push(0);
        data.extend_from_slice(b"\0\0");
        if compressed {
            let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
            encoder.write_all(payload).unwrap();
            data.extend_from_slice(&encoder.finish().unwrap());
        } else {
            data.extend_from_slice(payload);
        }
        data
    }

    fn append_webp_chunk(webp: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        webp.extend_from_slice(kind);
        webp.extend_from_slice(&(data.len() as u32).to_le_bytes());
        webp.extend_from_slice(data);
        if data.len() % 2 == 1 {
            webp.push(0);
        }
    }

    fn webp_with_chunks(chunks: &[(&[u8; 4], &[u8])]) -> Vec<u8> {
        let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
        for (kind, data) in chunks {
            append_webp_chunk(&mut webp, kind, data);
        }
        let riff_size = (webp.len() - 8) as u32;
        webp[4..8].copy_from_slice(&riff_size.to_le_bytes());
        webp
    }

    fn exif_user_comment(payload: &[u8]) -> Vec<u8> {
        let mut comment = b"ASCII\0\0\0".to_vec();
        comment.extend_from_slice(payload);

        let mut exif = b"II\x2a\0\x08\0\0\0".to_vec();
        exif.extend_from_slice(&1_u16.to_le_bytes());
        exif.extend_from_slice(&0x8769_u16.to_le_bytes());
        exif.extend_from_slice(&4_u16.to_le_bytes());
        exif.extend_from_slice(&1_u32.to_le_bytes());
        exif.extend_from_slice(&26_u32.to_le_bytes());
        exif.extend_from_slice(&0_u32.to_le_bytes());
        exif.extend_from_slice(&1_u16.to_le_bytes());
        exif.extend_from_slice(&0x9286_u16.to_le_bytes());
        exif.extend_from_slice(&7_u16.to_le_bytes());
        exif.extend_from_slice(&(comment.len() as u32).to_le_bytes());
        exif.extend_from_slice(&44_u32.to_le_bytes());
        exif.extend_from_slice(&0_u32.to_le_bytes());
        exif.extend_from_slice(&comment);
        exif
    }

    fn charx(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (path, bytes) in entries {
            archive
                .start_file(*path, zip::write::SimpleFileOptions::default())
                .unwrap();
            archive.write_all(bytes).unwrap();
        }
        archive.finish().unwrap().into_inner()
    }

    fn ccv3_card() -> Vec<u8> {
        br#"{
            "spec":"chara_card_v3",
            "spec_version":"3.0",
            "data":{
                "name":"Archive Alice",
                "description":"A librarian.",
                "tags":[],
                "creator":"Tester",
                "character_version":"1.0",
                "mes_example":"",
                "extensions":{},
                "system_prompt":"",
                "post_history_instructions":"",
                "first_mes":"Welcome.",
                "alternate_greetings":["You came back."],
                "personality":"Curious",
                "scenario":"An old library",
                "creator_notes":"",
                "group_only_greetings":[],
                "assets":[
                    {"type":"icon","uri":"embeded://assets/icon/images/avatar.png","name":"main","ext":"png"},
                    {"type":"emotion","uri":"embeded://assets/emotion/images/happy.png","name":"happy","ext":"png"}
                ],
                "character_book":{
                    "extensions":{},
                    "entries":[
                        {"keys":["archive"],"content":"Embedded lore","extensions":{},"enabled":true,"insertion_order":1,"use_regex":false}
                    ]
                }
            }
        }"#
        .to_vec()
    }

    #[test]
    fn charx_import_preserves_card_and_links_lorebooks_and_assets() {
        // Regression coverage for CHARX bundle extraction and reference registration.
        let directory = tempdir().unwrap();
        let card = ccv3_card();
        let image = png_with_chunk(b"tEXt", b"comment\0asset");
        let lorebook = br#"{"spec":"lorebook_v3","data":{"extensions":{},"entries":[{"keys":["root"],"content":"Root lore","extensions":{},"enabled":true,"insertion_order":2,"use_regex":false}]}}"#;
        let source = charx(&[
            ("card.json", &card),
            ("lorebook.json", lorebook),
            ("assets/icon/images/avatar.png", &image),
            ("assets/emotion/images/happy.png", &image),
        ]);
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();

        let bundle = store.import_artifact_bundle(&source).unwrap();
        let references = store
            .asset_references(
                "artifact-revision",
                &bundle.primary.revision_hash.to_string(),
            )
            .unwrap();

        assert_eq!(bundle.primary.kind, ArtifactKind::CharacterCardV3);
        assert_eq!(bundle.primary.source_format, "json");
        assert_eq!(bundle.supplementary_artifacts.len(), 2);
        assert!(
            bundle
                .supplementary_artifacts
                .iter()
                .all(|artifact| artifact.kind == ArtifactKind::Lorebook)
        );
        assert_eq!(bundle.asset_count, 2);
        assert_eq!(
            store
                .export_artifact(&bundle.primary.revision_hash)
                .unwrap(),
            card
        );
        assert_eq!(
            references
                .iter()
                .map(|reference| reference.logical_path.as_str())
                .collect::<Vec<_>>(),
            [
                "assets/emotion/images/happy.png",
                "assets/icon/images/avatar.png"
            ]
        );
        assert!(references.iter().all(
            |reference| store.asset_bytes(&reference.asset_hash).unwrap() == Some(image.clone())
        ));
    }

    #[test]
    fn charx_traversal_is_rejected_without_partial_import() {
        // Regression coverage for Zip-Slip rejection before Artifact or asset persistence.
        let directory = tempdir().unwrap();
        let card = ccv3_card();
        let source = charx(&[
            ("card.json", &card),
            ("../escape.png", b"\x89PNG\r\n\x1a\n"),
        ]);
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();

        let error = store.import_artifact_bundle(&source).unwrap_err();

        assert!(matches!(error, ArtifactError::UnsafeArchivePath(_)));
        assert!(store.artifacts().unwrap().is_empty());
        assert!(!directory.path().join("escape.png").exists());
    }

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
    fn v2_card_is_extracted_from_png_text_metadata() {
        let artifact = decode_artifact(&png_with_chunk(b"tEXt", V2_TEXT_PAYLOAD)).unwrap();

        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV2);
        assert_eq!(artifact.semantic["data"]["name"], "PNG V2");
        assert_eq!(artifact.greetings, ["Hello V2"]);
    }

    #[test]
    fn v3_card_is_extracted_from_compressed_apng_itxt_metadata() {
        let animation_control = [0, 0, 0, 1, 0, 0, 0, 0];
        let frame_control = [
            0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 10, 0, 0,
        ];
        let metadata = itxt(b"ccv3", true, V3_JSON);
        let png = png_with_chunks(&[
            (b"acTL", &animation_control),
            (b"fcTL", &frame_control),
            (b"iTXt", &metadata),
        ]);

        let artifact = decode_artifact(&png).unwrap();

        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV3);
        assert_eq!(artifact.semantic["data"]["name"], "PNG V3");
        assert_eq!(artifact.greetings, ["Hello V3"]);
    }

    #[test]
    fn png_metadata_precedence_prefers_ccv3_then_itxt_then_text() {
        let chara_itxt = itxt(b"chara", false, V2_JSON);
        let ccv3_itxt = itxt(b"ccv3", false, V3_JSON);
        let png = png_with_chunks(&[
            (b"tEXt", b"chara\0not base64"),
            (b"iTXt", &chara_itxt),
            (b"iTXt", &ccv3_itxt),
        ]);

        let artifact = decode_artifact(&png).unwrap();

        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV3);
        assert_eq!(artifact.semantic["data"]["name"], "PNG V3");
    }

    #[test]
    fn corrupted_png_chunk_is_rejected() {
        let mut png = png_with_chunk(b"tEXt", V2_TEXT_PAYLOAD);
        *png.last_mut().unwrap() ^= 1;

        assert!(matches!(
            decode_artifact(&png),
            Err(ArtifactError::InvalidPng("chunk CRC mismatch"))
        ));
    }

    #[test]
    fn truncated_png_chunk_is_rejected() {
        let mut png = png_with_chunk(b"tEXt", V2_TEXT_PAYLOAD);
        png.truncate(png.len() - 2);

        assert!(matches!(
            decode_artifact(&png),
            Err(ArtifactError::TruncatedPng)
        ));
    }

    #[test]
    fn png_without_character_metadata_has_a_graceful_error() {
        let png = png_with_chunk(b"tEXt", b"comment\0avatar only");

        assert!(matches!(
            decode_artifact(&png),
            Err(ArtifactError::MissingPngMetadata)
        ));
    }

    #[test]
    fn imported_png_preserves_and_references_avatar_bytes() {
        let directory = tempdir().unwrap();
        let png = png_with_chunk(b"tEXt", V2_TEXT_PAYLOAD);
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();

        let artifact = store.import_artifact(&png).unwrap();
        let references = store
            .asset_references("artifact-revision", &artifact.revision_hash.to_string())
            .unwrap();
        let stored_source = store
            .blob(&artifact.source_blob_hash.to_string())
            .unwrap()
            .unwrap();

        assert_eq!(artifact.source_format, "png");
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].logical_path, "avatar.png");
        assert!(!stored_source.starts_with(container::PNG_SIGNATURE));
        assert_eq!(
            decode_artifact(&stored_source).unwrap().kind,
            ArtifactKind::CharacterCardV2
        );
        assert_eq!(
            store.asset_bytes(&references[0].asset_hash).unwrap(),
            Some(png.clone())
        );
        assert_eq!(store.export_artifact(&artifact.revision_hash).unwrap(), png);
    }

    #[test]
    fn webp_exif_user_comment_precedes_xmp_description() {
        let exif_payload = STANDARD.encode(V2_JSON);
        let exif = exif_user_comment(exif_payload.as_bytes());
        let xmp = format!(
            "<x:xmpmeta><rdf:RDF><rdf:Description><dc:description>{}</dc:description></rdf:Description></rdf:RDF></x:xmpmeta>",
            String::from_utf8_lossy(V3_JSON)
        );
        let webp = webp_with_chunks(&[(b"XMP ", xmp.as_bytes()), (b"EXIF", &exif)]);

        let artifact = decode_artifact(&webp).unwrap();

        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV2);
        assert_eq!(artifact.semantic["data"]["name"], "iTXt V2");
    }

    #[test]
    fn webp_xmp_descriptions_accept_raw_json() {
        let xmp_description = format!(
            "<x:xmpmeta><rdf:RDF><rdf:Description><xmp:description><!-- </xmp:description> --><![CDATA[{}]]></xmp:description></rdf:Description></rdf:RDF></x:xmpmeta>",
            String::from_utf8_lossy(V3_JSON)
        );
        let xmp_webp = webp_with_chunks(&[(b"XMP ", xmp_description.as_bytes())]);
        let dc_webp = include_bytes!("../tests/fixtures/artifacts/card-v3-xmp.webp").as_slice();

        let xmp_artifact = decode_artifact(&xmp_webp).unwrap();
        let dc_artifact = decode_artifact(dc_webp).unwrap();

        assert_eq!(xmp_artifact.kind, ArtifactKind::CharacterCardV3);
        assert_eq!(xmp_artifact.semantic["data"]["name"], "PNG V3");
        assert_eq!(dc_artifact.kind, ArtifactKind::CharacterCardV3);
        assert_eq!(dc_artifact.semantic["data"]["name"], "PNG V3");
    }

    #[test]
    fn imported_webp_preserves_and_references_avatar_bytes() {
        let directory = tempdir().unwrap();
        let webp = include_bytes!("../tests/fixtures/artifacts/card-v2-exif.webp").as_slice();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();

        let artifact = store.import_artifact(webp).unwrap();
        let references = store
            .asset_references("artifact-revision", &artifact.revision_hash.to_string())
            .unwrap();

        assert_eq!(artifact.kind, ArtifactKind::CharacterCardV2);
        assert_eq!(artifact.source_format, "webp");
        assert_eq!(references.len(), 1);
        assert_eq!(references[0].logical_path, "avatar.webp");
        assert_eq!(
            store.asset_bytes(&references[0].asset_hash).unwrap(),
            Some(webp.to_vec())
        );
        assert_eq!(
            store.export_artifact(&artifact.revision_hash).unwrap(),
            webp
        );
    }

    #[test]
    fn webp_reports_malformed_cardless_empty_and_unsupported_metadata() {
        let mut malformed = webp_with_chunks(&[(b"EXIF", b"metadata")]);
        malformed.truncate(malformed.len() - 1);
        let empty_exif_ifd = b"II\x2a\0\x08\0\0\0\0\0\0\0\0\0";
        let unrelated_xmp = b"<x:xmpmeta><dc:title>avatar</dc:title></x:xmpmeta>";
        let cardless = webp_with_chunks(&[(b"EXIF", empty_exif_ifd), (b"XMP ", unrelated_xmp)]);
        let empty_xmp = webp_with_chunks(&[(
            b"XMP ",
            b"<x:xmpmeta><dc:description> </dc:description></x:xmpmeta>",
        )]);
        let unsupported_xmp = [
            b"<x:xmpmeta><dc:description>".as_slice(),
            br#"{"name":"V1","description":"","personality":"","scenario":"","first_mes":"","mes_example":""}"#,
            b"</dc:description></x:xmpmeta>".as_slice(),
        ]
        .concat();
        let unsupported = webp_with_chunks(&[(b"XMP ", &unsupported_xmp)]);

        assert!(matches!(
            decode_artifact(&malformed),
            Err(ArtifactError::InvalidWebp(_))
        ));
        assert!(matches!(
            decode_artifact(&cardless),
            Err(ArtifactError::MissingWebpMetadata)
        ));
        assert!(matches!(
            decode_artifact(&empty_xmp),
            Err(ArtifactError::EmptyWebpDescription)
        ));
        assert!(matches!(
            decode_artifact(&unsupported),
            Err(ArtifactError::WebpCardMustBeV2OrV3)
        ));
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
    fn clone_and_patch_preset_changes_only_tunable_fields_and_revision_hash() {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let source = serde_json::json!({
            "preset_name": "Source",
            "temperature": 0.7,
            "max_context": 8192,
            "openai_max_tokens": 512,
            "use_sysprompt": true,
            "prompts": [{"identifier": "main", "role": "system", "content": "Stay in character."}],
            "prompt_order": [{"character_id": 100001, "order": [
                {"identifier": "main", "enabled": true}
            ]}],
            "extensions": {"regex_scripts": [{
                "id": "cleanup",
                "findRegex": "/secret/g",
                "replaceString": "[redacted]"
            }]}
        });
        let source_bytes = serde_json::to_vec(&source).unwrap();
        let source_record = store.import_artifact(&source_bytes).unwrap();

        let clone_bytes = clone_and_patch_preset(
            &source_bytes,
            PresetPatch {
                preset_name: "Source-copy".to_owned(),
                temperature: 0.9,
                max_context: 16_384,
                max_tokens: 1_024,
                use_sysprompt: false,
            },
        )
        .unwrap();
        let clone = decode_artifact(&clone_bytes).unwrap().semantic;
        let clone_record = store.import_artifact(&clone_bytes).unwrap();

        assert_eq!(clone["preset_name"], "Source-copy");
        assert_eq!(clone["temperature"], 0.9);
        assert_eq!(clone["max_context"], 16_384);
        assert_eq!(clone["openai_max_tokens"], 1_024);
        assert_eq!(clone["openai_max_context"], 16_384);
        assert_eq!(clone["use_sysprompt"], false);
        assert_eq!(clone["prompts"], source["prompts"]);
        assert_eq!(clone["prompt_order"], source["prompt_order"]);
        assert_eq!(clone["extensions"], source["extensions"]);
        assert_ne!(clone_record.revision_hash, source_record.revision_hash);
        assert_ne!(clone_record.semantic_hash, source_record.semantic_hash);
        let source_script =
            crate::transform_preset_content("", &source_record.revision_hash, &source, &[])
                .scripts
                .remove(0);
        let clone_script =
            crate::transform_preset_content("", &clone_record.revision_hash, &clone, &[])
                .scripts
                .remove(0);
        assert_eq!(clone_script.digest, source_script.digest);
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
