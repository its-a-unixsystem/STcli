use rusqlite::{OptionalExtension, params};
use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ArtifactError, ArtifactRecord, AttemptEffectReceipt, AttemptProjection, AttemptStatus,
    BranchProjection, CandidateOrigin, CandidateProjection, ContentHash, EntityId, PromptPlan,
    ProviderError, SessionConfigurationRecord, SessionError, SessionProjection, StateCell,
    StateError, Store, TurnError, TurnProjection, artifact_revision_hash, artifact_semantic_hash,
    canonical_json, canonical_json_hash, content_blob_hash, decode_artifact, provider_request_hash,
    session_projection_hash, storage::append_event, validate_recorded_receipt,
};

const CAPSULE_DOMAIN: &str = "stcli:turn-capsule:v1";
const FEATURE_MANIFEST_DOMAIN: &str = "stcli:feature-manifest:v1";
const CAPSULE_FORMAT: &str = "stcli_turn_capsule";
const CAPSULE_VERSION: &str = "1.0";
const BUILT_IN_PROFILE: &str = include_str!("../../../compat/profiles/sillytavern-1.18-core.json");

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CapsuleKind {
    Portable,
    Thin,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleEngine {
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleCompatibility {
    pub profile: String,
    pub feature_manifest_digest: ContentHash,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleIdentity {
    pub session_id: EntityId,
    pub branch_id: EntityId,
    pub turn_id: EntityId,
    pub attempt_id: EntityId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleArtifact {
    pub record: ArtifactRecord,
    pub source: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleReference {
    pub owner_kind: String,
    pub owner_id: String,
    pub blob_hash: ContentHash,
    pub embedded: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RedactionEntry {
    pub path: String,
    pub reason: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleCapabilities {
    pub inspect: bool,
    pub replay: bool,
    pub rerun: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CapsuleBaseline {
    pub session: SessionProjection,
    pub branch: BranchProjection,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapsuleProvider {
    pub request_hash: Option<ContentHash>,
    pub receipt: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionProjectionSnapshot {
    pub session: SessionProjection,
    pub branch: BranchProjection,
    pub turn: TurnProjection,
    pub attempt: AttemptProjection,
    pub candidate: Option<CandidateProjection>,
    pub state: Vec<StateCell>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CapsuleResult {
    pub projection: Option<SessionProjectionSnapshot>,
    pub projection_hash: Option<ContentHash>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TurnCapsule {
    pub format: String,
    pub version: String,
    pub kind: CapsuleKind,
    pub engine: CapsuleEngine,
    pub compatibility: CapsuleCompatibility,
    pub identity: CapsuleIdentity,
    pub artifacts: Vec<CapsuleArtifact>,
    pub configuration: Option<SessionConfigurationRecord>,
    pub baseline: Option<CapsuleBaseline>,
    pub effects: Option<AttemptEffectReceipt>,
    pub prompt: Option<PromptPlan>,
    pub provider: CapsuleProvider,
    pub result: CapsuleResult,
    pub references: Vec<CapsuleReference>,
    pub redactions: Vec<RedactionEntry>,
    pub capabilities: CapsuleCapabilities,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ReplayReport {
    pub capsule_hash: ContentHash,
    pub projection_hash: ContentHash,
    pub provider_calls: usize,
    pub plugin_executions: usize,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ImportedCapsule {
    pub capsule_hash: ContentHash,
    pub session_id: EntityId,
    pub branch_id: EntityId,
    pub turn_id: EntityId,
    pub attempt_id: EntityId,
    pub candidate_id: Option<EntityId>,
}

impl Store {
    pub fn export_turn_capsule(
        &self,
        attempt_id: EntityId,
        kind: CapsuleKind,
        redact_content: bool,
    ) -> Result<TurnCapsule, CapsuleError> {
        let attempt = self
            .attempt(attempt_id)?
            .ok_or(CapsuleError::AttemptNotFound(attempt_id))?;
        let turn = self
            .turn(attempt.turn_id)?
            .ok_or(CapsuleError::TurnNotFound(attempt.turn_id))?;
        let session = self
            .session(turn.session_id)?
            .ok_or(CapsuleError::SessionNotFound(turn.session_id))?;
        let branch = self
            .branch(turn.branch_id)?
            .ok_or(CapsuleError::BranchNotFound(turn.branch_id))?;
        let configuration = self
            .configuration(&attempt.config_hash)?
            .ok_or_else(|| CapsuleError::ConfigurationNotFound(attempt.config_hash.clone()))?;
        let candidate = self
            .candidates_for_turn(turn.turn_id)?
            .into_iter()
            .find(|candidate| candidate.attempt_id == Some(attempt_id));
        let state = self.state_transaction(turn.session_id)?.cells();
        let projection = SessionProjectionSnapshot {
            session: session.clone(),
            branch: branch.clone(),
            turn: turn.clone(),
            attempt: attempt.clone(),
            candidate,
            state,
        };
        let projection_hash = session_projection_hash(&serde_json::to_value(&projection)?)?;
        let revision_hashes = configuration_artifacts(&configuration);
        let mut artifacts = Vec::with_capacity(revision_hashes.len());
        let mut references = Vec::with_capacity(revision_hashes.len());
        for revision_hash in revision_hashes {
            let record = self
                .artifact(&revision_hash)?
                .ok_or_else(|| CapsuleError::ArtifactNotFound(revision_hash.clone()))?;
            let source = match kind {
                CapsuleKind::Portable => Some(
                    String::from_utf8(self.export_artifact(&revision_hash)?)
                        .map_err(|_| CapsuleError::ArtifactSourceNotUtf8(revision_hash.clone()))?,
                ),
                CapsuleKind::Thin => None,
            };
            references.push(CapsuleReference {
                owner_kind: "artifact-revision".to_owned(),
                owner_id: revision_hash.to_string(),
                blob_hash: record.source_blob_hash.clone(),
                embedded: source.is_some(),
            });
            artifacts.push(CapsuleArtifact { record, source });
        }
        let mut capsule = TurnCapsule {
            format: CAPSULE_FORMAT.to_owned(),
            version: CAPSULE_VERSION.to_owned(),
            kind,
            engine: CapsuleEngine {
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            compatibility: CapsuleCompatibility {
                profile: configuration.configuration.compatibility_profile.clone(),
                feature_manifest_digest: feature_manifest_digest()?,
            },
            identity: CapsuleIdentity {
                session_id: turn.session_id,
                branch_id: turn.branch_id,
                turn_id: turn.turn_id,
                attempt_id,
            },
            artifacts,
            configuration: Some(configuration),
            baseline: Some(CapsuleBaseline { session, branch }),
            effects: attempt.effect_receipt.clone(),
            prompt: Some(attempt.prompt_plan.clone()),
            provider: CapsuleProvider {
                request_hash: attempt.provider_request_hash.clone(),
                receipt: attempt.provider_receipt.clone(),
            },
            result: CapsuleResult {
                projection: Some(projection),
                projection_hash: Some(projection_hash),
            },
            references,
            redactions: Vec::new(),
            capabilities: CapsuleCapabilities {
                inspect: true,
                replay: attempt.effect_receipt.is_some(),
                rerun: attempt.effect_receipt.is_some() && attempt.status != AttemptStatus::Running,
            },
        };
        if redact_content {
            capsule.redact_content();
        }
        capsule.recalculate_capabilities(self);
        Ok(capsule)
    }

    pub fn replay_turn_capsule(&self, capsule: &TurnCapsule) -> Result<ReplayReport, CapsuleError> {
        if !capsule.capabilities.replay {
            return Err(CapsuleError::CapabilityDenied("replay"));
        }
        capsule.validate(self)?;
        let projection = capsule
            .result
            .projection
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("result.projection"))?;
        let expected = capsule
            .result
            .projection_hash
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("result.projection_hash"))?;
        let actual = session_projection_hash(&serde_json::to_value(projection)?)?;
        if &actual != expected {
            return Err(CapsuleError::ProjectionHashMismatch {
                expected: expected.clone(),
                actual,
            });
        }
        Ok(ReplayReport {
            capsule_hash: capsule.hash()?,
            projection_hash: expected.clone(),
            provider_calls: 0,
            plugin_executions: 0,
        })
    }
    pub fn import_turn_capsule(
        &mut self,
        capsule: &TurnCapsule,
    ) -> Result<ImportedCapsule, CapsuleError> {
        if !capsule.capabilities.replay {
            return Err(CapsuleError::CapabilityDenied("replay"));
        }
        capsule.validate(self)?;
        let capsule_hash = capsule.hash()?;
        if let Some(session_id) = self
            .connection
            .query_row(
                "SELECT imported_session_id FROM capsule_imports WHERE capsule_hash = ?1",
                [capsule_hash.to_string()],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(crate::StorageError::Sqlite)?
        {
            let session_id = session_id
                .parse()
                .map_err(|_| CapsuleError::StoredIdentityInvalid)?;
            let session = self
                .session(session_id)?
                .ok_or(CapsuleError::SessionNotFound(session_id))?;
            let turns = self.turns_for_branch(session.root_branch_id)?;
            let turn = turns.first().ok_or(CapsuleError::ImportedTurnMissing)?;
            let attempt = self
                .attempts_for_turn(turn.turn_id)?
                .into_iter()
                .next()
                .ok_or(CapsuleError::ImportedAttemptMissing)?;
            return Ok(ImportedCapsule {
                capsule_hash,
                session_id,
                branch_id: session.root_branch_id,
                turn_id: turn.turn_id,
                attempt_id: attempt.attempt_id,
                candidate_id: turn.selected_candidate_id,
            });
        }
        for artifact in &capsule.artifacts {
            if self.artifact(&artifact.record.revision_hash)?.is_none() {
                let source = artifact.source.as_ref().ok_or_else(|| {
                    CapsuleError::MissingReferencedBlob(artifact.record.source_blob_hash.clone())
                })?;
                let imported = self.import_artifact(source.as_bytes())?;
                if imported.revision_hash != artifact.record.revision_hash {
                    return Err(CapsuleError::ArtifactHashMismatch(
                        artifact.record.revision_hash.clone(),
                    ));
                }
            }
        }
        let configuration = capsule
            .configuration
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("configuration"))?;
        let baseline = capsule
            .baseline
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("baseline"))?;
        let projection = capsule
            .result
            .projection
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("result.projection"))?;
        let session_id = EntityId::new();
        let branch_id = EntityId::new();
        let turn_id = EntityId::new();
        let attempt_id = EntityId::new();
        let candidate_id = projection.candidate.as_ref().map(|_| EntityId::new());
        let configuration_bytes =
            canonical_json(&serde_json::to_value(&configuration.configuration)?)?;
        let prompt_bytes = canonical_json(&serde_json::to_value(&projection.attempt.prompt_plan)?)?;
        let effect_bytes = projection
            .attempt
            .effect_receipt
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .map(|value| canonical_json(&value))
            .transpose()?;
        let provider_receipt = projection
            .attempt
            .provider_receipt
            .as_ref()
            .map(canonical_json)
            .transpose()?;
        let capsule_bytes = canonical_json(&serde_json::to_value(capsule)?)?;
        let capsule_blob_hash = content_blob_hash(&capsule_bytes);
        let transaction = self
            .connection
            .transaction()
            .map_err(crate::StorageError::Sqlite)?;
        Store::put_blob(&transaction, &capsule_blob_hash.to_string(), &capsule_bytes)?;
        let configuration_event = append_event(
            &transaction,
            Some(session_id),
            "session.configuration-created",
            &json!({
                "revision_hash": configuration.revision_hash,
                "configuration": configuration.configuration,
            }),
        )?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO session_config_revisions(revision_hash, body, created_event_id) VALUES (?1, ?2, ?3)",
                params![
                    configuration.revision_hash.to_string(),
                    configuration_bytes,
                    configuration_event.event_id.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        let session_event = append_event(
            &transaction,
            Some(session_id),
            "session.created",
            &json!({
                "session_id": session_id,
                "configuration_revision": configuration.revision_hash,
                "root_branch_id": branch_id,
                "greeting_revision": baseline.branch.greeting_revision_hash,
                "greeting_index": baseline.branch.greeting_index,
                "imported_from_capsule": capsule_hash,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, created_event_id) VALUES (?1, ?2, ?3, 0, ?4)",
                params![
                    session_id.to_string(),
                    configuration.revision_hash.to_string(),
                    branch_id.to_string(),
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5)",
                params![
                    branch_id.to_string(),
                    session_id.to_string(),
                    baseline.branch.greeting_revision_hash.to_string(),
                    baseline.branch.greeting_index as i64,
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        let turn_event = append_event(
            &transaction,
            Some(session_id),
            "turn.created",
            &json!({
                "turn_id": turn_id,
                "branch_id": branch_id,
                "user_content": projection.turn.user_content,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    turn_id.to_string(),
                    session_id.to_string(),
                    branch_id.to_string(),
                    projection.turn.user_content,
                    candidate_id.map(|id| id.to_string()),
                    turn_event.event_id.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        let attempt_event = append_event(
            &transaction,
            Some(session_id),
            "attempt.started",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn_id,
                "config_hash": configuration.revision_hash,
                "retry_of_attempt_id": null,
                "prompt_plan": projection.attempt.prompt_plan,
                "effect_receipt": projection.attempt.effect_receipt,
            }),
        )?;
        let completion_event = append_event(
            &transaction,
            Some(session_id),
            "capsule.attempt-replayed",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn_id,
                "status": projection.attempt.status,
                "provider_request_hash": projection.attempt.provider_request_hash,
                "provider_receipt": projection.attempt.provider_receipt,
                "error_message": projection.attempt.error_message,
                "candidate_id": candidate_id,
                "candidate": projection.candidate,
                "state": projection.state,
                "source_capsule_hash": capsule_hash,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO attempts(attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, provider_receipt, effect_receipt, error_message, created_event_id, completed_event_id) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    attempt_id.to_string(),
                    turn_id.to_string(),
                    configuration.revision_hash.to_string(),
                    attempt_status_name(projection.attempt.status),
                    prompt_bytes,
                    projection.attempt.provider_request_hash.as_ref().map(ToString::to_string),
                    provider_receipt,
                    effect_bytes,
                    projection.attempt.error_message,
                    attempt_event.event_id.to_string(),
                    completion_event.event_id.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        if let (Some(source), Some(candidate_id)) = (&projection.candidate, candidate_id) {
            transaction
                .execute(
                    "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                    params![
                        candidate_id.to_string(),
                        turn_id.to_string(),
                        attempt_id.to_string(),
                        candidate_origin_name(source.origin),
                        source.content,
                        completion_event.event_id.to_string(),
                    ],
                )
                .map_err(crate::StorageError::Sqlite)?;
        }
        for cell in &projection.state {
            if cell.key.scope != crate::VariableScope::Local {
                continue;
            }
            transaction
                .execute(
                    "INSERT INTO state_cells(scope_kind, scope_id, name, value, raw_value, owner, origin, revision) VALUES ('local', ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    params![
                        session_id.to_string(),
                        cell.key.name,
                        canonical_json(&cell.value)?,
                        cell.raw_value,
                        cell.owner,
                        cell.origin,
                        cell.revision as i64,
                    ],
                )
                .map_err(crate::StorageError::Sqlite)?;
        }
        transaction
            .execute(
                "INSERT INTO capsules(capsule_hash, kind, body_blob_hash) VALUES (?1, ?2, ?3)",
                params![
                    capsule_hash.to_string(),
                    capsule_kind_name(capsule.kind),
                    capsule_blob_hash.to_string(),
                ],
            )
            .map_err(crate::StorageError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO capsule_imports(capsule_hash, imported_session_id) VALUES (?1, ?2)",
                params![capsule_hash.to_string(), session_id.to_string()],
            )
            .map_err(crate::StorageError::Sqlite)?;
        Store::add_blob_reference(
            &transaction,
            "capsule",
            &capsule_hash.to_string(),
            &capsule_blob_hash.to_string(),
        )?;
        for artifact in &capsule.artifacts {
            transaction
                .execute(
                    "INSERT INTO capsule_artifacts(capsule_hash, revision_hash) VALUES (?1, ?2)",
                    params![
                        capsule_hash.to_string(),
                        artifact.record.revision_hash.to_string(),
                    ],
                )
                .map_err(crate::StorageError::Sqlite)?;
            Store::add_blob_reference(
                &transaction,
                "capsule",
                &capsule_hash.to_string(),
                &artifact.record.source_blob_hash.to_string(),
            )?;
        }
        transaction.commit().map_err(crate::StorageError::Sqlite)?;
        Ok(ImportedCapsule {
            capsule_hash,
            session_id,
            branch_id,
            turn_id,
            attempt_id,
            candidate_id,
        })
    }
}

impl TurnCapsule {
    pub fn hash(&self) -> Result<ContentHash, CapsuleError> {
        canonical_json_hash(CAPSULE_DOMAIN, &serde_json::to_value(self)?)
            .map_err(CapsuleError::Json)
    }

    pub fn redact_content(&mut self) {
        for artifact in &mut self.artifacts {
            artifact.source = None;
        }
        self.configuration = None;
        self.baseline = None;
        self.effects = None;
        self.prompt = None;
        self.provider.receipt = None;
        self.result.projection = None;
        self.result.projection_hash = None;
        for reference in &mut self.references {
            reference.embedded = false;
        }
        self.redactions.push(RedactionEntry {
            path: "/artifacts/*/source,/configuration,/baseline,/effects,/prompt,/provider/receipt,/result"
                .to_owned(),
            reason: "narrative and provider content removed by user request".to_owned(),
        });
        self.capabilities = CapsuleCapabilities {
            inspect: true,
            replay: false,
            rerun: false,
        };
    }

    pub fn recalculate_capabilities(&mut self, store: &Store) {
        let references_available = self.references.iter().all(|reference| {
            reference.embedded
                || store
                    .blob(&reference.blob_hash.to_string())
                    .ok()
                    .flatten()
                    .is_some()
        });
        let replay = self.redactions.is_empty()
            && references_available
            && self.configuration.is_some()
            && self.baseline.is_some()
            && self.effects.is_some()
            && self.prompt.is_some()
            && self.result.projection.is_some()
            && self.result.projection_hash.is_some();
        self.capabilities = CapsuleCapabilities {
            inspect: true,
            replay,
            rerun: replay && self.provider.request_hash.is_some(),
        };
    }

    pub fn validate(&self, store: &Store) -> Result<(), CapsuleError> {
        if self.format != CAPSULE_FORMAT || self.version != CAPSULE_VERSION {
            return Err(CapsuleError::UnsupportedFormat {
                format: self.format.clone(),
                version: self.version.clone(),
            });
        }
        if self.compatibility.feature_manifest_digest != feature_manifest_digest()? {
            return Err(CapsuleError::FeatureManifestMismatch);
        }
        let configuration = self
            .configuration
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("configuration"))?;
        if configuration.revision_hash
            != crate::canonical_json_hash(
                "stcli:session-configuration:v1",
                &serde_json::to_value(&configuration.configuration)?,
            )?
        {
            return Err(CapsuleError::ConfigurationHashMismatch);
        }
        let effect = self
            .effects
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("effects"))?;
        let request_hash = provider_request_hash(&effect.provider_request)?;
        if request_hash != effect.provider_request_hash
            || self.provider.request_hash.as_ref() != Some(&request_hash)
        {
            return Err(CapsuleError::ProviderRequestHashMismatch);
        }
        for receipt in &effect.plugins {
            validate_recorded_receipt(receipt)?;
            let pin = configuration
                .configuration
                .plugins
                .iter()
                .find(|pin| pin.id == receipt.id)
                .ok_or_else(|| CapsuleError::PluginPinMissing(receipt.id.clone()))?;
            if pin.version != receipt.version.to_string()
                || pin.component_hash != receipt.component_sha256
                || pin.capabilities != receipt.grants
            {
                return Err(CapsuleError::PluginPinMismatch(receipt.id.clone()));
            }
        }
        let projection = self
            .result
            .projection
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("result.projection"))?;
        if projection.turn.session_id != self.identity.session_id
            || projection.turn.branch_id != self.identity.branch_id
            || projection.turn.turn_id != self.identity.turn_id
            || projection.attempt.attempt_id != self.identity.attempt_id
            || projection.attempt.turn_id != self.identity.turn_id
            || projection.attempt.config_hash != configuration.revision_hash
        {
            return Err(CapsuleError::IdentityMismatch);
        }
        if let Some(candidate) = &projection.candidate
            && (candidate.turn_id != self.identity.turn_id
                || candidate.attempt_id != Some(self.identity.attempt_id))
        {
            return Err(CapsuleError::IdentityMismatch);
        }
        let mut revisions = HashSet::new();
        for artifact in &self.artifacts {
            if !revisions.insert(artifact.record.revision_hash.clone()) {
                return Err(CapsuleError::DuplicateArtifact(
                    artifact.record.revision_hash.clone(),
                ));
            }
            if let Some(source) = &artifact.source {
                let actual = artifact_revision_hash(
                    artifact.record.kind.as_str(),
                    &artifact.record.source_format,
                    source.as_bytes(),
                );
                if actual != artifact.record.revision_hash
                    || content_blob_hash(source.as_bytes()) != artifact.record.source_blob_hash
                {
                    return Err(CapsuleError::ArtifactHashMismatch(
                        artifact.record.revision_hash.clone(),
                    ));
                }
                let decoded = decode_artifact(source.as_bytes())?;
                if decoded.kind != artifact.record.kind
                    || artifact_semantic_hash(&decoded.semantic)? != artifact.record.semantic_hash
                {
                    return Err(CapsuleError::ArtifactHashMismatch(
                        artifact.record.revision_hash.clone(),
                    ));
                }
            } else if store
                .blob(&artifact.record.source_blob_hash.to_string())?
                .is_none()
            {
                return Err(CapsuleError::MissingReferencedBlob(
                    artifact.record.source_blob_hash.clone(),
                ));
            }
        }
        let expected_revisions = configuration_artifacts(configuration)
            .into_iter()
            .collect::<HashSet<_>>();
        if revisions != expected_revisions {
            return Err(CapsuleError::ArtifactSetMismatch);
        }
        let expected_projection_hash = self
            .result
            .projection_hash
            .as_ref()
            .ok_or(CapsuleError::MissingReplayData("result.projection_hash"))?;
        let actual_projection_hash = session_projection_hash(&serde_json::to_value(projection)?)?;
        if &actual_projection_hash != expected_projection_hash {
            return Err(CapsuleError::ProjectionHashMismatch {
                expected: expected_projection_hash.clone(),
                actual: actual_projection_hash,
            });
        }
        Ok(())
    }
}

fn capsule_kind_name(kind: CapsuleKind) -> &'static str {
    match kind {
        CapsuleKind::Portable => "portable",
        CapsuleKind::Thin => "thin",
    }
}

fn attempt_status_name(status: AttemptStatus) -> &'static str {
    match status {
        AttemptStatus::Running => "running",
        AttemptStatus::Completed => "completed",
        AttemptStatus::Failed => "failed",
        AttemptStatus::Cancelled => "cancelled",
        AttemptStatus::Incomplete => "incomplete",
    }
}

fn candidate_origin_name(origin: CandidateOrigin) -> &'static str {
    match origin {
        CandidateOrigin::Generated => "generated",
        CandidateOrigin::Continued => "continued",
        CandidateOrigin::Manual => "manual",
        CandidateOrigin::AcceptedPartial => "accepted-partial",
    }
}

fn configuration_artifacts(configuration: &SessionConfigurationRecord) -> Vec<ContentHash> {
    let mut revisions = vec![configuration.configuration.character_revision.clone()];
    revisions.extend(
        configuration
            .configuration
            .lorebook_revisions
            .iter()
            .cloned(),
    );
    revisions.extend(
        configuration
            .configuration
            .prompt_preset_revision
            .iter()
            .cloned(),
    );
    revisions.sort_by_key(ToString::to_string);
    revisions.dedup();
    revisions
}

fn feature_manifest_digest() -> Result<ContentHash, CapsuleError> {
    let profile: Value = serde_json::from_str(BUILT_IN_PROFILE)?;
    canonical_json_hash(FEATURE_MANIFEST_DOMAIN, &profile).map_err(CapsuleError::Json)
}

#[derive(Debug, Error)]
pub enum CapsuleError {
    #[error("artifact operation failed: {0}")]
    Artifact(#[from] ArtifactError),
    #[error("session operation failed: {0}")]
    Session(#[from] SessionError),
    #[error("turn operation failed: {0}")]
    Turn(#[from] TurnError),
    #[error("state operation failed: {0}")]
    State(#[from] StateError),
    #[error("capsule storage failed: {0}")]
    Storage(#[from] crate::StorageError),
    #[error("capsule JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("provider operation failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("Plugin operation failed: {0}")]
    Plugin(#[from] crate::PluginError),
    #[error("capsule has no Session Configuration pin for Plugin '{0}'")]
    PluginPinMissing(String),
    #[error("capsule Plugin pin and recorded grant differ for '{0}'")]
    PluginPinMismatch(String),
    #[error("attempt {0} was not found")]
    AttemptNotFound(EntityId),
    #[error("turn {0} was not found")]
    TurnNotFound(EntityId),
    #[error("stored imported capsule identity is invalid")]
    StoredIdentityInvalid,
    #[error("imported capsule session has no turn")]
    ImportedTurnMissing,
    #[error("imported capsule turn has no generation attempt")]
    ImportedAttemptMissing,
    #[error("session {0} was not found")]
    SessionNotFound(EntityId),
    #[error("branch {0} was not found")]
    BranchNotFound(EntityId),
    #[error("configuration revision {0} was not found")]
    ConfigurationNotFound(ContentHash),
    #[error("artifact revision {0} was not found")]
    ArtifactNotFound(ContentHash),
    #[error("artifact revision {0} source is not UTF-8 JSON")]
    ArtifactSourceNotUtf8(ContentHash),
    #[error("unsupported capsule format {format} version {version}")]
    UnsupportedFormat { format: String, version: String },
    #[error("capsule feature manifest digest does not match this engine")]
    FeatureManifestMismatch,
    #[error("capsule configuration hash is invalid")]
    ConfigurationHashMismatch,
    #[error("capsule provider request hash is invalid")]
    ProviderRequestHashMismatch,
    #[error("capsule identities or relationships are invalid")]
    IdentityMismatch,
    #[error("capsule contains duplicate artifact revision {0}")]
    DuplicateArtifact(ContentHash),
    #[error("capsule artifact revision hash is invalid for {0}")]
    ArtifactHashMismatch(ContentHash),
    #[error("capsule artifact set does not match its configuration")]
    ArtifactSetMismatch,
    #[error("thin capsule references missing blob {0}")]
    MissingReferencedBlob(ContentHash),
    #[error("capsule is missing replay data at {0}")]
    MissingReplayData(&'static str),
    #[error("capsule capability denied: {0}")]
    CapabilityDenied(&'static str),
    #[error("capsule projection hash mismatch: expected {expected}, got {actual}")]
    ProjectionHashMismatch {
        expected: ContentHash,
        actual: ContentHash,
    },
}
