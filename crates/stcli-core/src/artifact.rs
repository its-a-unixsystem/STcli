use std::{
    collections::{HashMap, HashSet},
    fmt,
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use rusqlite::{OptionalExtension, Transaction, params};
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ArtifactCodecAsset, ArtifactCodecBundle, ArtifactCodecError, ArtifactCodecProvenance,
    ArtifactCodecSupplementary, ArtifactCodecSupplementaryProvenance, ContentHash, Store,
    artifact_codec::{
        MAX_CODEC_ASSET_BYTES, MAX_CODEC_ASSETS, MAX_CODEC_LOGICAL_PATH_BYTES,
        MAX_CODEC_PAYLOAD_BYTES, MAX_CODEC_SUPPLEMENTARY_ARTIFACTS, MAX_CODEC_TOTAL_ASSET_BYTES,
        MAX_CODEC_TOTAL_SUPPLEMENTARY_BYTES,
    },
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
    pub reasoning_effort: Option<String>,
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
    if let Some(reasoning_effort) = patch.reasoning_effort {
        object.insert(
            "reasoning_effort".to_owned(),
            Value::String(reasoning_effort),
        );
    } else {
        object.remove("reasoning_effort");
    }
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

impl Store {
    pub fn import_artifact(&mut self, source: &[u8]) -> Result<ArtifactRecord, ArtifactError> {
        self.import_single_artifact(source)
    }

    pub fn import_artifact_bundle(
        &mut self,
        source: &[u8],
    ) -> Result<ArtifactBundle, ArtifactError> {
        Ok(ArtifactBundle {
            primary: self.import_single_artifact(source)?,
            supplementary_artifacts: Vec::new(),
            asset_count: 0,
        })
    }

    fn import_single_artifact(&mut self, source: &[u8]) -> Result<ArtifactRecord, ArtifactError> {
        let decoded = decode_artifact(source)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let record = insert_artifact_revision(&transaction, source, source, &decoded, "json")?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(record)
    }

    pub(crate) fn restore_artifact_revision(
        &mut self,
        record: &ArtifactRecord,
        payload: &[u8],
    ) -> Result<ArtifactRecord, ArtifactError> {
        if payload.len() > crate::limits::MAX_ARTIFACT_BYTES {
            return Err(ArtifactError::SourceTooLarge {
                size: payload.len(),
                limit: crate::limits::MAX_ARTIFACT_BYTES,
            });
        }
        let decoded = decode_artifact_payload_as(payload, record.kind)?;
        if artifact_semantic_hash(&decoded.semantic).map_err(ArtifactError::Canonicalize)?
            != record.semantic_hash
        {
            return Err(ArtifactError::RestoredRecordMismatch("semantic hash"));
        }
        if content_blob_hash(payload) != record.source_blob_hash {
            return Err(ArtifactError::RestoredRecordMismatch("source blob hash"));
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let restored = insert_artifact_revision_with_hash(
            &transaction,
            record.revision_hash.clone(),
            payload,
            &decoded,
            &record.source_format,
        )?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(restored)
    }

    pub fn import_artifact_from_codec(
        &mut self,
        source: &[u8],
        bundle: &ArtifactCodecBundle,
        provenance: &ArtifactCodecProvenance,
    ) -> Result<ArtifactBundle, ArtifactCodecError> {
        validate_codec_format(&bundle.source_format)?;
        validate_codec_format(&provenance.format)?;
        let expected_source_format = match provenance.format.as_str() {
            "json" | "charx" => Some("json"),
            "png" | "apng" => Some("png"),
            "webp" => Some("webp"),
            _ => None,
        };
        if expected_source_format.is_some_and(|expected| bundle.source_format != expected) {
            return Err(ArtifactCodecError::InvalidFormat(
                bundle.source_format.clone(),
            ));
        }
        if bundle.assets.len() > MAX_CODEC_ASSETS {
            return Err(ArtifactCodecError::AssetCount {
                actual: bundle.assets.len(),
                limit: MAX_CODEC_ASSETS,
            });
        }
        if bundle.supplementary_artifacts.len() > MAX_CODEC_SUPPLEMENTARY_ARTIFACTS {
            return Err(ArtifactCodecError::SupplementaryArtifactCount {
                actual: bundle.supplementary_artifacts.len(),
                limit: MAX_CODEC_SUPPLEMENTARY_ARTIFACTS,
            });
        }
        let payload =
            BASE64
                .decode(&bundle.payload)
                .map_err(|source| ArtifactCodecError::InvalidBase64 {
                    field: "payload",
                    source,
                })?;
        if payload.len() > MAX_CODEC_PAYLOAD_BYTES {
            return Err(ArtifactCodecError::PayloadSize {
                actual: payload.len(),
                limit: MAX_CODEC_PAYLOAD_BYTES,
            });
        }
        if ContentHash::new(Sha256::digest(&payload).into()) != bundle.payload_sha256 {
            return Err(ArtifactCodecError::HashMismatch { field: "payload" });
        }
        let decoded = decode_artifact_payload(&payload)?;
        if decoded.kind != bundle.artifact_kind {
            return Err(ArtifactCodecError::ArtifactKindMismatch {
                proposed: bundle.artifact_kind,
                actual: decoded.kind,
            });
        }
        if provenance.format == "charx" {
            validate_character_card_v3(&decoded)?;
        }

        let mut paths = HashSet::with_capacity(bundle.assets.len());
        let mut total_asset_bytes = 0usize;
        let mut decoded_assets = Vec::with_capacity(bundle.assets.len());
        for asset in &bundle.assets {
            validate_codec_logical_path(&asset.logical_path)?;
            if !paths.insert(asset.logical_path.as_str()) {
                return Err(ArtifactCodecError::DuplicateLogicalPath(
                    asset.logical_path.clone(),
                ));
            }
            let bytes = BASE64.decode(&asset.bytes).map_err(|source| {
                ArtifactCodecError::InvalidBase64 {
                    field: "asset",
                    source,
                }
            })?;
            if bytes.len() != asset.byte_size {
                return Err(ArtifactCodecError::ByteSizeMismatch {
                    path: asset.logical_path.clone(),
                    proposed: asset.byte_size,
                    actual: bytes.len(),
                });
            }
            if bytes.len() > MAX_CODEC_ASSET_BYTES {
                return Err(ArtifactCodecError::AssetSize {
                    path: asset.logical_path.clone(),
                    actual: bytes.len(),
                    limit: MAX_CODEC_ASSET_BYTES,
                });
            }
            total_asset_bytes = total_asset_bytes.checked_add(bytes.len()).ok_or(
                ArtifactCodecError::TotalAssetSize {
                    actual: usize::MAX,
                    limit: MAX_CODEC_TOTAL_ASSET_BYTES,
                },
            )?;
            if total_asset_bytes > MAX_CODEC_TOTAL_ASSET_BYTES {
                return Err(ArtifactCodecError::TotalAssetSize {
                    actual: total_asset_bytes,
                    limit: MAX_CODEC_TOTAL_ASSET_BYTES,
                });
            }
            if ContentHash::new(Sha256::digest(&bytes).into()) != asset.sha256 {
                return Err(ArtifactCodecError::HashMismatch { field: "asset" });
            }
            Store::validate_asset(&bytes).map_err(ArtifactError::Storage)?;
            decoded_assets.push((asset.logical_path.clone(), bytes));
        }
        if provenance.format == "charx" {
            validate_embedded_asset_references(&decoded, &paths)?;
        }
        let mut supplementary_paths = HashSet::with_capacity(bundle.supplementary_artifacts.len());
        let mut decoded_supplementary = Vec::with_capacity(bundle.supplementary_artifacts.len());
        let mut total_supplementary_bytes = 0usize;
        for supplementary in &bundle.supplementary_artifacts {
            validate_codec_logical_path(&supplementary.logical_path)?;
            if !supplementary_paths.insert(supplementary.logical_path.as_str())
                || paths.contains(supplementary.logical_path.as_str())
            {
                return Err(ArtifactCodecError::DuplicateLogicalPath(
                    supplementary.logical_path.clone(),
                ));
            }
            if supplementary.source_format != "json" {
                return Err(ArtifactCodecError::InvalidFormat(
                    supplementary.source_format.clone(),
                ));
            }
            let bytes = BASE64.decode(&supplementary.payload).map_err(|source| {
                ArtifactCodecError::InvalidBase64 {
                    field: "supplementary artifact",
                    source,
                }
            })?;
            if bytes.len() > MAX_CODEC_PAYLOAD_BYTES {
                return Err(ArtifactCodecError::PayloadSize {
                    actual: bytes.len(),
                    limit: MAX_CODEC_PAYLOAD_BYTES,
                });
            }
            if bytes.len() != supplementary.byte_size {
                return Err(ArtifactCodecError::ByteSizeMismatch {
                    path: supplementary.logical_path.clone(),
                    proposed: supplementary.byte_size,
                    actual: bytes.len(),
                });
            }
            if ContentHash::new(Sha256::digest(&bytes).into()) != supplementary.payload_sha256 {
                return Err(ArtifactCodecError::HashMismatch {
                    field: "supplementary artifact",
                });
            }
            total_supplementary_bytes = total_supplementary_bytes.checked_add(bytes.len()).ok_or(
                ArtifactCodecError::TotalSupplementarySize {
                    actual: usize::MAX,
                    limit: MAX_CODEC_TOTAL_SUPPLEMENTARY_BYTES,
                },
            )?;
            if total_supplementary_bytes > MAX_CODEC_TOTAL_SUPPLEMENTARY_BYTES {
                return Err(ArtifactCodecError::TotalSupplementarySize {
                    actual: total_supplementary_bytes,
                    limit: MAX_CODEC_TOTAL_SUPPLEMENTARY_BYTES,
                });
            }
            let decoded = decode_artifact_payload(&bytes)?;
            if decoded.kind != supplementary.artifact_kind {
                return Err(ArtifactCodecError::ArtifactKindMismatch {
                    proposed: supplementary.artifact_kind,
                    actual: decoded.kind,
                });
            }
            if provenance.format == "charx" {
                validate_lorebook(&decoded)?;
            }
            decoded_supplementary.push((
                supplementary.logical_path.clone(),
                supplementary.source_format.clone(),
                bytes,
                decoded,
            ));
        }
        if provenance.format == "charx" {
            let embedded_payload = decoded
                .semantic
                .get("data")
                .and_then(|data| data.get("character_book"))
                .map(serde_json::to_vec)
                .transpose()
                .map_err(ArtifactError::Canonicalize)?;
            let claimed = bundle
                .supplementary_artifacts
                .iter()
                .enumerate()
                .filter(|(_, supplementary)| supplementary.embedded)
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            match (embedded_payload, claimed.as_slice()) {
                (Some(embedded), [index]) if decoded_supplementary[*index].2 == embedded => {}
                (None, []) => {}
                _ => return Err(ArtifactCodecError::InvalidEmbeddedSupplementary),
            }
        }
        let assets_root = self.assets_root().to_owned();
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
            .map_err(StorageError::Sqlite)
            .map_err(ArtifactError::Storage)?;
        let revision_source = if provenance.format == "charx" {
            payload.as_slice()
        } else {
            source
        };
        let revision_hash = artifact_revision_hash(
            decoded.kind.as_str(),
            &bundle.source_format,
            revision_source,
        );
        let revision_exists = transaction
            .query_row(
                "SELECT 1 FROM artifact_revisions WHERE revision_hash = ?1",
                [revision_hash.to_string()],
                |_| Ok(()),
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(ArtifactError::Storage)?
            .is_some();
        if revision_exists {
            return Err(ArtifactCodecError::RevisionConflict(revision_hash));
        }
        let mut created_assets = HashSet::new();
        let result: Result<ArtifactBundle, ArtifactError> = (|| {
            let primary = insert_artifact_revision(
                &transaction,
                revision_source,
                &payload,
                &decoded,
                &bundle.source_format,
            )?;
            let supplementary_artifacts = decoded_supplementary
                .iter()
                .map(|(_, source_format, bytes, decoded)| {
                    insert_artifact_revision(&transaction, bytes, bytes, decoded, source_format)
                })
                .collect::<Result<Vec<_>, ArtifactError>>()?;
            for (logical_path, bytes) in &decoded_assets {
                let hash = ContentHash::new(Sha256::digest(bytes).into());
                if !Store::asset_file_exists(&assets_root, &hash) {
                    created_assets.insert(hash);
                }
                let record = Store::put_asset_in_transaction(&transaction, &assets_root, bytes)?;
                Store::add_asset_reference_in_transaction(
                    &transaction,
                    "artifact-revision",
                    &primary.revision_hash.to_string(),
                    &record.hash,
                    logical_path,
                )?;
            }
            let mut stored_provenance = provenance.clone();
            stored_provenance.supplementary_artifacts = decoded_supplementary
                .iter()
                .zip(&supplementary_artifacts)
                .zip(&bundle.supplementary_artifacts)
                .map(|(((logical_path, _, _, _), record), proposed)| {
                    ArtifactCodecSupplementaryProvenance {
                        logical_path: logical_path.clone(),
                        embedded: proposed.embedded,
                        revision_hash: record.revision_hash.clone(),
                    }
                })
                .collect();
            let provenance_body =
                serde_json::to_vec(&stored_provenance).map_err(ArtifactError::Canonicalize)?;
            transaction
                .execute(
                    "INSERT OR IGNORE INTO artifact_codec_provenance(revision_hash, body) VALUES (?1, ?2)",
                    params![primary.revision_hash.to_string(), provenance_body],
                )
                .map_err(StorageError::Sqlite)?;
            append_event(
                &transaction,
                None,
                "artifact.codec-imported",
                &serde_json::json!({
                    "revision_hash": primary.revision_hash,
                    "provenance": stored_provenance,
                }),
            )?;
            Ok(ArtifactBundle {
                primary,
                supplementary_artifacts,
                asset_count: decoded_assets.len(),
            })
        })();

        match result {
            Ok(bundle) => {
                if let Err(error) = transaction.commit() {
                    cleanup_assets(&assets_root, &created_assets)?;
                    return Err(ArtifactError::Storage(StorageError::Sqlite(error)).into());
                }
                Ok(bundle)
            }
            Err(error) => {
                drop(transaction);
                cleanup_assets(&assets_root, &created_assets)?;
                Err(error.into())
            }
        }
    }

    pub fn artifact_codec_provenance(
        &self,
        revision_hash: &ContentHash,
    ) -> Result<Option<ArtifactCodecProvenance>, ArtifactError> {
        let body = self
            .connection
            .query_row(
                "SELECT body FROM artifact_codec_provenance WHERE revision_hash = ?1",
                [revision_hash.to_string()],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()
            .map_err(StorageError::Sqlite)?;
        body.map(|body| serde_json::from_slice(&body).map_err(StorageError::Json))
            .transpose()
            .map_err(ArtifactError::Storage)
    }
    pub fn artifact_codec_plugin_in_use(&self, id: &str) -> Result<bool, ArtifactError> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM artifact_codec_provenance")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StorageError::Sqlite)?;
        for row in rows {
            let provenance = serde_json::from_slice::<ArtifactCodecProvenance>(
                &row.map_err(StorageError::Sqlite)?,
            )
            .map_err(StorageError::Json)?;
            if provenance.plugin_id == id {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(crate) fn artifact_codec_bundle(
        &self,
        revision_hash: &ContentHash,
        provenance: &ArtifactCodecProvenance,
    ) -> Result<ArtifactCodecBundle, ArtifactCodecError> {
        let artifact = self
            .artifact(revision_hash)?
            .ok_or_else(|| ArtifactError::NotFound(revision_hash.clone()))?;
        let payload = self.stored_artifact_payload(revision_hash)?;
        let assets = self
            .asset_references("artifact-revision", &revision_hash.to_string())
            .map_err(ArtifactError::Storage)?
            .into_iter()
            .map(|reference| {
                let bytes = self
                    .asset_bytes(&reference.asset_hash)?
                    .ok_or_else(|| StorageError::MissingAsset(reference.asset_hash.clone()))?;
                Ok(ArtifactCodecAsset {
                    logical_path: reference.logical_path,
                    byte_size: bytes.len(),
                    sha256: ContentHash::new(Sha256::digest(&bytes).into()),
                    bytes: BASE64.encode(bytes),
                })
            })
            .collect::<Result<Vec<_>, StorageError>>()
            .map_err(ArtifactError::Storage)?;
        let supplementary_artifacts = provenance
            .supplementary_artifacts
            .iter()
            .map(|supplementary| {
                let artifact = self
                    .artifact(&supplementary.revision_hash)?
                    .ok_or_else(|| ArtifactError::NotFound(supplementary.revision_hash.clone()))?;
                let payload = self.stored_artifact_payload(&supplementary.revision_hash)?;
                Ok(ArtifactCodecSupplementary {
                    logical_path: supplementary.logical_path.clone(),
                    embedded: supplementary.embedded,
                    artifact_kind: artifact.kind,
                    source_format: artifact.source_format,
                    byte_size: payload.len(),
                    payload_sha256: ContentHash::new(Sha256::digest(&payload).into()),
                    payload: BASE64.encode(payload),
                })
            })
            .collect::<Result<Vec<_>, ArtifactError>>()?;
        Ok(ArtifactCodecBundle {
            artifact_kind: artifact.kind,
            source_format: artifact.source_format,
            payload_sha256: ContentHash::new(Sha256::digest(&payload).into()),
            payload: BASE64.encode(payload),
            assets,
            supplementary_artifacts,
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

    pub(crate) fn stored_artifact_payload(
        &self,
        revision_hash: &ContentHash,
    ) -> Result<Vec<u8>, ArtifactError> {
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
        let record = self
            .artifact(revision_hash)?
            .ok_or_else(|| ArtifactError::NotFound(revision_hash.clone()))?;
        let payload = self.stored_artifact_payload(revision_hash)?;
        decode_artifact_payload_as(&payload, record.kind)
    }
    /// Create an immutable preset revision with Prompt Order Entry enabled flags changed.
    pub fn patch_prompt_order(
        &mut self,
        revision_hash: &ContentHash,
        character_id: Option<u64>,
        changes: &std::collections::BTreeMap<String, bool>,
    ) -> Result<ArtifactRecord, ArtifactError> {
        let original = self
            .artifact(revision_hash)?
            .ok_or_else(|| ArtifactError::NotFound(revision_hash.clone()))?;
        let source = self.stored_artifact_payload(revision_hash)?;
        let mut decoded = decode_artifact_payload_as(&source, original.kind)?;
        if decoded.kind != ArtifactKind::ChatCompletionPreset {
            return Err(ArtifactError::ChatCompletionPresetRequired(decoded.kind));
        }
        let object = decoded
            .semantic
            .as_object_mut()
            .ok_or(ArtifactError::ExpectedObject)?;
        let prompt_order = object
            .get_mut("prompt_order")
            .ok_or(ArtifactError::MissingField("prompt_order"))?;
        let order = if let Some(profiles) = prompt_order.as_array_mut() {
            let target = character_id.unwrap_or(crate::CHAT_COMPLETION_CHARACTER_ID);
            let profile_index = profiles
                .iter()
                .position(|profile| {
                    profile
                        .get("character_id")
                        .and_then(Value::as_u64)
                        .is_some_and(|id| id == target)
                })
                .or_else(|| {
                    profiles
                        .iter()
                        .position(|profile| profile.get("order").is_some())
                });
            if let Some(index) = profile_index {
                profiles
                    .get_mut(index)
                    .and_then(|profile| profile.get_mut("order"))
            } else {
                Some(prompt_order)
            }
        } else {
            prompt_order.get_mut("order")
        };
        let Some(Value::Array(order)) = order else {
            return Err(ArtifactError::InvalidField("prompt_order"));
        };
        let mut changed = false;
        for entry in order {
            let Some(identifier) = entry.get("identifier").and_then(Value::as_str) else {
                continue;
            };
            if let Some(enabled) = changes.get(identifier) {
                if entry.get("enabled").and_then(Value::as_bool) != Some(*enabled) {
                    changed = true;
                }
                if let Some(entry) = entry.as_object_mut() {
                    entry.insert("enabled".to_owned(), Value::Bool(*enabled));
                }
            }
        }
        if !changed {
            return Ok(original);
        }
        let patched =
            serde_json::to_vec_pretty(&decoded.semantic).map_err(ArtifactError::Canonicalize)?;
        self.import_artifact(&patched)
    }
}

fn insert_artifact_revision(
    transaction: &Transaction<'_>,
    source: &[u8],
    payload: &[u8],
    decoded: &DecodedArtifact,
    source_format: &str,
) -> Result<ArtifactRecord, ArtifactError> {
    insert_artifact_revision_with_hash(
        transaction,
        artifact_revision_hash(decoded.kind.as_str(), source_format, source),
        payload,
        decoded,
        source_format,
    )
}

fn insert_artifact_revision_with_hash(
    transaction: &Transaction<'_>,
    revision_hash: ContentHash,
    payload: &[u8],
    decoded: &DecodedArtifact,
    source_format: &str,
) -> Result<ArtifactRecord, ArtifactError> {
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
        return Err(ArtifactError::CharacterCardV3Required);
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
        return Err(ArtifactError::LorebookRequired);
    }
    let entries = decoded.semantic.get("entries").or_else(|| {
        decoded
            .semantic
            .get("data")
            .and_then(|data| data.get("entries"))
    });
    if !entries.is_some_and(|entries| entries.is_array() || entries.is_object()) {
        return Err(ArtifactError::LorebookRequired);
    }
    Ok(())
}

fn is_media_path(path: &str) -> bool {
    path.starts_with("assets/")
        && matches!(
            path.rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase())
                .as_deref(),
            Some(
                "png"
                    | "jpg"
                    | "jpeg"
                    | "gif"
                    | "webp"
                    | "avif"
                    | "mp3"
                    | "ogg"
                    | "wav"
                    | "flac"
                    | "m4a"
                    | "mp4"
                    | "webm"
            )
        )
}

fn validate_embedded_asset_references(
    card: &DecodedArtifact,
    paths: &HashSet<&str>,
) -> Result<(), ArtifactError> {
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
        if is_media_path(path) && !paths.contains(path) {
            return Err(ArtifactError::MissingDeclaredAsset(path.to_owned()));
        }
    }
    Ok(())
}

fn validate_codec_format(format: &str) -> Result<(), ArtifactCodecError> {
    if format.is_empty()
        || format.len() > 32
        || !format
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(ArtifactCodecError::InvalidFormat(format.to_owned()));
    }
    Ok(())
}

fn validate_codec_logical_path(path: &str) -> Result<(), ArtifactCodecError> {
    if path.is_empty()
        || path.len() > MAX_CODEC_LOGICAL_PATH_BYTES
        || path.starts_with('/')
        || path.contains('\\')
        || path
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || path.chars().any(char::is_control)
    {
        return Err(ArtifactCodecError::InvalidLogicalPath(path.to_owned()));
    }
    Ok(())
}

pub fn decode_artifact(source: &[u8]) -> Result<DecodedArtifact, ArtifactError> {
    if source.len() > crate::limits::MAX_ARTIFACT_BYTES {
        return Err(ArtifactError::SourceTooLarge {
            size: source.len(),
            limit: crate::limits::MAX_ARTIFACT_BYTES,
        });
    }
    decode_artifact_payload(source)
}

fn decode_artifact_payload_as(
    payload: &[u8],
    kind: ArtifactKind,
) -> Result<DecodedArtifact, ArtifactError> {
    let semantic = decode_unique_json(payload)?;
    let object = semantic.as_object().ok_or(ArtifactError::ExpectedObject)?;
    if detect_kind(object)? != kind {
        return Err(ArtifactError::StoredKindMismatch(kind));
    }
    let greetings = greetings_for_kind(object, kind);
    Ok(DecodedArtifact {
        kind,
        semantic,
        greetings,
    })
}

fn decode_artifact_payload(payload: &[u8]) -> Result<DecodedArtifact, ArtifactError> {
    let semantic = decode_unique_json(payload)?;
    let object = semantic.as_object().ok_or(ArtifactError::ExpectedObject)?;
    let kind = detect_kind(object)?;
    let greetings = greetings_for_kind(object, kind);
    Ok(DecodedArtifact {
        kind,
        semantic,
        greetings,
    })
}

fn greetings_for_kind(object: &Map<String, Value>, kind: ArtifactKind) -> Vec<String> {
    match kind {
        ArtifactKind::CharacterCardV1 => greeting_values(object),
        ArtifactKind::CharacterCardV2 | ArtifactKind::CharacterCardV3 => object
            .get("data")
            .and_then(Value::as_object)
            .map(greeting_values)
            .unwrap_or_default(),
        ArtifactKind::Lorebook | ArtifactKind::ChatCompletionPreset => Vec::new(),
    }
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
    #[error("artifact kind '{0}' is not a Chat Completion preset")]
    ChatCompletionPresetRequired(ArtifactKind),
    #[error("preset temperature '{0}' is not a finite JSON number")]
    InvalidPresetTemperature(f64),
    #[error("Artifact payload must contain a Character Card V3")]
    CharacterCardV3Required,
    #[error("Artifact payload must contain a Lorebook")]
    LorebookRequired,
    #[error("Artifact payload references missing bundled asset '{0}'")]
    MissingDeclaredAsset(String),
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
    #[error("stored Artifact kind '{0}' does not match its payload")]
    StoredKindMismatch(ArtifactKind),
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
    #[error("restored Artifact record has a mismatched {0}")]
    RestoredRecordMismatch(&'static str),
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
                reasoning_effort: Some("high".to_owned()),
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
        assert_eq!(clone["reasoning_effort"], "high");
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
