use std::{
    collections::{BTreeMap, BTreeSet},
    str::FromStr,
};

use rusqlite::{OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ArtifactKind, ContentHash, ContextFormatting, EntityId, FormatMode, InstructTemplate,
    PluginCapability, PluginError, PluginRegistry, ProviderError, StateMutation, Store,
    TraceEventRecord, VariableScope,
    artifact::ArtifactError,
    identity::{canonical_json, canonical_json_hash},
    provider::validate_text_completion_settings,
    storage::{StorageError, append_event},
    turn::{
        AttemptProjection, AttemptStatus, CandidateProjection, decode_attempt, decode_candidate,
    },
};

const SESSION_CONFIG_DOMAIN: &str = "stcli:session-configuration:v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionConfiguration {
    pub compatibility_profile: String,
    pub character_revision: ContentHash,
    pub persona_name: String,
    #[serde(default, skip_serializing_if = "option_string_is_none_or_blank")]
    pub persona_description: Option<String>,
    pub lorebook_revisions: Vec<ContentHash>,
    pub prompt_preset_revision: Option<ContentHash>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub prompt_order_overrides: BTreeMap<String, bool>,
    pub provider: ProviderSettings,
    pub tokenizer: String,
    pub generation_settings: Value,
    /// Empty by default; omitted from serialization so existing configuration
    /// revisions keep their hashes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub plugins: Vec<PluginPin>,
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        alias = "preset_script_grants"
    )]
    pub script_grants: Vec<ContentHash>,
}
fn option_string_is_none_or_blank(value: &Option<String>) -> bool {
    value.as_deref().is_none_or(|value| value.trim().is_empty())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderSettings {
    pub id: String,
    pub base_url: String,
    pub chat_completions_path: String,
    #[serde(default, skip_serializing_if = "FormatMode::is_chat_completion")]
    pub format_mode: FormatMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completions_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instruct_template: Option<InstructTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_formatting: Option<ContextFormatting>,
    #[serde(default)]
    pub api_key_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_key: Option<String>,
    #[serde(default)]
    pub static_headers: BTreeMap<String, HeaderSetting>,
    pub timeout_seconds: u64,
    #[serde(default)]
    pub ca_certificate_pem: Option<String>,
    pub model: String,
    pub stream: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "source", content = "value", rename_all = "kebab-case")]
pub enum HeaderSetting {
    Literal(String),
    Environment(String),
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginPin {
    pub id: String,
    pub version: String,
    pub component_hash: ContentHash,
    pub capabilities: BTreeSet<PluginCapability>,
    pub settings: Value,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct SessionConfigurationRecord {
    pub revision_hash: ContentHash,
    pub configuration: SessionConfiguration,
    pub created_event_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionProjection {
    pub session_id: EntityId,
    pub current_config_hash: ContentHash,
    pub root_branch_id: EntityId,
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_name: Option<String>,
    pub created_event_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BranchProjection {
    pub branch_id: EntityId,
    pub session_id: EntityId,
    pub parent_branch_id: Option<EntityId>,
    pub forked_from_turn_id: Option<EntityId>,
    pub greeting_revision_hash: ContentHash,
    pub greeting_index: usize,
    pub greeting: String,
    pub created_event_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CreatedSession {
    pub session: SessionProjection,
    pub branch: BranchProjection,
    pub configuration: SessionConfigurationRecord,
}

struct RecordedTurn {
    turn_id: EntityId,
    user_content: String,
    selected_candidate_id: Option<EntityId>,
    selection_history: Vec<EntityId>,
    hidden: bool,
    deleted: bool,
    attempts: Vec<AttemptProjection>,
    candidates: Vec<RecordedCandidate>,
}

struct RecordedCandidate {
    projection: CandidateProjection,
    deleted: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionCounts {
    pub branches: usize,
    pub turns: usize,
    pub candidates: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompactionReport {
    pub removed: CompactionCounts,
    pub preserved: CompactionCounts,
}

pub fn available_duplicated_session_name<'a>(
    source_name: &str,
    existing_names: impl IntoIterator<Item = &'a str>,
) -> String {
    let existing_names = existing_names.into_iter().collect::<BTreeSet<_>>();
    let first = format!("{source_name} (copy)");
    if !existing_names.contains(first.as_str()) {
        return first;
    }
    (2..)
        .map(|counter| format!("{source_name} (copy {counter})"))
        .find(|candidate| !existing_names.contains(candidate.as_str()))
        .expect("an unused Duplicated Session name exists")
}

impl Store {
    pub fn create_session(
        &mut self,
        configuration: SessionConfiguration,
        greeting_index: usize,
    ) -> Result<CreatedSession, SessionError> {
        validate_configuration(self, &configuration)?;
        let character = self.decoded_artifact(&configuration.character_revision)?;
        let greeting = character.greetings.get(greeting_index).cloned().ok_or(
            SessionError::GreetingOutOfRange {
                requested: greeting_index,
                available: character.greetings.len(),
            },
        )?;
        let configuration_value = serde_json::to_value(&configuration)?;
        let configuration_bytes = canonical_json(&configuration_value)?;
        let configuration_hash = canonical_json_hash(SESSION_CONFIG_DOMAIN, &configuration_value)?;
        let configuration = serde_json::from_slice::<SessionConfiguration>(&configuration_bytes)?;
        let session_id = EntityId::new();
        let branch_id = EntityId::new();

        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let configuration_event = append_event(
            &transaction,
            Some(session_id),
            "session.configuration-created",
            &json!({
                "revision_hash": configuration_hash,
                "configuration": configuration,
            }),
        )?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO session_config_revisions(revision_hash, body, created_event_id) VALUES (?1, ?2, ?3)",
                params![
                    configuration_hash.to_string(),
                    configuration_bytes,
                    configuration_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        let session_event = append_event(
            &transaction,
            Some(session_id),
            "session.created",
            &json!({
                "session_id": session_id,
                "configuration_revision": configuration_hash,
                "root_branch_id": branch_id,
                "greeting_revision": configuration.character_revision,
                "greeting_index": greeting_index,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, created_event_id) VALUES (?1, ?2, ?3, 0, ?4)",
                params![
                    session_id.to_string(),
                    configuration_hash.to_string(),
                    branch_id.to_string(),
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5)",
                params![
                    branch_id.to_string(),
                    session_id.to_string(),
                    configuration.character_revision.to_string(),
                    greeting_index as i64,
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;

        Ok(CreatedSession {
            session: SessionProjection {
                session_id,
                current_config_hash: configuration_hash.clone(),
                root_branch_id: branch_id,
                archived: false,
                custom_name: None,
                created_event_id: session_event.event_id.to_string(),
            },
            branch: BranchProjection {
                branch_id,
                session_id,
                parent_branch_id: None,
                forked_from_turn_id: None,
                greeting_revision_hash: configuration.character_revision.clone(),
                greeting_index,
                greeting,
                created_event_id: session_event.event_id.to_string(),
            },
            configuration: SessionConfigurationRecord {
                revision_hash: configuration_hash,
                configuration,
                created_event_id: configuration_event.event_id.to_string(),
            },
        })
    }

    pub fn duplicate_session(
        &mut self,
        source_session_id: EntityId,
        source_branch_id: Option<EntityId>,
        source_up_to_turn_id: Option<EntityId>,
        new_name: Option<String>,
    ) -> Result<CreatedSession, SessionError> {
        let source_session = self
            .session(source_session_id)?
            .ok_or(SessionError::SessionNotFound(source_session_id))?;
        let source_branch_id = source_branch_id.unwrap_or(source_session.root_branch_id);
        let source_branch = self
            .branch(source_branch_id)?
            .ok_or(SessionError::BranchNotFound(source_branch_id))?;
        if source_branch.session_id != source_session_id {
            return Err(SessionError::BranchSessionMismatch);
        }
        let configuration = self
            .configuration(&source_session.current_config_hash)?
            .ok_or_else(|| {
                SessionError::ConfigurationNotFound(source_session.current_config_hash.clone())
            })?;
        let mut turn_ids =
            recorded_lineage(&self.connection, source_branch_id, &mut BTreeSet::new())?;
        if let Some(turn_id) = source_up_to_turn_id {
            let cutoff = turn_ids
                .iter()
                .position(|candidate| *candidate == turn_id)
                .ok_or(SessionError::TurnNotOnBranch {
                    turn_id,
                    branch_id: source_branch_id,
                })?;
            turn_ids.truncate(cutoff + 1);
        }
        let mut selection_history = BTreeMap::<EntityId, Vec<EntityId>>::new();
        for event in self.trace_events(Some(source_session_id))? {
            if event.event_type != "turn.candidate-selected" {
                continue;
            }
            let turn_id = required_string(&event.payload, "turn_id")?
                .parse()
                .map_err(|_| SessionError::InvalidTrace("turn_id"))?;
            let candidate_id = required_string(&event.payload, "candidate_id")?
                .parse()
                .map_err(|_| SessionError::InvalidTrace("candidate_id"))?;
            selection_history
                .entry(turn_id)
                .or_default()
                .push(candidate_id);
        }
        let turns = load_recorded_turns(&self.connection, &turn_ids, &selection_history)?;
        let custom_name =
            self.duplicated_session_name(&source_session, &configuration, new_name)?;

        let session_id = EntityId::new();
        let branch_id = EntityId::new();
        let turn_ids = turns
            .iter()
            .map(|turn| (turn.turn_id, EntityId::new()))
            .collect::<BTreeMap<_, _>>();
        let attempt_ids = turns
            .iter()
            .flat_map(|turn| turn.attempts.iter())
            .map(|attempt| (attempt.attempt_id, EntityId::new()))
            .collect::<BTreeMap<_, _>>();
        let candidate_ids = turns
            .iter()
            .flat_map(|turn| turn.candidates.iter())
            .map(|candidate| (candidate.projection.candidate_id, EntityId::new()))
            .collect::<BTreeMap<_, _>>();
        let configuration_value = serde_json::to_value(&configuration.configuration)?;
        let configuration_bytes = canonical_json(&configuration_value)?;

        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
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
            .map_err(StorageError::Sqlite)?;
        let session_event = append_event(
            &transaction,
            Some(session_id),
            "session.created",
            &json!({
                "session_id": session_id,
                "configuration_revision": configuration.revision_hash,
                "root_branch_id": branch_id,
                "greeting_revision": source_branch.greeting_revision_hash,
                "greeting_index": source_branch.greeting_index,
                "custom_name": custom_name,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, custom_name, created_event_id) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                params![
                    session_id.to_string(),
                    configuration.revision_hash.to_string(),
                    branch_id.to_string(),
                    custom_name,
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5)",
                params![
                    branch_id.to_string(),
                    session_id.to_string(),
                    source_branch.greeting_revision_hash.to_string(),
                    source_branch.greeting_index as i64,
                    session_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "session.duplicated",
            &json!({
                "source_session_id": source_session_id,
                "source_branch_id": source_branch_id,
                "source_up_to_turn_id": source_up_to_turn_id,
                "copied_turns": turns.len(),
                "copied_candidates": candidate_ids.len(),
            }),
        )?;
        append_event(
            &transaction,
            Some(session_id),
            "branch.greeting-selected",
            &json!({
                "branch_id": branch_id,
                "greeting_revision": source_branch.greeting_revision_hash,
                "greeting_index": source_branch.greeting_index,
            }),
        )?;

        for turn in &turns {
            let new_turn_id = turn_ids[&turn.turn_id];
            let turn_event = append_event(
                &transaction,
                Some(session_id),
                "turn.created",
                &json!({
                    "turn_id": new_turn_id,
                    "branch_id": branch_id,
                    "user_content": turn.user_content,
                }),
            )?;
            transaction
                .execute(
                    "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                    params![
                        new_turn_id.to_string(),
                        session_id.to_string(),
                        branch_id.to_string(),
                        turn.user_content,
                        turn_event.event_id.to_string(),
                    ],
                )
                .map_err(StorageError::Sqlite)?;
            let candidates_by_attempt = turn
                .candidates
                .iter()
                .filter_map(|candidate| {
                    candidate
                        .projection
                        .attempt_id
                        .map(|attempt_id| (attempt_id, candidate))
                })
                .collect::<BTreeMap<_, _>>();
            for attempt in &turn.attempts {
                duplicate_attempt(
                    &transaction,
                    session_id,
                    new_turn_id,
                    attempt,
                    candidates_by_attempt.get(&attempt.attempt_id).copied(),
                    &attempt_ids,
                    &candidate_ids,
                )?;
            }
            for candidate in turn
                .candidates
                .iter()
                .filter(|candidate| candidate.projection.attempt_id.is_none())
            {
                duplicate_manual_candidate(
                    &transaction,
                    session_id,
                    new_turn_id,
                    candidate,
                    &candidate_ids,
                )?;
            }
            for selected_candidate_id in &turn.selection_history {
                let selected_candidate_id = candidate_ids
                    .get(selected_candidate_id)
                    .copied()
                    .ok_or(SessionError::InvalidTrace("candidate_id"))?;
                append_event(
                    &transaction,
                    Some(session_id),
                    "turn.candidate-selected",
                    &json!({
                        "turn_id": new_turn_id,
                        "candidate_id": selected_candidate_id,
                    }),
                )?;
            }
            if let Some(selected_candidate_id) = turn.selected_candidate_id {
                let selected_candidate_id = candidate_ids
                    .get(&selected_candidate_id)
                    .copied()
                    .ok_or(SessionError::InvalidTrace("selected_candidate_id"))?;
                transaction
                    .execute(
                        "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                        params![selected_candidate_id.to_string(), new_turn_id.to_string()],
                    )
                    .map_err(StorageError::Sqlite)?;
            }
            duplicate_visibility_and_deletion(
                &transaction,
                session_id,
                new_turn_id,
                turn,
                &candidate_ids,
            )?;
        }
        transaction.commit().map_err(StorageError::Sqlite)?;

        Ok(CreatedSession {
            session: SessionProjection {
                session_id,
                current_config_hash: configuration.revision_hash.clone(),
                root_branch_id: branch_id,
                archived: false,
                custom_name,
                created_event_id: session_event.event_id.to_string(),
            },
            branch: BranchProjection {
                branch_id,
                session_id,
                parent_branch_id: None,
                forked_from_turn_id: None,
                greeting_revision_hash: source_branch.greeting_revision_hash,
                greeting_index: source_branch.greeting_index,
                greeting: source_branch.greeting,
                created_event_id: session_event.event_id.to_string(),
            },
            configuration,
        })
    }

    fn duplicated_session_name(
        &self,
        source: &SessionProjection,
        configuration: &SessionConfigurationRecord,
        requested: Option<String>,
    ) -> Result<Option<String>, SessionError> {
        if let Some(requested) = requested {
            let requested = requested.trim();
            return Ok((!requested.is_empty()).then(|| requested.to_owned()));
        }
        let character = self.decoded_artifact(&configuration.configuration.character_revision)?;
        let source_name = source.custom_name.clone().unwrap_or_else(|| {
            character
                .semantic
                .pointer("/data/name")
                .or_else(|| character.semantic.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("Character")
                .to_owned()
        });
        let mut existing_names = BTreeSet::new();
        for session in self.sessions()? {
            if let Some(name) = session.custom_name {
                existing_names.insert(name);
                continue;
            }
            let configuration = self
                .configuration(&session.current_config_hash)?
                .ok_or_else(|| {
                    SessionError::ConfigurationNotFound(session.current_config_hash.clone())
                })?;
            let character =
                self.decoded_artifact(&configuration.configuration.character_revision)?;
            existing_names.insert(
                character
                    .semantic
                    .pointer("/data/name")
                    .or_else(|| character.semantic.get("name"))
                    .and_then(Value::as_str)
                    .unwrap_or("Character")
                    .to_owned(),
            );
        }
        Ok(Some(available_duplicated_session_name(
            &source_name,
            existing_names.iter().map(String::as_str),
        )))
    }

    pub fn session(&self, session_id: EntityId) -> Result<Option<SessionProjection>, SessionError> {
        self.connection
            .query_row(
                "SELECT session_id, current_config_hash, root_branch_id, archived, custom_name, created_event_id FROM sessions WHERE session_id = ?1",
                [session_id.to_string()],
                decode_session,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    pub fn sessions(&self) -> Result<Vec<SessionProjection>, SessionError> {
        let mut statement = self
            .connection
            .prepare("SELECT session_id, current_config_hash, root_branch_id, archived, custom_name, created_event_id FROM sessions ORDER BY rowid")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([], decode_session)
            .map_err(StorageError::Sqlite)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    pub fn configuration(
        &self,
        revision_hash: &ContentHash,
    ) -> Result<Option<SessionConfigurationRecord>, SessionError> {
        self.connection
            .query_row(
                "SELECT revision_hash, body, created_event_id FROM session_config_revisions WHERE revision_hash = ?1",
                [revision_hash.to_string()],
                decode_configuration,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    pub fn branch(&self, branch_id: EntityId) -> Result<Option<BranchProjection>, SessionError> {
        let stored = self
            .connection
            .query_row(
                "SELECT branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id FROM branches WHERE branch_id = ?1 AND deleted = 0",
                [branch_id.to_string()],
                decode_branch_without_greeting,
            )
            .optional()
            .map_err(StorageError::Sqlite)?;
        stored
            .map(|branch| self.resolve_greeting(branch))
            .transpose()
    }

    pub fn branches(&self, session_id: EntityId) -> Result<Vec<BranchProjection>, SessionError> {
        let mut statement = self
            .connection
            .prepare("SELECT branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id FROM branches WHERE session_id = ?1 AND deleted = 0 ORDER BY rowid")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([session_id.to_string()], decode_branch_without_greeting)
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)?;
        rows.into_iter()
            .map(|branch| self.resolve_greeting(branch))
            .collect()
    }

    pub fn create_branch(
        &mut self,
        session_id: EntityId,
        parent_branch_id: EntityId,
        greeting_index: usize,
    ) -> Result<BranchProjection, SessionError> {
        self.create_branch_at(session_id, parent_branch_id, None, greeting_index)
    }

    pub(crate) fn create_branch_at(
        &mut self,
        session_id: EntityId,
        parent_branch_id: EntityId,
        forked_from_turn_id: Option<EntityId>,
        greeting_index: usize,
    ) -> Result<BranchProjection, SessionError> {
        if let Some(turn_id) = forked_from_turn_id {
            let lineage =
                recorded_lineage(&self.connection, parent_branch_id, &mut BTreeSet::new())?;
            if !lineage.contains(&turn_id) {
                return Err(SessionError::TurnNotOnBranch {
                    turn_id,
                    branch_id: parent_branch_id,
                });
            }
        }
        let session = self
            .session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        let parent = self
            .branch(parent_branch_id)?
            .ok_or(SessionError::BranchNotFound(parent_branch_id))?;
        if parent.session_id != session_id {
            return Err(SessionError::BranchSessionMismatch);
        }
        let configuration = self
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| {
                SessionError::ConfigurationNotFound(session.current_config_hash.clone())
            })?;
        let character = self.decoded_artifact(&configuration.configuration.character_revision)?;
        let greeting = character.greetings.get(greeting_index).cloned().ok_or(
            SessionError::GreetingOutOfRange {
                requested: greeting_index,
                available: character.greetings.len(),
            },
        )?;
        let branch_id = EntityId::new();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(session_id),
            "branch.created",
            &json!({
                "branch_id": branch_id,
                "parent_branch_id": parent_branch_id,
                "forked_from_turn_id": forked_from_turn_id,
                "greeting_revision": configuration.configuration.character_revision,
                "greeting_index": greeting_index,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    branch_id.to_string(),
                    session_id.to_string(),
                    parent_branch_id.to_string(),
                    forked_from_turn_id.map(|id| id.to_string()),
                    configuration.configuration.character_revision.to_string(),
                    greeting_index as i64,
                    event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(BranchProjection {
            branch_id,
            session_id,
            parent_branch_id: Some(parent_branch_id),
            forked_from_turn_id,
            greeting_revision_hash: configuration.configuration.character_revision,
            greeting_index,
            greeting,
            created_event_id: event.event_id.to_string(),
        })
    }

    pub fn delete_branch(&mut self, branch_id: EntityId) -> Result<(), SessionError> {
        let (session_id, root_branch_id) = self
            .connection
            .query_row(
                "SELECT branches.session_id, sessions.root_branch_id FROM branches JOIN sessions ON sessions.session_id = branches.session_id WHERE branches.branch_id = ?1 AND branches.deleted = 0",
                [branch_id.to_string()],
                |row| Ok((parse_column(row, 0)?, parse_column(row, 1)?)),
            )
            .optional()
            .map_err(StorageError::Sqlite)?
            .ok_or(SessionError::BranchNotFound(branch_id))?;
        if branch_id == root_branch_id {
            return Err(SessionError::RootBranchDeletion(branch_id));
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "branch.deleted",
            &json!({"branch_id": branch_id}),
        )?;
        transaction
            .execute(
                "UPDATE branches SET deleted = 1 WHERE branch_id = ?1",
                [branch_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn update_session_configuration(
        &mut self,
        session_id: EntityId,
        configuration: SessionConfiguration,
    ) -> Result<SessionConfigurationRecord, SessionError> {
        self.session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        validate_configuration(self, &configuration)?;
        let value = serde_json::to_value(&configuration)?;
        let body = canonical_json(&value)?;
        let revision_hash = canonical_json_hash(SESSION_CONFIG_DOMAIN, &value)?;
        let configuration = serde_json::from_slice::<SessionConfiguration>(&body)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let created = append_event(
            &transaction,
            Some(session_id),
            "session.configuration-created",
            &json!({
                "revision_hash": revision_hash,
                "configuration": configuration,
            }),
        )?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO session_config_revisions(revision_hash, body, created_event_id) VALUES (?1, ?2, ?3)",
                params![
                    revision_hash.to_string(),
                    body,
                    created.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "session.configuration-selected",
            &json!({
                "session_id": session_id,
                "revision_hash": revision_hash,
            }),
        )?;
        transaction
            .execute(
                "UPDATE sessions SET current_config_hash = ?1 WHERE session_id = ?2",
                params![revision_hash.to_string(), session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(SessionConfigurationRecord {
            revision_hash,
            configuration,
            created_event_id: created.event_id.to_string(),
        })
    }

    pub fn select_greeting(
        &mut self,
        session_id: EntityId,
        branch_id: EntityId,
        greeting_index: usize,
    ) -> Result<BranchProjection, SessionError> {
        let session = self
            .session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        let branch = self
            .branch(branch_id)?
            .ok_or(SessionError::BranchNotFound(branch_id))?;
        if branch.session_id != session_id {
            return Err(SessionError::BranchSessionMismatch);
        }
        let direct_turns = self
            .connection
            .query_row(
                "SELECT COUNT(*) FROM turns WHERE branch_id = ?1",
                [branch_id.to_string()],
                |row| row.get::<_, i64>(0),
            )
            .map_err(StorageError::Sqlite)?;
        if direct_turns > 0 || branch.forked_from_turn_id.is_some() {
            return self.create_branch(session_id, session.root_branch_id, greeting_index);
        }
        let configuration = self
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| {
                SessionError::ConfigurationNotFound(session.current_config_hash.clone())
            })?;
        let character = self.decoded_artifact(&configuration.configuration.character_revision)?;
        let greeting = character.greetings.get(greeting_index).cloned().ok_or(
            SessionError::GreetingOutOfRange {
                requested: greeting_index,
                available: character.greetings.len(),
            },
        )?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "branch.greeting-selected",
            &json!({
                "branch_id": branch_id,
                "greeting_revision": configuration.configuration.character_revision,
                "greeting_index": greeting_index,
            }),
        )?;
        transaction
            .execute(
                "UPDATE branches SET greeting_revision_hash = ?1, greeting_index = ?2 WHERE branch_id = ?3",
                params![
                    configuration.configuration.character_revision.to_string(),
                    greeting_index as i64,
                    branch_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(BranchProjection {
            greeting_revision_hash: configuration.configuration.character_revision,
            greeting_index,
            greeting,
            created_event_id: branch.created_event_id.clone(),
            ..branch
        })
    }
    pub fn plugin_in_use(&self, id: &str) -> Result<bool, SessionError> {
        let mut statement = self
            .connection
            .prepare("SELECT body FROM session_config_revisions")
            .map_err(StorageError::Sqlite)?;
        let rows = statement
            .query_map([], |row| row.get::<_, Vec<u8>>(0))
            .map_err(StorageError::Sqlite)?;
        for row in rows {
            let configuration = serde_json::from_slice::<SessionConfiguration>(
                &row.map_err(StorageError::Sqlite)?,
            )?;
            if configuration.plugins.iter().any(|pin| pin.id == id) {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub fn rename_session(&mut self, session_id: EntityId, name: &str) -> Result<(), SessionError> {
        let name = name.trim();
        let value: Option<&str> = if name.is_empty() { None } else { Some(name) };
        self.connection
            .execute(
                "UPDATE sessions SET custom_name = ?1 WHERE session_id = ?2",
                rusqlite::params![value, session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn archive_session(
        &mut self,
        session_id: EntityId,
    ) -> Result<SessionProjection, SessionError> {
        let mut session = self
            .session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        if session.archived {
            return Ok(session);
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "session.archived",
            &json!({"session_id": session_id}),
        )?;
        transaction
            .execute(
                "UPDATE sessions SET archived = 1 WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        session.archived = true;

        Ok(session)
    }

    pub fn purge_session(&mut self, session_id: EntityId) -> Result<usize, SessionError> {
        self.session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM capsule_imports WHERE imported_session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        let removed_events = transaction
            .execute(
                "DELETE FROM trace_events WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM sessions WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM state_cells WHERE scope_kind = 'local' AND scope_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM session_config_revisions WHERE NOT EXISTS (SELECT 1 FROM sessions WHERE sessions.current_config_hash = session_config_revisions.revision_hash) AND NOT EXISTS (SELECT 1 FROM attempts WHERE attempts.config_hash = session_config_revisions.revision_hash)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM artifact_revisions WHERE NOT EXISTS (SELECT 1 FROM branches WHERE branches.greeting_revision_hash = artifact_revisions.revision_hash) AND NOT EXISTS (SELECT 1 FROM session_config_revisions WHERE CAST(session_config_revisions.body AS TEXT) LIKE '%' || artifact_revisions.revision_hash || '%') AND NOT EXISTS (SELECT 1 FROM capsule_artifacts WHERE capsule_artifacts.revision_hash = artifact_revisions.revision_hash)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM content_refs WHERE owner_kind = 'artifact-revision' AND NOT EXISTS (SELECT 1 FROM artifact_revisions WHERE artifact_revisions.revision_hash = content_refs.owner_id)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "DELETE FROM content_blobs WHERE NOT EXISTS (SELECT 1 FROM content_refs WHERE content_refs.blob_hash = content_blobs.hash)",
                [],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(removed_events)
    }

    fn compaction_branches(
        &self,
        session_id: EntityId,
    ) -> Result<Vec<CompactionBranch>, SessionError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT branch_id, parent_branch_id, forked_from_turn_id, deleted FROM branches WHERE session_id = ?1",
            )
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([session_id.to_string()], |row| {
                Ok(CompactionBranch {
                    id: parse_column(row, 0)?,
                    parent_id: parse_optional_column(row, 1)?,
                    forked_from_turn_id: parse_optional_column(row, 2)?,
                    deleted: row.get::<_, i64>(3)? != 0,
                })
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    fn compaction_turns(&self, session_id: EntityId) -> Result<Vec<CompactionTurn>, SessionError> {
        let mut statement = self
            .connection
            .prepare("SELECT turn_id, branch_id, deleted FROM turns WHERE session_id = ?1")
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([session_id.to_string()], |row| {
                Ok(CompactionTurn {
                    id: parse_column(row, 0)?,
                    branch_id: parse_column(row, 1)?,
                    deleted: row.get::<_, i64>(2)? != 0,
                })
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    fn compaction_candidates(
        &self,
        session_id: EntityId,
    ) -> Result<Vec<CompactionCandidate>, SessionError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT candidates.candidate_id, candidates.turn_id, candidates.attempt_id, candidates.parent_candidate_id, candidates.deleted FROM candidates JOIN turns ON turns.turn_id = candidates.turn_id WHERE turns.session_id = ?1",
            )
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([session_id.to_string()], |row| {
                Ok(CompactionCandidate {
                    id: parse_column(row, 0)?,
                    turn_id: parse_column(row, 1)?,
                    attempt_id: parse_optional_column(row, 2)?,
                    parent_id: parse_optional_column(row, 3)?,
                    deleted: row.get::<_, i64>(4)? != 0,
                })
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    fn compaction_attempts(
        &self,
        session_id: EntityId,
    ) -> Result<Vec<CompactionAttempt>, SessionError> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT attempts.attempt_id, attempts.turn_id, attempts.retry_of_attempt_id FROM attempts JOIN turns ON turns.turn_id = attempts.turn_id WHERE turns.session_id = ?1",
            )
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([session_id.to_string()], |row| {
                Ok(CompactionAttempt {
                    id: parse_column(row, 0)?,
                    turn_id: parse_column(row, 1)?,
                    retry_of_id: parse_optional_column(row, 2)?,
                })
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(SessionError::Storage)
    }

    pub fn compact_session(
        &mut self,
        session_id: EntityId,
    ) -> Result<CompactionReport, SessionError> {
        self.session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        let branches = self.compaction_branches(session_id)?;
        let turns = self.compaction_turns(session_id)?;
        let candidates = self.compaction_candidates(session_id)?;
        let attempts = self.compaction_attempts(session_id)?;
        let deleted_branches = deleted_entity_ids(&branches);
        let deleted_turns = deleted_entity_ids(&turns);
        let deleted_candidates = deleted_entity_ids(&candidates);
        let mut branch_targets = deleted_branches.clone();
        let mut turn_targets = deleted_turns.clone();
        let mut candidate_targets = BTreeSet::new();
        loop {
            let previous = (
                branch_targets.clone(),
                turn_targets.clone(),
                candidate_targets.clone(),
            );
            candidate_targets = candidates
                .iter()
                .filter(|candidate| candidate.deleted || turn_targets.contains(&candidate.turn_id))
                .map(|candidate| candidate.id)
                .collect();
            let candidate_snapshot = candidate_targets.clone();
            candidate_targets.retain(|candidate_id| {
                candidates.iter().all(|candidate| {
                    candidate.parent_id != Some(*candidate_id)
                        || candidate_snapshot.contains(&candidate.id)
                })
            });
            let branch_snapshot = branch_targets.clone();
            let candidate_snapshot = candidate_targets.clone();
            let turn_snapshot = turn_targets.clone();
            turn_targets.retain(|turn_id| {
                branches.iter().all(|branch| {
                    branch.forked_from_turn_id != Some(*turn_id)
                        || branch_snapshot.contains(&branch.id)
                }) && attempts.iter().all(|attempt| {
                    attempt.turn_id != *turn_id
                        || attempts.iter().all(|child| {
                            child.retry_of_id != Some(attempt.id)
                                || turn_snapshot.contains(&child.turn_id)
                        })
                }) && candidates.iter().all(|candidate| {
                    candidate.turn_id != *turn_id
                        || candidates.iter().all(|child| {
                            child.parent_id != Some(candidate.id)
                                || candidate_snapshot.contains(&child.id)
                        })
                })
            });
            let branch_snapshot = branch_targets.clone();
            let turn_snapshot = turn_targets.clone();
            branch_targets.retain(|branch_id| {
                branches.iter().all(|branch| {
                    branch.parent_id != Some(*branch_id) || branch_snapshot.contains(&branch.id)
                }) && turns
                    .iter()
                    .all(|turn| turn.branch_id != *branch_id || turn_snapshot.contains(&turn.id))
            });
            if previous
                == (
                    branch_targets.clone(),
                    turn_targets.clone(),
                    candidate_targets.clone(),
                )
            {
                break;
            }
        }
        candidate_targets = candidates
            .iter()
            .filter(|candidate| candidate.deleted || turn_targets.contains(&candidate.turn_id))
            .map(|candidate| candidate.id)
            .collect();
        let surviving_retry_parents = attempts
            .iter()
            .filter(|attempt| !turn_targets.contains(&attempt.turn_id))
            .filter_map(|attempt| attempt.retry_of_id)
            .collect::<BTreeSet<_>>();
        let candidate_snapshot = candidate_targets.clone();
        candidate_targets.retain(|candidate_id| {
            let candidate = candidates
                .iter()
                .find(|candidate| candidate.id == *candidate_id)
                .expect("candidate target came from inventory");
            candidate
                .attempt_id
                .is_none_or(|attempt_id| !surviving_retry_parents.contains(&attempt_id))
                && candidates.iter().all(|candidate| {
                    candidate.parent_id != Some(*candidate_id)
                        || candidate_snapshot.contains(&candidate.id)
                })
        });

        let turn_attempt_ids = attempts
            .iter()
            .filter(|attempt| turn_targets.contains(&attempt.turn_id))
            .map(|attempt| attempt.id)
            .collect::<BTreeSet<_>>();
        let candidate_attempt_ids = candidates
            .iter()
            .filter(|candidate| candidate_targets.contains(&candidate.id))
            .filter_map(|candidate| candidate.attempt_id)
            .collect::<BTreeSet<_>>();
        let report = CompactionReport {
            removed: CompactionCounts {
                branches: branch_targets.len(),
                turns: turn_targets.len(),
                candidates: candidate_targets.len(),
            },
            preserved: CompactionCounts {
                branches: deleted_branches.len() - branch_targets.len(),
                turns: deleted_turns.len() - turn_targets.len(),
                candidates: deleted_candidates
                    .iter()
                    .filter(|id| !candidate_targets.contains(id))
                    .count(),
            },
        };
        let plan = CompactionPlan {
            branch_ids: branch_targets,
            turn_ids: turn_targets,
            candidate_ids: candidate_targets,
            turn_attempt_ids,
            candidate_attempt_ids,
            report,
        };
        let events = self.trace_events(Some(session_id))?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let report = plan.execute(&transaction, &events, session_id)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        self.connection
            .execute_batch(
                "PRAGMA wal_checkpoint(TRUNCATE); VACUUM; PRAGMA wal_checkpoint(TRUNCATE);",
            )
            .map_err(StorageError::Sqlite)?;
        Ok(report)
    }
    pub fn rebuild_session_projections(&mut self) -> Result<(), SessionError> {
        let events = self.trace_events(None)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM state_cells", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("UPDATE branches SET forked_from_turn_id = NULL", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM candidates", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM attempts", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM turns", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM branches", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM sessions", [])
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute("DELETE FROM session_config_revisions", [])
            .map_err(StorageError::Sqlite)?;

        for event in events {
            match event.event_type.as_str() {
                "session.configuration-created" => {
                    let revision = required_string(&event.payload, "revision_hash")?;
                    let configuration = event
                        .payload
                        .get("configuration")
                        .ok_or(SessionError::InvalidTrace("configuration"))?;
                    let body = canonical_json(configuration)?;
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO session_config_revisions(revision_hash, body, created_event_id) VALUES (?1, ?2, ?3)",
                            params![revision, body, event.event_id.to_string()],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "session.configuration-selected" => {
                    transaction
                        .execute(
                            "UPDATE sessions SET current_config_hash = ?1 WHERE session_id = ?2",
                            params![
                                required_string(&event.payload, "revision_hash")?,
                                required_string(&event.payload, "session_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "session.created" => {
                    let session_id = required_string(&event.payload, "session_id")?;
                    let configuration = required_string(&event.payload, "configuration_revision")?;
                    let branch_id = required_string(&event.payload, "root_branch_id")?;
                    let greeting_revision = required_string(&event.payload, "greeting_revision")?;
                    let greeting_index = required_u64(&event.payload, "greeting_index")?;
                    let custom_name = event.payload.get("custom_name").and_then(Value::as_str);
                    transaction
                        .execute(
                            "INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, custom_name, created_event_id) VALUES (?1, ?2, ?3, 0, ?4, ?5)",
                            params![session_id, configuration, branch_id, custom_name, event.event_id.to_string()],
                        )
                        .map_err(StorageError::Sqlite)?;
                    transaction
                        .execute(
                            "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5)",
                            params![branch_id, session_id, greeting_revision, greeting_index as i64, event.event_id.to_string()],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "branch.created" => {
                    let session_id = event
                        .session_id
                        .ok_or(SessionError::InvalidTrace("session_id"))?;
                    transaction
                        .execute(
                            "INSERT INTO branches(branch_id, session_id, parent_branch_id, forked_from_turn_id, greeting_revision_hash, greeting_index, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                required_string(&event.payload, "branch_id")?,
                                session_id.to_string(),
                                required_string(&event.payload, "parent_branch_id")?,
                                event.payload.get("forked_from_turn_id").and_then(Value::as_str),
                                required_string(&event.payload, "greeting_revision")?,
                                required_u64(&event.payload, "greeting_index")? as i64,
                                event.event_id.to_string(),
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "branch.greeting-selected" => {
                    transaction
                        .execute(
                            "UPDATE branches SET greeting_revision_hash = ?1, greeting_index = ?2 WHERE branch_id = ?3",
                            params![
                                required_string(&event.payload, "greeting_revision")?,
                                required_u64(&event.payload, "greeting_index")? as i64,
                                required_string(&event.payload, "branch_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "turn.created" => {
                    let session_id = event
                        .session_id
                        .ok_or(SessionError::InvalidTrace("session_id"))?;
                    transaction
                        .execute(
                            "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                            params![
                                required_string(&event.payload, "turn_id")?,
                                session_id.to_string(),
                                required_string(&event.payload, "branch_id")?,
                                required_string(&event.payload, "user_content")?,
                                event.event_id.to_string(),
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "turn.hidden" => {
                    transaction
                        .execute(
                            "UPDATE turns SET hidden = ?1 WHERE turn_id = ?2",
                            params![
                                event
                                    .payload
                                    .get("hidden")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                                required_string(&event.payload, "turn_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "turn.deleted" => {
                    transaction
                        .execute(
                            "UPDATE turns SET deleted = 1 WHERE turn_id = ?1",
                            [required_string(&event.payload, "turn_id")?],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "branch.deleted" => {
                    transaction
                        .execute(
                            "UPDATE branches SET deleted = 1 WHERE branch_id = ?1 AND branch_id NOT IN (SELECT root_branch_id FROM sessions)",
                            [required_string(&event.payload, "branch_id")?],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "attempt.started" => {
                    let prompt_plan = event
                        .payload
                        .get("prompt_plan")
                        .ok_or(SessionError::InvalidTrace("prompt_plan"))?;
                    let retry = event
                        .payload
                        .get("retry_of_attempt_id")
                        .and_then(Value::as_str);
                    let effect = event
                        .payload
                        .get("effect_receipt")
                        .map(canonical_json)
                        .transpose()?;
                    let request_hash = event
                        .payload
                        .pointer("/effect_receipt/provider_request_hash")
                        .and_then(Value::as_str);
                    transaction
                        .execute(
                            "INSERT INTO attempts(attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, effect_receipt, created_event_id) VALUES (?1, ?2, ?3, ?4, 'running', ?5, ?6, ?7, ?8)",
                            params![
                                required_string(&event.payload, "attempt_id")?,
                                required_string(&event.payload, "turn_id")?,
                                required_string(&event.payload, "config_hash")?,
                                retry,
                                canonical_json(prompt_plan)?,
                                request_hash,
                                effect,
                                event.event_id.to_string(),
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "attempt.completed" => {
                    let attempt_id = required_string(&event.payload, "attempt_id")?;
                    let turn_id = required_string(&event.payload, "turn_id")?;
                    let candidate_id = required_string(&event.payload, "candidate_id")?;
                    let receipt = event
                        .payload
                        .get("provider_receipt")
                        .ok_or(SessionError::InvalidTrace("provider_receipt"))?;
                    transaction
                        .execute(
                            "UPDATE attempts SET status = 'completed', provider_request_hash = ?1, provider_receipt = ?2, completed_event_id = ?3 WHERE attempt_id = ?4",
                            params![
                                required_string(&event.payload, "provider_request_hash")?,
                                canonical_json(receipt)?,
                                event.event_id.to_string(),
                                attempt_id,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                    transaction
                        .execute(
                            "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                            params![
                                candidate_id,
                                turn_id,
                                attempt_id,
                                event.payload.get("parent_candidate_id").and_then(Value::as_str),
                                event.payload.get("origin").and_then(Value::as_str).unwrap_or("generated"),
                                required_string(&event.payload, "content")?,
                                event.event_id.to_string(),
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                    transaction
                        .execute(
                            "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                            params![candidate_id, turn_id],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "candidate.manual-created" => {
                    let candidate_id = required_string(&event.payload, "candidate_id")?;
                    let turn_id = required_string(&event.payload, "turn_id")?;
                    transaction
                        .execute(
                            "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, NULL, ?3, 'manual', ?4, ?5)",
                            params![
                                candidate_id,
                                turn_id,
                                event.payload.get("parent_candidate_id").and_then(Value::as_str),
                                required_string(&event.payload, "content")?,
                                event.event_id.to_string(),
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                    transaction
                        .execute(
                            "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                            params![candidate_id, turn_id],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "candidate.hidden" => {
                    transaction
                        .execute(
                            "UPDATE candidates SET hidden = ?1 WHERE candidate_id = ?2",
                            params![
                                event
                                    .payload
                                    .get("hidden")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true),
                                required_string(&event.payload, "candidate_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "candidate.deleted" => {
                    let candidate_id = required_string(&event.payload, "candidate_id")?;
                    transaction
                        .execute(
                            "UPDATE candidates SET deleted = 1 WHERE candidate_id = ?1",
                            [candidate_id],
                        )
                        .map_err(StorageError::Sqlite)?;
                    transaction
                        .execute(
                            "UPDATE turns SET selected_candidate_id = NULL WHERE selected_candidate_id = ?1",
                            [candidate_id],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "turn.candidate-selected" => {
                    transaction
                        .execute(
                            "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                            params![
                                required_string(&event.payload, "candidate_id")?,
                                required_string(&event.payload, "turn_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "attempt.failed" => {
                    transaction
                        .execute(
                            "UPDATE attempts SET status = 'failed', error_message = ?1, completed_event_id = ?2 WHERE attempt_id = ?3",
                            params![
                                required_string(&event.payload, "message")?,
                                event.event_id.to_string(),
                                required_string(&event.payload, "attempt_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "attempt.cancelled" => {
                    transaction
                        .execute(
                            "UPDATE attempts SET status = 'cancelled', completed_event_id = ?1 WHERE attempt_id = ?2",
                            params![
                                event.event_id.to_string(),
                                required_string(&event.payload, "attempt_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "attempt.cancellation-receipt" => {
                    let receipt = json!({
                        "cancelled": true,
                        "partial_text": required_string(&event.payload, "partial_text")?,
                    });
                    transaction
                        .execute(
                            "UPDATE attempts SET provider_receipt = ?1, completed_event_id = ?2 WHERE attempt_id = ?3 AND status = 'cancelled'",
                            params![
                                canonical_json(&receipt)?,
                                event.event_id.to_string(),
                                required_string(&event.payload, "attempt_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                    if let Some(candidate_id) =
                        event.payload.get("candidate_id").and_then(Value::as_str)
                    {
                        let turn_id = required_string(&event.payload, "turn_id")?;
                        transaction
                            .execute(
                                "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, ?4, 'accepted-partial', ?5, ?6)",
                                params![
                                    candidate_id,
                                    turn_id,
                                    required_string(&event.payload, "attempt_id")?,
                                    event
                                        .payload
                                        .get("parent_candidate_id")
                                        .and_then(Value::as_str),
                                    required_string(&event.payload, "candidate_content")?,
                                    event.event_id.to_string(),
                                ],
                            )
                            .map_err(StorageError::Sqlite)?;
                        transaction
                            .execute(
                                "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                                params![candidate_id, turn_id],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                }
                "state.committed" => {
                    let session_id = event
                        .session_id
                        .ok_or(SessionError::InvalidTrace("session_id"))?;
                    let mutations = serde_json::from_value::<Vec<StateMutation>>(
                        event
                            .payload
                            .get("mutations")
                            .cloned()
                            .ok_or(SessionError::InvalidTrace("mutations"))?,
                    )?;
                    for mutation in mutations {
                        let scope = match mutation.key.scope {
                            VariableScope::Local => "local",
                            VariableScope::Global => "global",
                        };
                        let scope_id = match mutation.key.scope {
                            VariableScope::Local => session_id.to_string(),
                            VariableScope::Global => "global".to_owned(),
                        };
                        if let Some(cell) = mutation.after {
                            transaction
                                .execute(
                                    "INSERT INTO state_cells(scope_kind, scope_id, name, value, raw_value, owner, origin, revision) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) ON CONFLICT(scope_kind, scope_id, name) DO UPDATE SET value = excluded.value, raw_value = excluded.raw_value, owner = excluded.owner, origin = excluded.origin, revision = excluded.revision",
                                    params![
                                        scope,
                                        scope_id,
                                        cell.key.name,
                                        canonical_json(&cell.value)?,
                                        cell.raw_value,
                                        cell.owner,
                                        cell.origin,
                                        cell.revision as i64,
                                    ],
                                )
                                .map_err(StorageError::Sqlite)?;
                        } else {
                            transaction
                                .execute(
                                    "DELETE FROM state_cells WHERE scope_kind = ?1 AND scope_id = ?2 AND name = ?3",
                                    params![scope, scope_id, mutation.key.name],
                                )
                                .map_err(StorageError::Sqlite)?;
                        }
                    }
                }
                "capsule.attempt-replayed" => {
                    let attempt_id = required_string(&event.payload, "attempt_id")?;
                    let turn_id = required_string(&event.payload, "turn_id")?;
                    let receipt = event
                        .payload
                        .get("provider_receipt")
                        .filter(|value| !value.is_null())
                        .map(canonical_json)
                        .transpose()?;
                    transaction
                        .execute(
                            "UPDATE attempts SET status = ?1, provider_request_hash = ?2, provider_receipt = ?3, error_message = ?4, completed_event_id = ?5 WHERE attempt_id = ?6",
                            params![
                                required_string(&event.payload, "status")?,
                                event.payload.get("provider_request_hash").and_then(Value::as_str),
                                receipt,
                                event.payload.get("error_message").and_then(Value::as_str),
                                event.event_id.to_string(),
                                attempt_id,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                    if let (Some(candidate_id), Some(candidate)) = (
                        event.payload.get("candidate_id").and_then(Value::as_str),
                        event.payload.get("candidate").and_then(Value::as_object),
                    ) {
                        transaction
                            .execute(
                                "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6)",
                                params![
                                    candidate_id,
                                    turn_id,
                                    attempt_id,
                                    candidate.get("origin").and_then(Value::as_str).unwrap_or("generated"),
                                    candidate.get("content").and_then(Value::as_str).ok_or(SessionError::InvalidTrace("candidate.content"))?,
                                    event.event_id.to_string(),
                                ],
                            )
                            .map_err(StorageError::Sqlite)?;
                        transaction
                            .execute(
                                "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                                params![candidate_id, turn_id],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    let session_id = event
                        .session_id
                        .ok_or(SessionError::InvalidTrace("session_id"))?;
                    let state = serde_json::from_value::<Vec<crate::StateCell>>(
                        event
                            .payload
                            .get("state")
                            .cloned()
                            .ok_or(SessionError::InvalidTrace("state"))?,
                    )?;
                    for cell in state
                        .into_iter()
                        .filter(|cell| cell.key.scope == VariableScope::Local)
                    {
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
                            .map_err(StorageError::Sqlite)?;
                    }
                }
                "session.archived" => {
                    transaction
                        .execute(
                            "UPDATE sessions SET archived = 1 WHERE session_id = ?1",
                            [required_string(&event.payload, "session_id")?],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                "session.compacted" => {
                    let branch_ids = compacted_ids(&event.payload, "branch_ids")?;
                    let turn_ids = compacted_ids(&event.payload, "turn_ids")?;
                    let candidate_ids = compacted_ids(&event.payload, "candidate_ids")?;
                    let attempt_ids = compacted_ids(&event.payload, "attempt_ids")?;
                    for branch_id in &branch_ids {
                        transaction
                            .execute(
                                "UPDATE branches SET parent_branch_id = NULL, forked_from_turn_id = NULL WHERE branch_id = ?1",
                                [branch_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for candidate_id in &candidate_ids {
                        transaction
                            .execute(
                                "UPDATE candidates SET parent_candidate_id = NULL WHERE parent_candidate_id = ?1",
                                [candidate_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                        transaction
                            .execute(
                                "UPDATE turns SET selected_candidate_id = NULL WHERE selected_candidate_id = ?1",
                                [candidate_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for candidate_id in candidate_ids {
                        transaction
                            .execute(
                                "DELETE FROM candidates WHERE candidate_id = ?1",
                                [candidate_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for attempt_id in &attempt_ids {
                        transaction
                            .execute(
                                "UPDATE attempts SET retry_of_attempt_id = NULL WHERE retry_of_attempt_id = ?1",
                                [attempt_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for attempt_id in attempt_ids {
                        transaction
                            .execute(
                                "DELETE FROM attempts WHERE attempt_id = ?1",
                                [attempt_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for turn_id in turn_ids {
                        transaction
                            .execute(
                                "DELETE FROM candidates WHERE turn_id = ?1",
                                [turn_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                        transaction
                            .execute(
                                "DELETE FROM attempts WHERE turn_id = ?1",
                                [turn_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                        transaction
                            .execute(
                                "DELETE FROM turns WHERE turn_id = ?1",
                                [turn_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                    for branch_id in branch_ids {
                        transaction
                            .execute(
                                "DELETE FROM branches WHERE branch_id = ?1",
                                [branch_id.to_string()],
                            )
                            .map_err(StorageError::Sqlite)?;
                    }
                }
                "attempt.recovered-incomplete" => {
                    transaction
                        .execute(
                            "UPDATE attempts SET status = 'incomplete', completed_event_id = ?1 WHERE attempt_id = ?2",
                            params![
                                event.event_id.to_string(),
                                required_string(&event.payload, "attempt_id")?,
                            ],
                        )
                        .map_err(StorageError::Sqlite)?;
                }
                _ => {}
            }
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(())
    }

    fn resolve_greeting(&self, stored: StoredBranch) -> Result<BranchProjection, SessionError> {
        let character = self.decoded_artifact(&stored.greeting_revision_hash)?;
        let greeting = character
            .greetings
            .get(stored.greeting_index)
            .cloned()
            .ok_or(SessionError::GreetingOutOfRange {
                requested: stored.greeting_index,
                available: character.greetings.len(),
            })?;
        Ok(BranchProjection {
            branch_id: stored.branch_id,
            session_id: stored.session_id,
            parent_branch_id: stored.parent_branch_id,
            forked_from_turn_id: stored.forked_from_turn_id,
            greeting_revision_hash: stored.greeting_revision_hash,
            greeting_index: stored.greeting_index,
            greeting,
            created_event_id: stored.created_event_id,
        })
    }
}
fn recorded_lineage(
    connection: &rusqlite::Connection,
    branch_id: EntityId,
    visited: &mut BTreeSet<EntityId>,
) -> Result<Vec<EntityId>, SessionError> {
    if !visited.insert(branch_id) {
        return Err(SessionError::InvalidTrace("branch cycle"));
    }
    if visited.len() > crate::limits::MAX_BRANCH_DEPTH {
        return Err(SessionError::InvalidTrace("branch depth"));
    }
    let (parent_branch_id, forked_from_turn_id) = connection
        .query_row(
            "SELECT parent_branch_id, forked_from_turn_id FROM branches WHERE branch_id = ?1 AND deleted = 0",
            [branch_id.to_string()],
            |row| {
                Ok((
                    parse_optional_column(row, 0)?,
                    parse_optional_column(row, 1)?,
                ))
            },
        )
        .optional()
        .map_err(StorageError::Sqlite)?
        .ok_or(SessionError::BranchNotFound(branch_id))?;
    let mut turns = if let (Some(parent), Some(fork)) = (parent_branch_id, forked_from_turn_id) {
        let mut inherited = recorded_lineage(connection, parent, visited)?;
        let cutoff = inherited
            .iter()
            .position(|turn_id| *turn_id == fork)
            .ok_or(SessionError::InvalidTrace("forked_from_turn_id"))?;
        inherited.truncate(cutoff);
        inherited
    } else {
        Vec::new()
    };
    let mut statement = connection
        .prepare("SELECT turn_id FROM turns WHERE branch_id = ?1 ORDER BY rowid")
        .map_err(StorageError::Sqlite)?;
    turns.extend(
        statement
            .query_map([branch_id.to_string()], |row| {
                parse_column::<EntityId>(row, 0)
            })
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)?,
    );
    Ok(turns)
}

fn load_recorded_turns(
    connection: &rusqlite::Connection,
    turn_ids: &[EntityId],
    selection_history: &BTreeMap<EntityId, Vec<EntityId>>,
) -> Result<Vec<RecordedTurn>, SessionError> {
    turn_ids
        .iter()
        .map(|turn_id| {
            let (user_content, selected_candidate_id, hidden, deleted) = connection
                .query_row(
                    "SELECT user_content, selected_candidate_id, hidden, deleted FROM turns WHERE turn_id = ?1",
                    [turn_id.to_string()],
                    |row| {
                        Ok((
                            row.get(0)?,
                            parse_optional_column(row, 1)?,
                            row.get::<_, i64>(2)? != 0,
                            row.get::<_, i64>(3)? != 0,
                        ))
                    },
                )
                .map_err(StorageError::Sqlite)?;
            let attempts = {
                let mut statement = connection
                    .prepare("SELECT attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, provider_receipt, effect_receipt, error_message, created_event_id, completed_event_id FROM attempts WHERE turn_id = ?1 ORDER BY rowid")
                    .map_err(StorageError::Sqlite)?;
                statement
                    .query_map([turn_id.to_string()], decode_attempt)
                    .map_err(StorageError::Sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StorageError::Sqlite)?
            };
            let candidates = {
                let mut statement = connection
                    .prepare("SELECT candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id, hidden, deleted FROM candidates WHERE turn_id = ?1 ORDER BY rowid")
                    .map_err(StorageError::Sqlite)?;
                statement
                    .query_map([turn_id.to_string()], |row| {
                        Ok(RecordedCandidate {
                            projection: decode_candidate(row)?,
                            deleted: row.get::<_, i64>(8)? != 0,
                        })
                    })
                    .map_err(StorageError::Sqlite)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StorageError::Sqlite)?
            };
            Ok(RecordedTurn {
                turn_id: *turn_id,
                user_content,
                selected_candidate_id,
                selection_history: selection_history.get(turn_id).cloned().unwrap_or_default(),
                hidden,
                deleted,
                attempts,
                candidates,
            })
        })
        .collect()
}

fn duplicate_attempt(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    turn_id: EntityId,
    attempt: &AttemptProjection,
    source_candidate: Option<&RecordedCandidate>,
    attempt_ids: &BTreeMap<EntityId, EntityId>,
    candidate_ids: &BTreeMap<EntityId, EntityId>,
) -> Result<(), SessionError> {
    let attempt_id = attempt_ids[&attempt.attempt_id];
    let retry_of_attempt_id = attempt
        .retry_of_attempt_id
        .map(|source_id| {
            attempt_ids
                .get(&source_id)
                .copied()
                .ok_or(SessionError::InvalidTrace("retry_of_attempt_id"))
        })
        .transpose()?;
    let mut prompt_plan = attempt.prompt_plan.clone();
    prompt_plan.parent_candidate_id =
        mapped_candidate_id(prompt_plan.parent_candidate_id, candidate_ids);
    let started = append_event(
        transaction,
        Some(session_id),
        "attempt.started",
        &json!({
            "attempt_id": attempt_id,
            "turn_id": turn_id,
            "config_hash": attempt.config_hash,
            "retry_of_attempt_id": retry_of_attempt_id,
            "prompt_plan": prompt_plan,
            "effect_receipt": attempt.effect_receipt,
        }),
    )?;
    let mut candidate_event = None;
    let completed_event = match attempt.status {
        AttemptStatus::Running => None,
        AttemptStatus::Completed => {
            let candidate =
                source_candidate.ok_or(SessionError::InvalidTrace("completed candidate"))?;
            let candidate_id = candidate_ids[&candidate.projection.candidate_id];
            let parent_candidate_id =
                mapped_candidate_id(candidate.projection.parent_candidate_id, candidate_ids);
            let request_hash = attempt
                .provider_request_hash
                .as_ref()
                .ok_or(SessionError::InvalidTrace("provider_request_hash"))?;
            let receipt = attempt
                .provider_receipt
                .as_ref()
                .ok_or(SessionError::InvalidTrace("provider_receipt"))?;
            let event = append_event(
                transaction,
                Some(session_id),
                "attempt.completed",
                &json!({
                    "attempt_id": attempt_id,
                    "turn_id": turn_id,
                    "candidate_id": candidate_id,
                    "parent_candidate_id": parent_candidate_id,
                    "origin": candidate.projection.origin.as_str(),
                    "provider_request_hash": request_hash,
                    "provider_receipt": receipt,
                    "plugin_receipts": attempt.effect_receipt.as_ref().map(|effect| &effect.plugins),
                    "content": candidate.projection.content,
                }),
            )?;
            candidate_event = Some((candidate, event.event_id.to_string()));
            Some(event.event_id.to_string())
        }
        AttemptStatus::Failed => {
            let event = append_event(
                transaction,
                Some(session_id),
                "attempt.failed",
                &json!({
                    "attempt_id": attempt_id,
                    "turn_id": turn_id,
                    "message": attempt.error_message.as_deref().unwrap_or("attempt failed"),
                }),
            )?;
            Some(event.event_id.to_string())
        }
        AttemptStatus::Cancelled => {
            let cancelled = append_event(
                transaction,
                Some(session_id),
                "attempt.cancelled",
                &json!({"attempt_id": attempt_id, "turn_id": turn_id}),
            )?;
            if let Some(receipt) = &attempt.provider_receipt {
                let source_candidate_id =
                    source_candidate.map(|candidate| candidate.projection.candidate_id);
                let candidate_id = source_candidate_id.map(|source_id| candidate_ids[&source_id]);
                let parent_candidate_id = source_candidate.and_then(|candidate| {
                    mapped_candidate_id(candidate.projection.parent_candidate_id, candidate_ids)
                });
                let candidate_content =
                    source_candidate.map(|candidate| candidate.projection.content.as_str());
                let event = append_event(
                    transaction,
                    Some(session_id),
                    "attempt.cancellation-receipt",
                    &json!({
                        "attempt_id": attempt_id,
                        "turn_id": turn_id,
                        "partial_text": receipt.get("partial_text").and_then(Value::as_str).unwrap_or(""),
                        "candidate_content": candidate_content,
                        "parent_candidate_id": parent_candidate_id,
                        "candidate_id": candidate_id,
                        "origin": candidate_id.map(|_| "accepted-partial"),
                    }),
                )?;
                if let Some(candidate) = source_candidate {
                    candidate_event = Some((candidate, event.event_id.to_string()));
                }
                Some(event.event_id.to_string())
            } else {
                Some(cancelled.event_id.to_string())
            }
        }
        AttemptStatus::Incomplete => {
            let event = append_event(
                transaction,
                Some(session_id),
                "attempt.recovered-incomplete",
                &json!({"attempt_id": attempt_id, "turn_id": turn_id}),
            )?;
            Some(event.event_id.to_string())
        }
    };
    let prompt_bytes = canonical_json(&serde_json::to_value(&prompt_plan)?)?;
    let receipt_bytes = attempt
        .provider_receipt
        .as_ref()
        .map(canonical_json)
        .transpose()?;
    let effect_bytes = attempt
        .effect_receipt
        .as_ref()
        .map(serde_json::to_value)
        .transpose()?
        .as_ref()
        .map(canonical_json)
        .transpose()?;
    transaction
        .execute(
            "INSERT INTO attempts(attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, provider_receipt, effect_receipt, error_message, created_event_id, completed_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                attempt_id.to_string(),
                turn_id.to_string(),
                attempt.config_hash.to_string(),
                retry_of_attempt_id.map(|id| id.to_string()),
                attempt.status.as_str(),
                prompt_bytes,
                attempt.provider_request_hash.as_ref().map(ToString::to_string),
                receipt_bytes,
                effect_bytes,
                attempt.error_message,
                started.event_id.to_string(),
                completed_event,
            ],
        )
        .map_err(StorageError::Sqlite)?;
    if let Some((candidate, event_id)) = candidate_event {
        insert_duplicated_candidate(
            transaction,
            turn_id,
            Some(attempt_id),
            candidate,
            candidate_ids,
            &event_id,
        )?;
    }
    Ok(())
}

fn duplicate_manual_candidate(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    turn_id: EntityId,
    candidate: &RecordedCandidate,
    candidate_ids: &BTreeMap<EntityId, EntityId>,
) -> Result<(), SessionError> {
    let candidate_id = candidate_ids[&candidate.projection.candidate_id];
    let parent_candidate_id =
        mapped_candidate_id(candidate.projection.parent_candidate_id, candidate_ids);
    let event = append_event(
        transaction,
        Some(session_id),
        "candidate.manual-created",
        &json!({
            "candidate_id": candidate_id,
            "turn_id": turn_id,
            "parent_candidate_id": parent_candidate_id,
            "content": candidate.projection.content,
        }),
    )?;
    insert_duplicated_candidate(
        transaction,
        turn_id,
        None,
        candidate,
        candidate_ids,
        &event.event_id.to_string(),
    )
}

fn insert_duplicated_candidate(
    transaction: &Transaction<'_>,
    turn_id: EntityId,
    attempt_id: Option<EntityId>,
    candidate: &RecordedCandidate,
    candidate_ids: &BTreeMap<EntityId, EntityId>,
    created_event_id: &str,
) -> Result<(), SessionError> {
    let candidate_id = candidate_ids[&candidate.projection.candidate_id];
    let parent_candidate_id =
        mapped_candidate_id(candidate.projection.parent_candidate_id, candidate_ids);
    transaction
        .execute(
            "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                candidate_id.to_string(),
                turn_id.to_string(),
                attempt_id.map(|id| id.to_string()),
                parent_candidate_id.map(|id| id.to_string()),
                candidate.projection.origin.as_str(),
                candidate.projection.content,
                created_event_id,
            ],
        )
        .map_err(StorageError::Sqlite)?;
    Ok(())
}

fn duplicate_visibility_and_deletion(
    transaction: &Transaction<'_>,
    session_id: EntityId,
    turn_id: EntityId,
    turn: &RecordedTurn,
    candidate_ids: &BTreeMap<EntityId, EntityId>,
) -> Result<(), SessionError> {
    for candidate in &turn.candidates {
        let candidate_id = candidate_ids[&candidate.projection.candidate_id];
        if candidate.projection.hidden {
            append_event(
                transaction,
                Some(session_id),
                "candidate.hidden",
                &json!({"candidate_id": candidate_id, "hidden": true}),
            )?;
            transaction
                .execute(
                    "UPDATE candidates SET hidden = 1 WHERE candidate_id = ?1",
                    [candidate_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        if candidate.deleted {
            append_event(
                transaction,
                Some(session_id),
                "candidate.deleted",
                &json!({"candidate_id": candidate_id, "turn_id": turn_id}),
            )?;
            transaction
                .execute(
                    "UPDATE candidates SET deleted = 1 WHERE candidate_id = ?1",
                    [candidate_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
    }
    if turn.hidden {
        append_event(
            transaction,
            Some(session_id),
            "turn.hidden",
            &json!({"turn_id": turn_id, "hidden": true}),
        )?;
        transaction
            .execute(
                "UPDATE turns SET hidden = 1 WHERE turn_id = ?1",
                [turn_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
    }
    if turn.deleted {
        append_event(
            transaction,
            Some(session_id),
            "turn.deleted",
            &json!({"turn_id": turn_id}),
        )?;
        transaction
            .execute(
                "UPDATE turns SET deleted = 1 WHERE turn_id = ?1",
                [turn_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
    }
    Ok(())
}

fn mapped_candidate_id(
    source_id: Option<EntityId>,
    candidate_ids: &BTreeMap<EntityId, EntityId>,
) -> Option<EntityId> {
    source_id.and_then(|source_id| candidate_ids.get(&source_id).copied())
}

trait CompactableEntity {
    fn id(&self) -> EntityId;
    fn deleted(&self) -> bool;
}

fn deleted_entity_ids<T: CompactableEntity>(entities: &[T]) -> BTreeSet<EntityId> {
    entities
        .iter()
        .filter(|entity| entity.deleted())
        .map(CompactableEntity::id)
        .collect()
}

#[derive(Clone, Copy, Debug)]
struct CompactionBranch {
    id: EntityId,
    parent_id: Option<EntityId>,
    forked_from_turn_id: Option<EntityId>,
    deleted: bool,
}

impl CompactableEntity for CompactionBranch {
    fn id(&self) -> EntityId {
        self.id
    }

    fn deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Clone, Copy, Debug)]
struct CompactionTurn {
    id: EntityId,
    branch_id: EntityId,
    deleted: bool,
}

impl CompactableEntity for CompactionTurn {
    fn id(&self) -> EntityId {
        self.id
    }

    fn deleted(&self) -> bool {
        self.deleted
    }
}

#[derive(Clone, Copy, Debug)]
struct CompactionCandidate {
    id: EntityId,
    turn_id: EntityId,
    attempt_id: Option<EntityId>,
    parent_id: Option<EntityId>,
    deleted: bool,
}

impl CompactableEntity for CompactionCandidate {
    fn id(&self) -> EntityId {
        self.id
    }

    fn deleted(&self) -> bool {
        self.deleted
    }
}

struct CompactionPlan {
    branch_ids: BTreeSet<EntityId>,
    turn_ids: BTreeSet<EntityId>,
    candidate_ids: BTreeSet<EntityId>,
    turn_attempt_ids: BTreeSet<EntityId>,
    candidate_attempt_ids: BTreeSet<EntityId>,
    report: CompactionReport,
}

impl CompactionPlan {
    fn attempt_ids(&self) -> BTreeSet<EntityId> {
        self.turn_attempt_ids
            .union(&self.candidate_attempt_ids)
            .copied()
            .collect()
    }

    fn contains_event(&self, event: &TraceEventRecord) -> bool {
        event_belongs_to_compacted_entity(
            event,
            &self.branch_ids,
            &self.turn_ids,
            &self.candidate_ids,
            &self.turn_attempt_ids,
            &self.candidate_attempt_ids,
        )
    }

    fn execute(
        self,
        transaction: &Transaction<'_>,
        events: &[TraceEventRecord],
        session_id: EntityId,
    ) -> Result<CompactionReport, SessionError> {
        let attempt_ids = self.attempt_ids();
        for candidate_id in &self.candidate_ids {
            transaction
                .execute(
                    "UPDATE candidates SET parent_candidate_id = NULL WHERE parent_candidate_id = ?1",
                    [candidate_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
            transaction
                .execute(
                    "UPDATE turns SET selected_candidate_id = NULL WHERE selected_candidate_id = ?1",
                    [candidate_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        delete_entities(
            transaction,
            "candidates",
            "candidate_id",
            &self.candidate_ids,
        )?;
        for attempt_id in &attempt_ids {
            transaction
                .execute(
                    "UPDATE attempts SET retry_of_attempt_id = NULL WHERE attempt_id = ?1 OR retry_of_attempt_id = ?1",
                    [attempt_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        delete_entities(transaction, "attempts", "attempt_id", &attempt_ids)?;
        for turn_id in &self.turn_ids {
            transaction
                .execute(
                    "UPDATE branches SET forked_from_turn_id = NULL WHERE forked_from_turn_id = ?1",
                    [turn_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        for branch_id in &self.branch_ids {
            transaction
                .execute(
                    "UPDATE branches SET parent_branch_id = NULL WHERE parent_branch_id = ?1",
                    [branch_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        delete_entities(transaction, "turns", "turn_id", &self.turn_ids)?;
        delete_entities(transaction, "branches", "branch_id", &self.branch_ids)?;
        for event in events.iter().filter(|event| self.contains_event(event)) {
            transaction
                .execute(
                    "DELETE FROM trace_events WHERE event_id = ?1",
                    [event.event_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        append_event(
            transaction,
            Some(session_id),
            "session.compacted",
            &json!({
                "branch_ids": &self.branch_ids,
                "turn_ids": &self.turn_ids,
                "candidate_ids": &self.candidate_ids,
                "attempt_ids": &attempt_ids,
                "report": &self.report,
            }),
        )?;
        Ok(self.report)
    }
}

fn delete_entities(
    transaction: &Transaction<'_>,
    table: &'static str,
    id_column: &'static str,
    ids: &BTreeSet<EntityId>,
) -> Result<(), SessionError> {
    let statement = format!("DELETE FROM {table} WHERE {id_column} = ?1");
    for id in ids {
        transaction
            .execute(&statement, [id.to_string()])
            .map_err(StorageError::Sqlite)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug)]
struct CompactionAttempt {
    id: EntityId,
    turn_id: EntityId,
    retry_of_id: Option<EntityId>,
}

fn event_belongs_to_compacted_entity(
    event: &TraceEventRecord,
    branch_ids: &BTreeSet<EntityId>,
    turn_ids: &BTreeSet<EntityId>,
    candidate_ids: &BTreeSet<EntityId>,
    turn_attempt_ids: &BTreeSet<EntityId>,
    candidate_attempt_ids: &BTreeSet<EntityId>,
) -> bool {
    let references = |key: &str, ids: &BTreeSet<EntityId>| {
        event
            .payload
            .get(key)
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<EntityId>().ok())
            .is_some_and(|id| ids.contains(&id))
    };
    references("turn_id", turn_ids)
        || references("attempt_id", turn_attempt_ids)
        || references("attempt_id", candidate_attempt_ids)
        || (event.event_type.starts_with("branch.") && references("branch_id", branch_ids))
        || (event.event_type.starts_with("candidate.") && references("candidate_id", candidate_ids))
        || (event.event_type == "turn.candidate-selected"
            && references("candidate_id", candidate_ids))
}

fn validate_configuration(
    store: &Store,
    configuration: &SessionConfiguration,
) -> Result<(), SessionError> {
    validate_text_completion_settings(&configuration.provider)?;
    let character = store
        .artifact(&configuration.character_revision)?
        .ok_or_else(|| SessionError::ArtifactNotFound(configuration.character_revision.clone()))?;
    if !matches!(
        character.kind,
        ArtifactKind::CharacterCardV1
            | ArtifactKind::CharacterCardV2
            | ArtifactKind::CharacterCardV3
    ) {
        return Err(SessionError::CharacterRequired(character.kind));
    }
    for revision in &configuration.lorebook_revisions {
        let artifact = store
            .artifact(revision)?
            .ok_or_else(|| SessionError::ArtifactNotFound(revision.clone()))?;
        if artifact.kind != ArtifactKind::Lorebook {
            return Err(SessionError::LorebookRequired(artifact.kind));
        }
    }
    if let Some(revision) = &configuration.prompt_preset_revision {
        let artifact = store
            .artifact(revision)?
            .ok_or_else(|| SessionError::ArtifactNotFound(revision.clone()))?;
        if artifact.kind != ArtifactKind::ChatCompletionPreset {
            return Err(SessionError::PromptPresetRequired(artifact.kind));
        }
    }
    let installed = PluginRegistry::new(
        store
            .path()
            .parent()
            .unwrap_or_else(|| std::path::Path::new("."))
            .join("plugins"),
    )
    .list()?;
    let mut ids = BTreeSet::new();
    for pin in &configuration.plugins {
        if !ids.insert(&pin.id) {
            return Err(SessionError::DuplicatePlugin(pin.id.clone()));
        }
        let version = semver::Version::parse(&pin.version)
            .map_err(|_| SessionError::InvalidPluginVersion(pin.version.clone()))?;
        let plugin = installed.iter().find(|plugin| {
            plugin.manifest.id == pin.id
                && plugin.manifest.version == version
                && plugin.manifest.component_sha256 == pin.component_hash
        });
        let plugin = plugin.ok_or_else(|| SessionError::PluginNotInstalled(pin.id.clone()))?;
        if !pin
            .capabilities
            .is_subset(&plugin.manifest.requested_capabilities)
        {
            return Err(SessionError::PluginGrantExceeded(pin.id.clone()));
        }
    }
    Ok(())
}

fn decode_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionProjection> {
    Ok(SessionProjection {
        session_id: parse_column(row, 0)?,
        current_config_hash: parse_column(row, 1)?,
        root_branch_id: parse_column(row, 2)?,
        archived: row.get::<_, i64>(3)? != 0,
        custom_name: row.get::<_, Option<String>>(4)?,
        created_event_id: row.get(5)?,
    })
}

fn decode_configuration(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionConfigurationRecord> {
    let body: Vec<u8> = row.get(1)?;
    Ok(SessionConfigurationRecord {
        revision_hash: parse_column(row, 0)?,
        configuration: serde_json::from_slice(&body).map_err(|error| conversion_error(1, error))?,
        created_event_id: row.get(2)?,
    })
}

struct StoredBranch {
    branch_id: EntityId,
    session_id: EntityId,
    parent_branch_id: Option<EntityId>,
    forked_from_turn_id: Option<EntityId>,
    greeting_revision_hash: ContentHash,
    greeting_index: usize,
    created_event_id: String,
}

fn decode_branch_without_greeting(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredBranch> {
    let greeting_index: i64 = row.get(5)?;
    Ok(StoredBranch {
        branch_id: parse_column(row, 0)?,
        session_id: parse_column(row, 1)?,
        parent_branch_id: parse_optional_column(row, 2)?,
        forked_from_turn_id: parse_optional_column(row, 3)?,
        greeting_revision_hash: parse_column(row, 4)?,
        greeting_index: usize::try_from(greeting_index)
            .map_err(|error| conversion_error(5, error))?,
        created_event_id: row.get(6)?,
    })
}

fn parse_column<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value: String = row.get(index)?;
    value
        .parse()
        .map_err(|error| conversion_error(index, error))
}

fn parse_optional_column<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value: Option<String> = row.get(index)?;
    value
        .map(|value| {
            value
                .parse()
                .map_err(|error| conversion_error(index, error))
        })
        .transpose()
}

fn conversion_error(
    index: usize,
    error: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(index, rusqlite::types::Type::Text, Box::new(error))
}

fn compacted_ids(value: &Value, key: &'static str) -> Result<Vec<EntityId>, SessionError> {
    serde_json::from_value(
        value
            .get(key)
            .cloned()
            .ok_or(SessionError::InvalidTrace(key))?,
    )
    .map_err(SessionError::Json)
}

fn required_string<'a>(value: &'a Value, key: &'static str) -> Result<&'a str, SessionError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or(SessionError::InvalidTrace(key))
}

fn required_u64(value: &Value, key: &'static str) -> Result<u64, SessionError> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .ok_or(SessionError::InvalidTrace(key))
}

#[derive(Debug, Error)]
pub enum SessionError {
    #[error("session storage failed: {0}")]
    Storage(#[from] StorageError),
    #[error("artifact operation failed: {0}")]
    Artifact(#[from] ArtifactError),
    #[error("session configuration JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Plugin operation failed: {0}")]
    Plugin(#[from] PluginError),
    #[error("provider settings are invalid: {0}")]
    Provider(#[from] ProviderError),
    #[error("Plugin '{0}' appears more than once")]
    DuplicatePlugin(String),
    #[error("Plugin version '{0}' is invalid")]
    InvalidPluginVersion(String),
    #[error("Plugin '{0}' with its pinned digest is not installed")]
    PluginNotInstalled(String),
    #[error("Plugin '{0}' grant exceeds its manifest request")]
    PluginGrantExceeded(String),
    #[error("artifact revision {0} was not found")]
    ArtifactNotFound(ContentHash),
    #[error("session character must be a character card, found {0}")]
    CharacterRequired(ArtifactKind),
    #[error("session lore source must be a lorebook, found {0}")]
    LorebookRequired(ArtifactKind),
    #[error("session prompt preset must be a Chat Completion preset, found {0}")]
    PromptPresetRequired(ArtifactKind),
    #[error("greeting index {requested} is unavailable; character has {available} greeting(s)")]
    GreetingOutOfRange { requested: usize, available: usize },
    #[error("session {0} was not found")]
    SessionNotFound(EntityId),
    #[error("branch {0} was not found")]
    BranchNotFound(EntityId),
    #[error("root branch {0} cannot be deleted; delete the session instead")]
    RootBranchDeletion(EntityId),
    #[error("branch belongs to a different session")]
    BranchSessionMismatch,
    #[error("turn {turn_id} is not on branch {branch_id}")]
    TurnNotOnBranch {
        turn_id: EntityId,
        branch_id: EntityId,
    },
    #[error("session configuration revision {0} was not found")]
    ConfigurationNotFound(ContentHash),
    #[error("authoritative trace is missing or has invalid field '{0}'")]
    InvalidTrace(&'static str),
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
            "plugins":{}
        }
    }"#;

    fn configuration(character_revision: ContentHash) -> SessionConfiguration {
        SessionConfiguration {
            compatibility_profile: "sillytavern-1.18-core".to_owned(),
            character_revision,
            persona_name: "User".to_owned(),
            persona_description: None,
            lorebook_revisions: vec![],
            prompt_preset_revision: None,
            prompt_order_overrides: BTreeMap::new(),
            provider: ProviderSettings {
                id: "default".to_owned(),
                base_url: "https://127.0.0.1:3443".to_owned(),
                chat_completions_path: "/v1/chat/completions".to_owned(),
                api_key_env: None,
                credential_key: None,
                static_headers: BTreeMap::new(),
                timeout_seconds: 120,
                ca_certificate_pem: None,
                model: "fixture-model".to_owned(),
                stream: true,
                format_mode: Default::default(),
                completions_path: None,
                instruct_template: None,
                context_formatting: None,
            },
            tokenizer: "tiktoken:o200k_base".to_owned(),
            generation_settings: json!({"temperature": 1.0}),
            plugins: vec![],
            script_grants: vec![],
        }
    }

    #[test]
    fn session_recovers_pinned_configuration_and_greeting_after_restart() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let created = {
            let mut store = Store::open(&path).unwrap();
            let character = store.import_artifact(CARD.as_bytes()).unwrap();
            store
                .create_session(configuration(character.revision_hash), 1)
                .unwrap()
        };

        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.session(created.session.session_id).unwrap().unwrap(),
            created.session
        );
        assert_eq!(
            store.branch(created.branch.branch_id).unwrap().unwrap(),
            created.branch
        );
        assert_eq!(
            store
                .configuration(&created.configuration.revision_hash)
                .unwrap()
                .unwrap(),
            created.configuration
        );
    }

    #[test]
    fn projections_rebuild_from_authoritative_trace() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(path).unwrap();
        let character = store.import_artifact(CARD.as_bytes()).unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();
        store
            .connection
            .execute("DELETE FROM branches", [])
            .unwrap();
        store
            .connection
            .execute("DELETE FROM sessions", [])
            .unwrap();
        store
            .connection
            .execute("DELETE FROM session_config_revisions", [])
            .unwrap();

        store.rebuild_session_projections().unwrap();
        assert_eq!(
            store.session(created.session.session_id).unwrap().unwrap(),
            created.session
        );
        assert_eq!(
            store.branch(created.branch.branch_id).unwrap().unwrap(),
            created.branch
        );
    }

    #[test]
    fn rename_session_sets_clears_and_reads_custom_name() {
        // Regression test for session list rename: an empty name restores the default.
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store.import_artifact(CARD.as_bytes()).unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();

        store
            .rename_session(created.session.session_id, "Library visit")
            .unwrap();
        assert_eq!(
            store
                .session(created.session.session_id)
                .unwrap()
                .unwrap()
                .custom_name
                .as_deref(),
            Some("Library visit"),
        );

        store
            .rename_session(created.session.session_id, "   ")
            .unwrap();
        assert_eq!(
            store
                .session(created.session.session_id)
                .unwrap()
                .unwrap()
                .custom_name,
            None,
        );
    }
    #[test]
    fn root_branch_cannot_be_deleted() {
        // Regression test: tombstoning the root branch must not invalidate its session.
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store.import_artifact(CARD.as_bytes()).unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();

        let error = store.delete_branch(created.branch.branch_id).unwrap_err();

        assert!(matches!(
            error,

            SessionError::RootBranchDeletion(id) if id == created.branch.branch_id
        ));
        assert!(store.branch(created.branch.branch_id).unwrap().is_some());
    }
    #[test]
    fn opening_legacy_store_restores_deleted_root_branch() {
        // Regression test: TUI startup must repair root branches deleted by older releases.
        let directory = tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let character = store.import_artifact(CARD.as_bytes()).unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();
        let transaction = store.connection.transaction().unwrap();
        append_event(
            &transaction,
            Some(created.session.session_id),
            "branch.deleted",
            &json!({"branch_id": created.branch.branch_id}),
        )
        .unwrap();
        transaction
            .execute(
                "UPDATE branches SET deleted = 1 WHERE branch_id = ?1",
                [created.branch.branch_id.to_string()],
            )
            .unwrap();
        transaction
            .execute("DELETE FROM schema_migrations", [])
            .unwrap();
        transaction
            .execute("INSERT INTO schema_migrations(version) VALUES (8)", [])
            .unwrap();
        transaction.commit().unwrap();
        drop(store);

        let mut store = Store::open(&database).unwrap();
        assert!(store.branch(created.branch.branch_id).unwrap().is_some());
        assert!(matches!(
            crate::StcliEngine::new(&database)
                .inspect(crate::EngineQuery::Sessions)
                .unwrap(),
            crate::EngineInspection::Sessions(_)
        ));

        store.rebuild_session_projections().unwrap();
        assert!(store.branch(created.branch.branch_id).unwrap().is_some());
    }

    #[test]
    fn branch_can_select_an_alternate_greeting_without_changing_root() {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store.import_artifact(CARD.as_bytes()).unwrap();
        let created = store
            .create_session(configuration(character.revision_hash), 0)
            .unwrap();
        let branch = store
            .create_branch(created.session.session_id, created.branch.branch_id, 1)
            .unwrap();
        assert_eq!(created.branch.greeting, "Welcome.");
        assert_eq!(branch.greeting, "You came back.");
        assert_eq!(branch.parent_branch_id, Some(created.branch.branch_id));
    }
}
