use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use parking_lot::Mutex;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ActivatedLore, BranchProjection, CHAT_COMPLETION_CHARACTER_ID, ChatMessage, ChatRole,
    ContentHash, EcmaRegexError, EcmaRegexWorker, EntityId, FormatMode, LoreEngine, LoreError,
    LorePosition, LoreResult, LoreSettings, MacroContext, MacroEngine, MacroError, MacroEvaluation,
    MacroWarning, OpenAiProvider, PluginEffect, PluginError, PluginEvent, PluginGrant, PluginHost,
    PluginInput, PluginReceipt, PluginRegistry, PromptContribution, PromptError, PromptPreset,
    PromptPruning, PromptSegment, PromptSlot, ProviderError, ProviderEvent, ProviderResult,
    RegexPlacement, RegexScript, RegexScriptApplication, RenderedPromptContent,
    SessionConfigurationRecord, SessionError, StateError, StateMutation, StateTransaction, Store,
    TokenizerError, TokenizerId, apply_prompt_preset,
    artifact::ArtifactError,
    canonical_json, insert_in_chat_segments,
    lore::parse_lore_entries,
    order_plugins,
    prompt::prune_segments,
    provider_request, provider_request_hash, regex_script,
    regex_script::apply_scripts,
    state::{apply_plugin_command_state_mutations, apply_state_mutations},
    storage::{StorageError, append_event},
    text_completion::prune_text_completion,
};

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PromptPlan {
    pub tokenizer: TokenizerId,
    #[serde(default)]
    pub rng_seed: u64,
    pub segments: Vec<PromptSegment>,
    pub messages: Vec<ChatMessage>,
    #[serde(default, skip_serializing_if = "FormatMode::is_chat_completion")]
    pub format_mode: FormatMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stop_sequences: Vec<String>,
    pub total_tokens: usize,
    pub macro_evaluations: Vec<MacroEvaluation>,
    pub macro_warnings: Vec<MacroWarning>,
    pub state_mutations: Vec<StateMutation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub regex_applications: Vec<RegexScriptApplication>,
    #[serde(default)]
    pub plugin_receipts: Vec<PluginReceipt>,
    pub lore: LoreResult,
    pub generation_type: GenerationType,
    pub parent_candidate_id: Option<EntityId>,
    pub continuation_prefix: Option<String>,
    pub pruning: PromptPruning,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationSettingSource {
    Session,
    Preset,
    Profile,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EffectiveGenerationSettings {
    pub values: Value,
    pub provenance: BTreeMap<String, GenerationSettingSource>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompatibilityWarning {
    pub code: String,
    pub profile_id: String,
    pub non_blocking: bool,
    pub source_revision: ContentHash,
    pub affected_identifiers: Vec<String>,
    pub count: usize,
    pub detail: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ScriptSource {
    Preset,
    Character,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PresetScriptMetadata {
    pub digest: ContentHash,
    pub source: ScriptSource,
    pub enabled: bool,
    pub granted: bool,
    pub placement: Value,
    pub metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PresetTransformationResult {
    pub content: Value,
    pub scripts: Vec<PresetScriptMetadata>,
    pub warnings: Vec<CompatibilityWarning>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TurnPreparation {
    pub prompt_plan: PromptPlan,
    pub effective_generation_settings: EffectiveGenerationSettings,
    pub provider_request: Value,
    pub compatibility_warnings: Vec<CompatibilityWarning>,
    pub preset_transformations: Vec<PresetScriptMetadata>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum GenerationType {
    Normal,
    Continue,
    Regenerate,
    Swipe,
}

impl GenerationType {
    fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Continue => "continue",
            Self::Regenerate => "regenerate",
            Self::Swipe => "swipe",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttemptStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
    Incomplete,
}
impl AttemptStatus {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Incomplete => "incomplete",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CandidateOrigin {
    Generated,
    Continued,
    Manual,
    AcceptedPartial,
}

impl CandidateOrigin {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::Continued => "continued",
            Self::Manual => "manual",
            Self::AcceptedPartial => "accepted-partial",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TurnProjection {
    pub turn_id: EntityId,
    pub session_id: EntityId,
    pub branch_id: EntityId,
    pub user_content: String,
    pub selected_candidate_id: Option<EntityId>,
    #[serde(default)]
    pub hidden: bool,
    pub created_event_id: String,
}

#[derive(Clone, Debug)]
struct StoredTurn {
    projection: TurnProjection,
    deleted: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AttemptEffectReceipt {
    pub rng_seed: u64,
    pub clock_outcomes: Vec<Value>,
    pub lore: LoreResult,
    pub macro_evaluations: Vec<MacroEvaluation>,
    pub macro_warnings: Vec<MacroWarning>,
    pub plugins: Vec<PluginReceipt>,
    #[serde(default = "empty_effective_generation_settings")]
    pub effective_generation_settings: EffectiveGenerationSettings,
    #[serde(default)]
    pub compatibility_warnings: Vec<CompatibilityWarning>,
    #[serde(default)]
    pub preset_transformations: Vec<PresetScriptMetadata>,
    pub state_mutations: Vec<StateMutation>,
    pub provider_request: Value,
    pub provider_request_hash: ContentHash,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginCommandResult {
    pub command_execution_id: EntityId,
    pub session_id: EntityId,
    pub plugin_id: String,
    pub command: String,
    pub receipt: PluginReceipt,
    pub state_mutations: Vec<StateMutation>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct AttemptProjection {
    pub attempt_id: EntityId,
    pub turn_id: EntityId,
    pub config_hash: ContentHash,
    pub retry_of_attempt_id: Option<EntityId>,
    pub status: AttemptStatus,
    pub prompt_plan: PromptPlan,
    pub provider_request_hash: Option<ContentHash>,
    pub provider_receipt: Option<Value>,
    pub effect_receipt: Option<AttemptEffectReceipt>,
    pub error_message: Option<String>,
    pub created_event_id: String,
    pub completed_event_id: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CandidateProjection {
    pub candidate_id: EntityId,
    pub turn_id: EntityId,
    pub attempt_id: Option<EntityId>,
    pub parent_candidate_id: Option<EntityId>,
    pub origin: CandidateOrigin,
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered_content: Option<String>,
    #[serde(default)]
    pub hidden: bool,
    pub created_event_id: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct DryRunResult {
    pub session_id: EntityId,
    pub branch_id: EntityId,
    pub user_content: String,
    pub prompt_plan: PromptPlan,
    pub effective_generation_settings: EffectiveGenerationSettings,
    pub compatibility_warnings: Vec<CompatibilityWarning>,
    pub preset_transformations: Vec<PresetScriptMetadata>,
    pub provider_request: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CompletedTurn {
    pub turn: TurnProjection,
    pub attempt: AttemptProjection,
    pub candidate: CandidateProjection,
    pub provider_events: Vec<ProviderEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scrubbed_reasoning: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct FailedTurn {
    pub turn: TurnProjection,
    pub attempt: AttemptProjection,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EditedCandidate {
    pub branch: BranchProjection,
    pub turn: TurnProjection,
    pub candidate: CandidateProjection,
}

impl Store {
    #[allow(clippy::too_many_arguments)]
    fn prepare_turn(
        &self,
        session_id: EntityId,
        branch_id: EntityId,
        configuration: &SessionConfigurationRecord,
        user_content: &str,
        generation_type: GenerationType,
        excluded_turn_id: Option<EntityId>,
        parent_candidate: Option<(EntityId, String)>,
    ) -> Result<TurnPreparation, TurnError> {
        let preset = configuration
            .configuration
            .prompt_preset_revision
            .as_ref()
            .map(|revision| self.decoded_artifact(revision))
            .transpose()?;
        let preset_transformation = preset
            .as_ref()
            .zip(configuration.configuration.prompt_preset_revision.as_ref())
            .map(|(artifact, revision)| {
                transform_preset_content(
                    &configuration.configuration.compatibility_profile,
                    revision,
                    &artifact.semantic,
                    &configuration.configuration.script_grants,
                )
            });
        let character = self.decoded_artifact(&configuration.configuration.character_revision)?;
        let character_scripts = extract_character_scripts(
            &character.semantic,
            &configuration.configuration.script_grants,
        );
        let preset_value = preset_transformation
            .as_ref()
            .map(|transformation| &transformation.content);
        let mut all_script_metadata = preset_transformation
            .as_ref()
            .map(|transformation| transformation.scripts.clone())
            .unwrap_or_default();
        all_script_metadata.extend(character_scripts);
        let regex_scripts = granted_regex_scripts(&all_script_metadata);
        let effective_generation_settings =
            resolve_effective_generation_settings(configuration, preset_value);
        let effective = effective_generation_settings
            .values
            .as_object()
            .expect("effective settings are an object");
        let text_prefill =
            if configuration.configuration.provider.format_mode == FormatMode::TextCompletion {
                if generation_type == GenerationType::Continue
                    && effective
                        .get("continue_prefill")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                {
                    parent_candidate
                        .as_ref()
                        .map(|(_, content)| content.as_str())
                } else if generation_type != GenerationType::Continue {
                    effective
                        .get("assistant_prefill")
                        .and_then(Value::as_str)
                        .filter(|content| !content.is_empty())
                } else {
                    None
                }
            } else {
                None
            };
        let mut prompt_plan = self.build_prompt_plan(
            session_id,
            branch_id,
            configuration,
            &character,
            user_content,
            generation_type,
            excluded_turn_id,
            preset_value,
            &effective_generation_settings,
            &regex_scripts,
            text_prefill,
        )?;
        if let Some((candidate_id, content)) = parent_candidate {
            prompt_plan.parent_candidate_id = Some(candidate_id);
            prompt_plan.continuation_prefix = Some(content);
        }
        let message_count = prompt_plan.messages.len();
        if configuration.configuration.provider.format_mode == FormatMode::ChatCompletion {
            if generation_type == GenerationType::Continue
                && effective
                    .get("continue_prefill")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                if let Some(content) = prompt_plan.continuation_prefix.clone() {
                    prompt_plan.messages.push(ChatMessage {
                        role: ChatRole::Assistant,
                        content,
                    });
                }
            } else if generation_type != GenerationType::Continue
                && let Some(content) = effective.get("assistant_prefill").and_then(Value::as_str)
                && !content.is_empty()
            {
                prompt_plan.messages.push(ChatMessage {
                    role: ChatRole::Assistant,
                    content: content.to_owned(),
                });
            }
        }
        let provider_settings = provider_generation_settings(&effective_generation_settings);
        let request = provider_request(
            &configuration.configuration.provider,
            &prompt_plan,
            &provider_settings,
        );
        prompt_plan.messages.truncate(message_count);
        let provider_request = request?;
        let mut compatibility_warnings = preset_transformation
            .map(|transformation| transformation.warnings)
            .unwrap_or_default();
        if let Some(artifact) = preset.as_ref()
            && let Some(revision) = configuration.configuration.prompt_preset_revision.as_ref()
        {
            let disabled_markers = disabled_structural_markers(
                &artifact.semantic,
                &configuration.configuration.prompt_order_overrides,
            );
            if !disabled_markers.is_empty() {
                compatibility_warnings.push(CompatibilityWarning {
                    code: "structural-prompt-marker-disabled".to_owned(),
                    profile_id: configuration.configuration.compatibility_profile.clone(),
                    non_blocking: true,
                    source_revision: revision.clone(),
                    count: disabled_markers.len(),
                    affected_identifiers: disabled_markers,
                    detail: "A structural Prompt Order Entry is disabled; prompt assembly remains permissive."
                        .to_owned(),
                });
            }
        }
        let ungranted_character = all_script_metadata
            .iter()
            .filter(|script| {
                script.source == ScriptSource::Character && script.enabled && !script.granted
            })
            .collect::<Vec<_>>();
        if !ungranted_character.is_empty() {
            compatibility_warnings.push(CompatibilityWarning {
                code: "script-ungranted".to_owned(),
                profile_id: configuration.configuration.compatibility_profile.clone(),
                non_blocking: true,
                source_revision: configuration.configuration.character_revision.clone(),
                affected_identifiers: ungranted_character
                    .iter()
                    .map(|script| script.digest.to_string())
                    .collect(),
                count: ungranted_character.len(),
                detail: "Enabled character card scripts have no matching script grant.".to_owned(),
            });
        }
        let slash_command_scripts = all_script_metadata
            .iter()
            .filter(|script| {
                script.enabled
                    && script.granted
                    && script
                        .placement
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_u64)
                        .any(|code| code == 3)
            })
            .collect::<Vec<_>>();
        if !slash_command_scripts.is_empty() {
            compatibility_warnings.push(CompatibilityWarning {
                code: "placement-slash-command-unsupported".to_owned(),
                profile_id: configuration.configuration.compatibility_profile.clone(),
                non_blocking: true,
                source_revision: configuration.configuration.character_revision.clone(),
                affected_identifiers: slash_command_scripts
                    .iter()
                    .map(|script| script.digest.to_string())
                    .collect(),
                count: slash_command_scripts.len(),
                detail: "Placement SlashCommand (3) is not supported until v0.6.".to_owned(),
            });
        }
        Ok(TurnPreparation {
            prompt_plan,
            effective_generation_settings,
            provider_request,
            compatibility_warnings,
            preset_transformations: all_script_metadata,
        })
    }

    fn dry_run_result(
        session_id: EntityId,
        branch_id: EntityId,
        user_content: String,
        preparation: TurnPreparation,
    ) -> DryRunResult {
        DryRunResult {
            session_id,
            branch_id,
            user_content,
            prompt_plan: preparation.prompt_plan,
            effective_generation_settings: preparation.effective_generation_settings,
            compatibility_warnings: preparation.compatibility_warnings,
            preset_transformations: preparation.preset_transformations,
            provider_request: preparation.provider_request,
        }
    }

    pub fn dry_run_message(
        &self,
        session_id: EntityId,
        branch_id: EntityId,
        user_content: &str,
    ) -> Result<DryRunResult, TurnError> {
        check_content_size(user_content)?;
        let (_, configuration) = self.session_configuration(session_id)?;
        let preparation = self.prepare_turn(
            session_id,
            branch_id,
            &configuration,
            user_content,
            GenerationType::Normal,
            None,
            None,
        )?;
        Ok(Self::dry_run_result(
            session_id,
            branch_id,
            user_content.to_owned(),
            preparation,
        ))
    }

    pub async fn send_message(
        &mut self,
        session_id: EntityId,
        branch_id: EntityId,
        user_content: String,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        check_content_size(&user_content)?;
        let (config_hash, configuration) = self.session_configuration(session_id)?;
        let preparation = self.prepare_turn(
            session_id,
            branch_id,
            &configuration,
            &user_content,
            GenerationType::Normal,
            None,
            None,
        )?;
        let (turn, attempt) = self.begin_turn(
            session_id,
            branch_id,
            user_content,
            config_hash,
            None,
            preparation,
        )?;
        self.execute_attempt(turn, attempt, configuration, &mut on_event)
            .await
    }

    pub async fn retry_turn(
        &mut self,
        turn_id: EntityId,
        retry_of_attempt_id: EntityId,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let previous = self
            .attempt(retry_of_attempt_id)?
            .ok_or(TurnError::AttemptNotFound(retry_of_attempt_id))?;
        if previous.turn_id != turn_id {
            return Err(TurnError::RetryAttemptMismatch);
        }
        let configuration = self
            .configuration(&previous.config_hash)?
            .ok_or_else(|| TurnError::ConfigurationNotFound(previous.config_hash.clone()))?;
        let parent_candidate = previous
            .prompt_plan
            .parent_candidate_id
            .zip(previous.prompt_plan.continuation_prefix.clone());
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            if previous.prompt_plan.generation_type == GenerationType::Continue {
                ""
            } else {
                &turn.user_content
            },
            previous.prompt_plan.generation_type,
            (previous.prompt_plan.generation_type != GenerationType::Continue)
                .then_some(turn.turn_id),
            parent_candidate,
        )?;
        let attempt = self.begin_attempt(
            &turn,
            previous.config_hash,
            Some(retry_of_attempt_id),
            preparation,
        )?;
        self.execute_attempt(turn, attempt, configuration, &mut on_event)
            .await
    }

    pub fn dry_run_rerun(&self, attempt_id: EntityId) -> Result<DryRunResult, TurnError> {
        let attempt = self
            .attempt(attempt_id)?
            .ok_or(TurnError::AttemptNotFound(attempt_id))?;
        let turn = self
            .turn(attempt.turn_id)?
            .ok_or(TurnError::TurnNotFound(attempt.turn_id))?;
        let effect = attempt
            .effect_receipt
            .ok_or(TurnError::AttemptEffectReceiptMissing(attempt_id))?;
        Ok(DryRunResult {
            session_id: turn.session_id,
            branch_id: turn.branch_id,
            user_content: turn.user_content,
            prompt_plan: attempt.prompt_plan,
            effective_generation_settings: effect.effective_generation_settings,
            compatibility_warnings: effect.compatibility_warnings,
            preset_transformations: effect.preset_transformations,
            provider_request: effect.provider_request,
        })
    }

    pub async fn rerun_attempt(
        &mut self,
        attempt_id: EntityId,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        let previous = self
            .attempt(attempt_id)?
            .ok_or(TurnError::AttemptNotFound(attempt_id))?;
        if previous.status == AttemptStatus::Running {
            return Err(TurnError::AttemptStillRunning(attempt_id));
        }
        let turn = self
            .turn(previous.turn_id)?
            .ok_or(TurnError::TurnNotFound(previous.turn_id))?;
        let configuration = self
            .configuration(&previous.config_hash)?
            .ok_or_else(|| TurnError::ConfigurationNotFound(previous.config_hash.clone()))?;
        let effect = previous
            .effect_receipt
            .ok_or(TurnError::AttemptEffectReceiptMissing(attempt_id))?;
        let preparation = TurnPreparation {
            prompt_plan: previous.prompt_plan,
            effective_generation_settings: effect.effective_generation_settings,
            provider_request: effect.provider_request,
            compatibility_warnings: effect.compatibility_warnings,
            preset_transformations: effect.preset_transformations,
        };
        let attempt =
            self.begin_attempt(&turn, previous.config_hash, Some(attempt_id), preparation)?;
        self.execute_attempt(turn, attempt, configuration, &mut on_event)
            .await
    }

    pub fn dry_run_regenerate(&self, turn_id: EntityId) -> Result<DryRunResult, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let (_, configuration) = self.session_configuration(turn.session_id)?;
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            &turn.user_content,
            GenerationType::Regenerate,
            Some(turn_id),
            None,
        )?;
        Ok(Self::dry_run_result(
            turn.session_id,
            turn.branch_id,
            turn.user_content,
            preparation,
        ))
    }

    pub fn dry_run_swipe(&self, turn_id: EntityId) -> Result<DryRunResult, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let (_, configuration) = self.session_configuration(turn.session_id)?;
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            &turn.user_content,
            GenerationType::Swipe,
            Some(turn_id),
            None,
        )?;
        Ok(Self::dry_run_result(
            turn.session_id,
            turn.branch_id,
            turn.user_content,
            preparation,
        ))
    }

    pub fn dry_run_continue(&self, turn_id: EntityId) -> Result<DryRunResult, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let parent_id = turn
            .selected_candidate_id
            .ok_or(TurnError::TurnHasNoSelection(turn_id))?;
        let parent = self
            .candidate(parent_id)?
            .ok_or(TurnError::CandidateNotFound(parent_id))?;
        let (_, configuration) = self.session_configuration(turn.session_id)?;
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            "",
            GenerationType::Continue,
            None,
            Some((parent_id, parent.content)),
        )?;
        Ok(Self::dry_run_result(
            turn.session_id,
            turn.branch_id,
            String::new(),
            preparation,
        ))
    }

    pub async fn regenerate_turn(
        &mut self,
        turn_id: EntityId,
        on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        self.generate_alternative(turn_id, GenerationType::Regenerate, on_event)
            .await
    }

    pub async fn swipe_turn(
        &mut self,
        turn_id: EntityId,
        on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        self.generate_alternative(turn_id, GenerationType::Swipe, on_event)
            .await
    }

    pub async fn continue_turn(
        &mut self,
        turn_id: EntityId,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let parent_id = turn
            .selected_candidate_id
            .ok_or(TurnError::TurnHasNoSelection(turn_id))?;
        let parent = self
            .candidate(parent_id)?
            .ok_or(TurnError::CandidateNotFound(parent_id))?;
        let (config_hash, configuration) = self.session_configuration(turn.session_id)?;
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            "",
            GenerationType::Continue,
            None,
            Some((parent_id, parent.content)),
        )?;
        let attempt = self.begin_attempt(&turn, config_hash, None, preparation)?;
        self.execute_attempt(turn, attempt, configuration, &mut on_event)
            .await
    }

    pub fn select_swipe(
        &mut self,
        turn_id: EntityId,
        candidate_id: EntityId,
    ) -> Result<TurnProjection, TurnError> {
        let mut turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let candidate = self
            .candidate(candidate_id)?
            .ok_or(TurnError::CandidateNotFound(candidate_id))?;
        if candidate.turn_id != turn_id {
            return Err(TurnError::CandidateTurnMismatch);
        }
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(turn.session_id),
            "turn.candidate-selected",
            &json!({
                "turn_id": turn_id,
                "candidate_id": candidate_id,
            }),
        )?;
        transaction
            .execute(
                "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                params![candidate_id.to_string(), turn_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        turn.selected_candidate_id = Some(candidate_id);
        Ok(turn)
    }
    pub async fn edit_user_turn(
        &mut self,
        turn_id: EntityId,
        user_content: String,
        on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        check_content_size(&user_content)?;
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let parent = self
            .branch(turn.branch_id)?
            .ok_or(TurnError::BranchNotFound(turn.branch_id))?;
        let branch = self.create_branch_at(
            turn.session_id,
            turn.branch_id,
            Some(turn_id),
            parent.greeting_index,
        )?;
        self.send_message(turn.session_id, branch.branch_id, user_content, on_event)
            .await
    }

    pub fn edit_candidate(
        &mut self,
        candidate_id: EntityId,
        content: String,
    ) -> Result<EditedCandidate, TurnError> {
        check_content_size(&content)?;
        let parent_candidate = self
            .candidate(candidate_id)?
            .ok_or(TurnError::CandidateNotFound(candidate_id))?;
        let parent_turn = self
            .turn(parent_candidate.turn_id)?
            .ok_or(TurnError::TurnNotFound(parent_candidate.turn_id))?;
        let parent_branch = self
            .branch(parent_turn.branch_id)?
            .ok_or(TurnError::BranchNotFound(parent_turn.branch_id))?;
        let branch = self.create_branch_at(
            parent_turn.session_id,
            parent_turn.branch_id,
            Some(parent_turn.turn_id),
            parent_branch.greeting_index,
        )?;
        let turn_id = EntityId::new();
        let manual_candidate_id = EntityId::new();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let turn_event = append_event(
            &transaction,
            Some(parent_turn.session_id),
            "turn.created",
            &json!({
                "turn_id": turn_id,
                "branch_id": branch.branch_id,
                "user_content": parent_turn.user_content,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                params![
                    turn_id.to_string(),
                    parent_turn.session_id.to_string(),
                    branch.branch_id.to_string(),
                    parent_turn.user_content,
                    turn_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        let candidate_event = append_event(
            &transaction,
            Some(parent_turn.session_id),
            "candidate.manual-created",
            &json!({
                "candidate_id": manual_candidate_id,
                "turn_id": turn_id,
                "parent_candidate_id": candidate_id,
                "content": content,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, NULL, ?3, 'manual', ?4, ?5)",
                params![
                    manual_candidate_id.to_string(),
                    turn_id.to_string(),
                    candidate_id.to_string(),
                    content,
                    candidate_event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                params![manual_candidate_id.to_string(), turn_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        let branch_id = branch.branch_id;
        Ok(EditedCandidate {
            branch,
            turn: TurnProjection {
                turn_id,
                session_id: parent_turn.session_id,
                branch_id,
                user_content: parent_turn.user_content,
                selected_candidate_id: Some(manual_candidate_id),
                hidden: false,
                created_event_id: turn_event.event_id.to_string(),
            },
            candidate: CandidateProjection {
                candidate_id: manual_candidate_id,
                turn_id,
                attempt_id: None,
                parent_candidate_id: Some(candidate_id),
                origin: CandidateOrigin::Manual,
                content,
                rendered_content: None,
                hidden: false,
                created_event_id: candidate_event.event_id.to_string(),
            },
        })
    }

    async fn generate_alternative(
        &mut self,
        turn_id: EntityId,
        generation_type: GenerationType,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        let turn = self
            .turn(turn_id)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let (config_hash, configuration) = self.session_configuration(turn.session_id)?;
        let preparation = self.prepare_turn(
            turn.session_id,
            turn.branch_id,
            &configuration,
            &turn.user_content,
            generation_type,
            Some(turn_id),
            None,
        )?;
        let attempt = self.begin_attempt(&turn, config_hash, None, preparation)?;
        self.execute_attempt(turn, attempt, configuration, &mut on_event)
            .await
    }

    pub fn turn(&self, turn_id: EntityId) -> Result<Option<TurnProjection>, TurnError> {
        self.connection
            .query_row(
                "SELECT turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id, hidden FROM turns WHERE turn_id = ?1 AND deleted = 0",
                [turn_id.to_string()],
                decode_turn,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(TurnError::Storage)
    }

    pub fn hide_turn(&mut self, turn_id: EntityId) -> Result<TurnProjection, TurnError> {
        let (session_id, hidden) = self
            .connection
            .query_row(
                "SELECT session_id, hidden FROM turns WHERE turn_id = ?1 AND deleted = 0",
                [turn_id.to_string()],
                |row| Ok((parse_column(row, 0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(StorageError::Sqlite)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        if !hidden {
            let transaction = self
                .connection
                .transaction()
                .map_err(StorageError::Sqlite)?;
            append_event(
                &transaction,
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
            transaction.commit().map_err(StorageError::Sqlite)?;
        }
        self.turn(turn_id)?.ok_or(TurnError::TurnNotFound(turn_id))
    }

    pub fn delete_turn(&mut self, turn_id: EntityId) -> Result<(), TurnError> {
        let session_id = self
            .connection
            .query_row(
                "SELECT session_id FROM turns WHERE turn_id = ?1 AND deleted = 0",
                [turn_id.to_string()],
                |row| parse_column(row, 0),
            )
            .optional()
            .map_err(StorageError::Sqlite)?
            .ok_or(TurnError::TurnNotFound(turn_id))?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
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
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn attempt(&self, attempt_id: EntityId) -> Result<Option<AttemptProjection>, TurnError> {
        self.connection
            .query_row(
                "SELECT attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, provider_receipt, effect_receipt, error_message, created_event_id, completed_event_id FROM attempts WHERE attempt_id = ?1",
                [attempt_id.to_string()],
                decode_attempt,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(TurnError::Storage)
    }

    pub fn cancel_attempt(&mut self, attempt_id: EntityId) -> Result<AttemptProjection, TurnError> {
        let mut attempt = self
            .attempt(attempt_id)?
            .ok_or(TurnError::AttemptNotFound(attempt_id))?;
        if attempt.status != AttemptStatus::Running {
            return Err(TurnError::AttemptNotRunning {
                attempt_id,
                status: attempt.status,
            });
        }
        let turn = self
            .turn(attempt.turn_id)?
            .ok_or(TurnError::TurnNotFound(attempt.turn_id))?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(turn.session_id),
            "attempt.cancelled",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn.turn_id,
            }),
        )?;
        let updated = transaction
            .execute(
                "UPDATE attempts SET status = ?1, completed_event_id = ?2 WHERE attempt_id = ?3 AND status = 'running'",
                params![
                    AttemptStatus::Cancelled.as_str(),
                    event.event_id.to_string(),
                    attempt_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        if updated == 0 {
            transaction.rollback().map_err(StorageError::Sqlite)?;
            return Err(self.attempt_not_running(attempt_id)?);
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        attempt.status = AttemptStatus::Cancelled;
        attempt.completed_event_id = Some(event.event_id.to_string());
        Ok(attempt)
    }

    pub fn candidate(
        &self,
        candidate_id: EntityId,
    ) -> Result<Option<CandidateProjection>, TurnError> {
        self.connection
            .query_row(
                "SELECT candidates.candidate_id, candidates.turn_id, candidates.attempt_id, candidates.parent_candidate_id, candidates.origin, candidates.content, candidates.created_event_id, candidates.hidden FROM candidates JOIN turns ON turns.turn_id = candidates.turn_id WHERE candidates.candidate_id = ?1 AND candidates.deleted = 0 AND turns.deleted = 0",
                [candidate_id.to_string()],
                decode_candidate,
            )
            .optional()
            .map_err(StorageError::Sqlite)
            .map_err(TurnError::Storage)
    }

    pub fn hide_candidate(
        &mut self,
        candidate_id: EntityId,
    ) -> Result<CandidateProjection, TurnError> {
        let (session_id, hidden) = self
            .connection
            .query_row(
                "SELECT turns.session_id, candidates.hidden FROM candidates JOIN turns ON turns.turn_id = candidates.turn_id WHERE candidates.candidate_id = ?1 AND candidates.deleted = 0 AND turns.deleted = 0",
                [candidate_id.to_string()],
                |row| Ok((parse_column(row, 0)?, row.get::<_, i64>(1)? != 0)),
            )
            .optional()
            .map_err(StorageError::Sqlite)?
            .ok_or(TurnError::CandidateNotFound(candidate_id))?;
        if !hidden {
            let transaction = self
                .connection
                .transaction()
                .map_err(StorageError::Sqlite)?;
            append_event(
                &transaction,
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
            transaction.commit().map_err(StorageError::Sqlite)?;
        }
        self.candidate(candidate_id)?
            .ok_or(TurnError::CandidateNotFound(candidate_id))
    }

    pub fn delete_candidate(&mut self, candidate_id: EntityId) -> Result<(), TurnError> {
        let (session_id, turn_id): (EntityId, EntityId) = self
            .connection
            .query_row(
                "SELECT turns.session_id, candidates.turn_id FROM candidates JOIN turns ON turns.turn_id = candidates.turn_id WHERE candidates.candidate_id = ?1 AND candidates.deleted = 0 AND turns.deleted = 0",
                [candidate_id.to_string()],
                |row| Ok((parse_column(row, 0)?, parse_column(row, 1)?)),
            )
            .optional()
            .map_err(StorageError::Sqlite)?
            .ok_or(TurnError::CandidateNotFound(candidate_id))?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
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
        transaction
            .execute(
                "UPDATE turns SET selected_candidate_id = (
                    SELECT candidate_id FROM candidates
                    WHERE turn_id = ?1 AND deleted = 0
                    ORDER BY rowid LIMIT 1
                ) WHERE turn_id = ?1 AND selected_candidate_id = ?2",
                params![turn_id.to_string(), candidate_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(())
    }

    pub fn candidates_for_turn(
        &self,
        turn_id: EntityId,
    ) -> Result<Vec<CandidateProjection>, TurnError> {
        let mut statement = self
            .connection
            .prepare("SELECT candidates.candidate_id, candidates.turn_id, candidates.attempt_id, candidates.parent_candidate_id, candidates.origin, candidates.content, candidates.created_event_id, candidates.hidden FROM candidates JOIN turns ON turns.turn_id = candidates.turn_id WHERE candidates.turn_id = ?1 AND candidates.deleted = 0 AND turns.deleted = 0 ORDER BY candidates.rowid")
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([turn_id.to_string()], decode_candidate)
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(TurnError::Storage)
    }

    pub fn attempts_for_turn(
        &self,
        turn_id: EntityId,
    ) -> Result<Vec<AttemptProjection>, TurnError> {
        let mut statement = self
            .connection
            .prepare("SELECT attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, provider_receipt, effect_receipt, error_message, created_event_id, completed_event_id FROM attempts WHERE turn_id = ?1 ORDER BY rowid")
            .map_err(StorageError::Sqlite)?;
        statement
            .query_map([turn_id.to_string()], decode_attempt)
            .map_err(StorageError::Sqlite)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StorageError::Sqlite)
            .map_err(TurnError::Storage)
    }

    pub fn turns_for_branch(&self, branch_id: EntityId) -> Result<Vec<TurnProjection>, TurnError> {
        self.branch(branch_id)?
            .ok_or(TurnError::BranchNotFound(branch_id))?;
        Ok(self
            .turns_for_branch_inner(branch_id, &mut BTreeSet::new())?
            .into_iter()
            .filter(|turn| !turn.deleted)
            .map(|turn| turn.projection)
            .collect())
    }

    fn turns_for_branch_inner(
        &self,
        branch_id: EntityId,
        visited: &mut BTreeSet<EntityId>,
    ) -> Result<Vec<StoredTurn>, TurnError> {
        if !visited.insert(branch_id) {
            return Err(TurnError::BranchCycle(branch_id));
        }
        if visited.len() > crate::limits::MAX_BRANCH_DEPTH {
            return Err(TurnError::BranchTooDeep {
                depth: visited.len(),
                limit: crate::limits::MAX_BRANCH_DEPTH,
            });
        }
        let (parent_branch_id, forked_from_turn_id) = self
            .connection
            .query_row(
                "SELECT parent_branch_id, forked_from_turn_id FROM branches WHERE branch_id = ?1",
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
            .ok_or(TurnError::BranchNotFound(branch_id))?;
        let mut turns = if let (Some(parent), Some(fork)) = (parent_branch_id, forked_from_turn_id)
        {
            let mut inherited = self.turns_for_branch_inner(parent, visited)?;
            let cutoff = inherited
                .iter()
                .position(|turn| turn.projection.turn_id == fork)
                .ok_or(TurnError::ForkTurnNotFound(fork))?;
            inherited.truncate(cutoff);
            inherited
        } else {
            Vec::new()
        };
        let mut statement = self
            .connection
            .prepare("SELECT turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id, hidden, deleted FROM turns WHERE branch_id = ?1 ORDER BY rowid")
            .map_err(StorageError::Sqlite)?;
        turns.extend(
            statement
                .query_map([branch_id.to_string()], |row| {
                    Ok(StoredTurn {
                        projection: decode_turn(row)?,
                        deleted: row.get::<_, i64>(7)? != 0,
                    })
                })
                .map_err(StorageError::Sqlite)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(StorageError::Sqlite)?,
        );
        visited.remove(&branch_id);
        Ok(turns)
    }

    fn session_configuration(
        &self,
        session_id: EntityId,
    ) -> Result<(ContentHash, SessionConfigurationRecord), TurnError> {
        let session = self
            .session(session_id)?
            .ok_or(TurnError::SessionNotFound(session_id))?;
        let configuration = self
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| TurnError::ConfigurationNotFound(session.current_config_hash.clone()))?;
        Ok((session.current_config_hash, configuration))
    }

    fn run_runtime_plugins(
        &self,
        configuration: &SessionConfigurationRecord,
        session_id: EntityId,
        branch_id: EntityId,
        generation_type: GenerationType,
        context: &mut MacroContext,
        state: &mut crate::StateTransaction,
    ) -> Result<Vec<PluginReceipt>, TurnError> {
        if configuration.configuration.plugins.is_empty() {
            return Ok(Vec::new());
        }
        let registry = PluginRegistry::new(
            self.path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("plugins"),
        );
        let mut selected = Vec::new();
        let mut grants = BTreeMap::new();
        for pin in configuration
            .configuration
            .plugins
            .iter()
            .filter(|pin| pin.enabled)
        {
            let version = pin
                .version
                .parse()
                .map_err(|_| TurnError::PluginVersion(pin.version.clone()))?;
            let installed = registry
                .find_pinned(&pin.id, &version, &pin.component_hash)?
                .ok_or_else(|| TurnError::PluginNotInstalled(pin.id.clone()))?;
            grants.insert(
                pin.id.clone(),
                PluginGrant {
                    id: pin.id.clone(),
                    version,
                    component_sha256: pin.component_hash.clone(),
                    capabilities: pin.capabilities.clone(),
                    settings: pin.settings.clone(),
                    enabled: true,
                },
            );
            selected.push(installed);
        }
        let ordered = order_plugins(&selected)?;
        let host = PluginHost::new(Default::default());
        let mut receipts = Vec::new();
        for event in [
            PluginEvent::PreLore,
            PluginEvent::PrePrompt,
            PluginEvent::PreRequest,
        ] {
            for installed in &ordered {
                if !installed.manifest.subscriptions.contains(&event) {
                    continue;
                }
                let grant = &grants[&installed.manifest.id];
                let session = if grant
                    .capabilities
                    .contains(&crate::PluginCapability::ReadSession)
                {
                    json!({
                        "session_id": session_id,
                        "branch_id": branch_id,
                        "generation_type": generation_type,
                    })
                } else {
                    Value::Null
                };
                let receipt = host.execute(
                    installed,
                    grant,
                    PluginInput {
                        event,
                        plugin_id: installed.manifest.id.clone(),
                        settings: grant.settings.clone(),
                        context: Value::Null,
                        artifact: Value::Null,
                        state: Value::Object(
                            state
                                .local_namespace(&installed.manifest.id)
                                .into_iter()
                                .collect(),
                        ),
                        session,
                    },
                )?;
                for effect in &receipt.effects {
                    match effect {
                        PluginEffect::RegisterMacro { name, value } => {
                            context.register(name, value);
                        }
                        PluginEffect::StateWrite { key, value } => {
                            state.set(
                                key.scope,
                                &key.name,
                                value.clone(),
                                &installed.manifest.id,
                                "runtime-plugin",
                            );
                        }
                        PluginEffect::Abort { code, message } => {
                            return Err(TurnError::PluginAbort {
                                id: installed.manifest.id.clone(),
                                code: code.clone(),
                                message: message.clone(),
                            });
                        }
                        PluginEffect::Observe { .. }
                        | PluginEffect::Output { .. }
                        | PluginEffect::RegisterCommand { .. }
                        | PluginEffect::Prompt { .. } => {}
                    }
                }
                context.plugins.insert(installed.manifest.id.clone());
                receipts.push(receipt);
            }
        }
        Ok(receipts)
    }
    fn run_post_commit_plugins(
        &self,
        attempt: &AttemptProjection,
        session_id: EntityId,
        branch_id: EntityId,
        content: &str,
    ) -> Result<Vec<PluginReceipt>, TurnError> {
        let configuration = self
            .configuration(&attempt.config_hash)?
            .ok_or_else(|| TurnError::ConfigurationNotFound(attempt.config_hash.clone()))?;
        if configuration.configuration.plugins.is_empty() {
            return Ok(Vec::new());
        }
        let registry = PluginRegistry::new(
            self.path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("plugins"),
        );
        let mut selected = Vec::new();
        let mut grants = BTreeMap::new();
        for pin in configuration
            .configuration
            .plugins
            .iter()
            .filter(|pin| pin.enabled)
        {
            let version = pin
                .version
                .parse()
                .map_err(|_| TurnError::PluginVersion(pin.version.clone()))?;
            let installed = registry
                .find_pinned(&pin.id, &version, &pin.component_hash)?
                .ok_or_else(|| TurnError::PluginNotInstalled(pin.id.clone()))?;
            grants.insert(
                pin.id.clone(),
                PluginGrant {
                    id: pin.id.clone(),
                    version,
                    component_sha256: pin.component_hash.clone(),
                    capabilities: pin.capabilities.clone(),
                    settings: pin.settings.clone(),
                    enabled: true,
                },
            );
            selected.push(installed);
        }
        let host = PluginHost::new(Default::default());
        let mut receipts = Vec::new();
        let state = self.state_transaction(session_id)?;
        for installed in order_plugins(&selected)? {
            if !installed
                .manifest
                .subscriptions
                .contains(&PluginEvent::PostCommit)
            {
                continue;
            }
            let grant = &grants[&installed.manifest.id];
            let session = if grant
                .capabilities
                .contains(&crate::PluginCapability::ReadSession)
            {
                json!({
                    "session_id": session_id,
                    "branch_id": branch_id,
                    "attempt_id": attempt.attempt_id,
                    "content": content,
                })
            } else {
                Value::Null
            };
            receipts.push(
                host.execute(
                    &installed,
                    grant,
                    PluginInput {
                        event: PluginEvent::PostCommit,
                        plugin_id: installed.manifest.id.clone(),
                        settings: grant.settings.clone(),
                        context: Value::Null,
                        artifact: Value::Null,
                        state: Value::Object(
                            state
                                .local_namespace(&installed.manifest.id)
                                .into_iter()
                                .collect(),
                        ),
                        session,
                    },
                )?,
            );
        }
        Ok(receipts)
    }
    pub fn invoke_plugin_command(
        &mut self,
        session_id: EntityId,
        plugin_id: &str,
        command: &str,
        arguments: Value,
    ) -> Result<PluginCommandResult, TurnError> {
        let session = self
            .session(session_id)?
            .ok_or(TurnError::SessionNotFound(session_id))?;
        let configuration = self
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| TurnError::ConfigurationNotFound(session.current_config_hash.clone()))?;
        let pin = configuration
            .configuration
            .plugins
            .iter()
            .find(|pin| pin.enabled && pin.id == plugin_id)
            .ok_or_else(|| TurnError::PluginNotInstalled(plugin_id.to_owned()))?;
        let registry = PluginRegistry::new(
            self.path()
                .parent()
                .unwrap_or_else(|| std::path::Path::new("."))
                .join("plugins"),
        );
        let version = pin
            .version
            .parse()
            .map_err(|_| TurnError::PluginVersion(pin.version.clone()))?;
        let installed = registry
            .find_pinned(plugin_id, &version, &pin.component_hash)?
            .ok_or_else(|| TurnError::PluginNotInstalled(plugin_id.to_owned()))?;
        if !pin
            .capabilities
            .contains(&crate::PluginCapability::RegisterCommand)
        {
            return Err(TurnError::Plugin(PluginError::CapabilityDenied(
                crate::PluginCapability::RegisterCommand,
            )));
        }
        if !installed.manifest.commands.contains(command) {
            return Err(TurnError::Plugin(PluginError::UndeclaredCommand(
                command.to_owned(),
            )));
        }
        let grant = PluginGrant {
            id: pin.id.clone(),
            version,
            component_sha256: pin.component_hash.clone(),
            capabilities: pin.capabilities.clone(),
            settings: pin.settings.clone(),
            enabled: true,
        };
        let mut state = self.state_transaction(session_id)?;
        let receipt = PluginHost::new(Default::default()).execute(
            &installed,
            &grant,
            PluginInput {
                event: PluginEvent::Command,
                plugin_id: plugin_id.to_owned(),
                settings: pin.settings.clone(),
                context: json!({"command": command, "arguments": arguments}),
                artifact: Value::Null,
                state: Value::Object(state.local_namespace(plugin_id).into_iter().collect()),
                session: Value::Null,
            },
        )?;
        for effect in &receipt.effects {
            if let PluginEffect::StateWrite { key, value } = effect {
                state.set(
                    key.scope,
                    &key.name,
                    value.clone(),
                    plugin_id,
                    "runtime-plugin-command",
                );
            }
        }
        let state_mutations = state.mutations();
        let command_execution_id = EntityId::new();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        append_event(
            &transaction,
            Some(session_id),
            "plugin.command",
            &json!({
                "command_execution_id": command_execution_id,
                "session_id": session_id,
                "plugin_id": plugin_id,
                "command": command,
                "arguments": arguments,
                "receipt": receipt,
                "state_mutations": state_mutations,
            }),
        )?;
        apply_plugin_command_state_mutations(
            &transaction,
            session_id,
            command_execution_id,
            &state_mutations,
        )?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(PluginCommandResult {
            command_execution_id,
            session_id,
            plugin_id: plugin_id.to_owned(),
            command: command.to_owned(),
            receipt,
            state_mutations,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn build_prompt_plan(
        &self,
        session_id: EntityId,
        branch_id: EntityId,
        configuration: &SessionConfigurationRecord,
        character: &crate::DecodedArtifact,
        user_content: &str,
        generation_type: GenerationType,
        excluded_turn_id: Option<EntityId>,
        preset_value: Option<&Value>,
        effective_generation_settings: &EffectiveGenerationSettings,
        regex_scripts: &[RegexScript],
        text_prefill: Option<&str>,
    ) -> Result<PromptPlan, TurnError> {
        let branch = self
            .branch(branch_id)?
            .ok_or(TurnError::BranchNotFound(branch_id))?;
        if branch.session_id != session_id {
            return Err(TurnError::BranchSessionMismatch);
        }
        let tokenizer = configuration
            .configuration
            .tokenizer
            .parse::<TokenizerId>()?;
        let data = match character.kind {
            crate::ArtifactKind::CharacterCardV1 => character.semantic.as_object(),
            crate::ArtifactKind::CharacterCardV2 | crate::ArtifactKind::CharacterCardV3 => {
                character.semantic.get("data").and_then(Value::as_object)
            }
            _ => None,
        }
        .ok_or(TurnError::CharacterDataMissing)?;
        let character_name = data
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Character");
        let mut history = self.turns_for_branch(branch_id)?;
        if let Some(excluded_turn_id) = excluded_turn_id {
            history.retain(|turn| turn.turn_id != excluded_turn_id);
        }
        history.retain(|turn| !turn.hidden);
        let mut context = MacroContext {
            random_seed: u64::from_be_bytes(
                configuration.revision_hash.as_bytes()[..8]
                    .try_into()
                    .expect("SHA-256 prefix"),
            ),
            ..MacroContext::default()
        };
        context.insert("user", &configuration.configuration.persona_name);
        context.insert("char", character_name);
        context.insert("persona", &configuration.configuration.persona_name);
        context.insert("model", &configuration.configuration.provider.model);
        for (macro_name, field) in [
            ("chardescription", "description"),
            ("charpersonality", "personality"),
            ("charscenario", "scenario"),
            ("charfirstmessage", "first_mes"),
            ("charcreatornotes", "creator_notes"),
            ("charversion", "character_version"),
            ("mesexamplesraw", "mes_example"),
            ("mesexamples", "mes_example"),
            ("charprompt", "system_prompt"),
            ("charinstruction", "post_history_instructions"),
        ] {
            context.insert(
                macro_name,
                data.get(field).and_then(Value::as_str).unwrap_or_default(),
            );
        }
        for (macro_name, field) in [
            ("description", "description"),
            ("personality", "personality"),
            ("scenario", "scenario"),
        ] {
            context.insert(
                macro_name,
                data.get(field).and_then(Value::as_str).unwrap_or_default(),
            );
        }
        for macro_name in [
            "group",
            "groupnotmuted",
            "summary",
            "short_term_memory",
            "long_term_memory",
        ] {
            context.insert(macro_name, "");
        }
        context.insert("lastgenerationtype", generation_type.as_str());
        context.insert("lastmessageid", history.len().to_string());
        if let Some(last) = history.last() {
            context.insert("lastusermessage", &last.user_content);
            if let Some(candidate_id) = last.selected_candidate_id {
                if let Some(candidate) = self.candidate(candidate_id)?
                    && !candidate.hidden
                {
                    context.insert("lastcharmessage", &candidate.content);
                    context.insert("lastmessage", &candidate.content);
                    context.insert("lastchatmessage", &candidate.content);
                } else {
                    context.insert("lastmessage", &last.user_content);
                }
            } else {
                context.insert("lastmessage", &last.user_content);
                context.insert("lastchatmessage", &last.user_content);
            }
        }

        let mut state = self.state_transaction(session_id)?;
        let mut engine = MacroEngine::new(context.random_seed);
        let mut evaluations = Vec::new();
        let mut warnings = Vec::new();
        let persona_description = configuration
            .configuration
            .persona_description
            .as_deref()
            .filter(|description| !description.trim().is_empty())
            .map(|raw| {
                render_segment_macro_text(
                    &mut engine,
                    &context,
                    &mut state,
                    raw,
                    &mut evaluations,
                    &mut warnings,
                )
                .map(|rendered| (raw.to_owned(), rendered))
            })
            .transpose()?;
        if let Some((_, rendered)) = &persona_description {
            context.insert("personaDescription", &rendered.content);
            context.insert("persona_description", &rendered.content);
        }
        let plugin_receipts = self.run_runtime_plugins(
            configuration,
            session_id,
            branch_id,
            generation_type,
            &mut context,
            &mut state,
        )?;
        let plugin_contributions = plugin_receipts
            .iter()
            .flat_map(|receipt| receipt.effects.iter())
            .filter_map(|effect| match effect {
                PluginEffect::Prompt { contribution } => Some(contribution.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut lore_entries = Vec::new();
        let mut source_index = 0;
        if data
            .get("character_book")
            .and_then(Value::as_object)
            .is_some()
        {
            lore_entries.extend(parse_lore_entries(
                &configuration.configuration.character_revision,
                &character.semantic,
                source_index,
            )?);
            source_index += 1;
        }
        for revision in &configuration.configuration.lorebook_revisions {
            let lorebook = self.decoded_artifact(revision)?;
            lore_entries.extend(parse_lore_entries(
                revision,
                &lorebook.semantic,
                source_index,
            )?);
            source_index += 1;
        }
        for entry in &mut lore_entries {
            for key in entry.keys.iter_mut().chain(entry.secondary_keys.iter_mut()) {
                *key = render_macro_text(
                    &mut engine,
                    &context,
                    &mut state,
                    key,
                    &mut evaluations,
                    &mut warnings,
                )?;
            }
        }
        let mut prior_activations = BTreeMap::new();
        for (index, prior_turn) in history.iter().enumerate() {
            let Some(candidate_id) = prior_turn.selected_candidate_id else {
                continue;
            };
            let Some(candidate) = self.candidate(candidate_id)? else {
                continue;
            };
            if candidate.hidden {
                continue;
            }
            let Some(attempt_id) = candidate.attempt_id else {
                continue;
            };
            let Some(attempt) = self.attempt(attempt_id)? else {
                continue;
            };
            for activated in attempt.prompt_plan.lore.activated {
                prior_activations.insert(activated.entry_key, 2 + index * 2);
            }
        }
        let mut scan_messages = vec![user_content.to_owned()];
        for prior_turn in history.iter().rev() {
            if let Some(candidate_id) = prior_turn.selected_candidate_id
                && let Some(candidate) = self.candidate(candidate_id)?
                && !candidate.hidden
            {
                scan_messages.push(candidate.content);
            }
            scan_messages.push(prior_turn.user_content.clone());
        }
        scan_messages.push(branch.greeting.clone());
        let lore_settings = lore_settings(
            &configuration.configuration.generation_settings,
            context.random_seed,
            scan_messages.len(),
            prior_activations,
            generation_type,
        );
        let mut lore_segment_effects =
            BTreeMap::<String, (String, Vec<MacroEvaluation>, Vec<StateMutation>)>::new();
        let worldinfo_scripts: Vec<&RegexScript> = regex_scripts
            .iter()
            .filter(|script| {
                script
                    .placements
                    .contains(&RegexPlacement::WorldInfo.code())
            })
            .collect();
        let lore_engine = LoreEngine::new(tokenizer)?;
        let lore = lore_engine.evaluate_transformed(
            &lore_entries,
            &scan_messages,
            &lore_settings,
            |entry| {
                let before = state.mutations();
                let rendered = engine.render(&entry.content, &context, &mut state)?;
                let state_mutations = state_mutation_delta(&before, &state.mutations());
                evaluations.extend(rendered.evaluations.iter().cloned());
                warnings.extend(rendered.warnings);
                let mut content = rendered.text;
                if !worldinfo_scripts.is_empty() {
                    let worker = EcmaRegexWorker::current(std::time::Duration::from_millis(250))?;
                    let finder = |pattern: &str, flags: &str, text: &str| {
                        worker.find_matches(pattern, flags, text)
                    };
                    for script in &worldinfo_scripts {
                        if script.disabled {
                            continue;
                        }
                        let matches = finder(&script.find_pattern, &script.find_flags, &content)
                            .map_err(LoreError::Regex)?;
                        if matches.is_empty() {
                            continue;
                        }
                        let mut output = String::with_capacity(content.len());
                        let mut cursor = 0;
                        for matched in &matches {
                            output.push_str(&content[cursor..matched.start]);
                            let replacement = regex_script::expand_replacement(
                                &script.replace_string,
                                matched,
                                &script.trim_strings,
                            );
                            output.push_str(&replacement);
                            cursor = matched.end;
                        }
                        output.push_str(&content[cursor..]);
                        content = output;
                    }
                }
                lore_segment_effects.insert(
                    entry.key(),
                    (entry.content.clone(), rendered.evaluations, state_mutations),
                );
                Ok(content)
            },
        )?;
        for activated in lore
            .activated
            .iter()
            .filter(|entry| entry.position == LorePosition::Outlet && !entry.outlet.is_empty())
        {
            context
                .outlets
                .entry(activated.outlet.clone())
                .and_modify(|content| {
                    content.push('\n');
                    content.push_str(&activated.content);
                })
                .or_insert_with(|| activated.content.clone());
        }
        let system_raw = format!(
            "Write {character_name}'s next reply in a fictional chat between {character_name} and {}.",
            configuration.configuration.persona_name
        );
        let system = render_segment_macro_text(
            &mut engine,
            &context,
            &mut state,
            &system_raw,
            &mut evaluations,
            &mut warnings,
        )?;
        let definition_fields = [
            ("character-description", "description"),
            ("character-personality", "personality"),
            ("character-scenario", "scenario"),
        ];
        let mut definitions = Vec::new();
        if configuration.configuration.provider.format_mode == FormatMode::TextCompletion {
            for (source, field) in definition_fields {
                let raw = data
                    .get(field)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if raw.is_empty() {
                    continue;
                }
                let rendered = render_segment_macro_text(
                    &mut engine,
                    &context,
                    &mut state,
                    &raw,
                    &mut evaluations,
                    &mut warnings,
                )?;
                definitions.push((source, Some(field), raw, rendered));
            }
        } else {
            let raw = definition_fields
                .iter()
                .filter_map(|(_, field)| data.get(*field).and_then(Value::as_str))
                .filter(|value| !value.is_empty())
                .collect::<Vec<_>>()
                .join("\n\n");
            let rendered = render_segment_macro_text(
                &mut engine,
                &context,
                &mut state,
                &raw,
                &mut evaluations,
                &mut warnings,
            )?;
            definitions.push(("character-definitions", None, raw, rendered));
        }
        let greeting = render_segment_macro_text(
            &mut engine,
            &context,
            &mut state,
            &branch.greeting,
            &mut evaluations,
            &mut warnings,
        )?;
        let current_user = render_segment_macro_text(
            &mut engine,
            &context,
            &mut state,
            user_content,
            &mut evaluations,
            &mut warnings,
        )?;

        let mut segments = Vec::new();
        push_segment(
            &mut segments,
            tokenizer,
            "main-prompt",
            ChatRole::System,
            system_raw,
            system,
        );
        push_lore_segment(
            &mut segments,
            tokenizer,
            &lore.activated,
            &lore_segment_effects,
            LorePosition::Before,
            "world-info-before",
        );
        if let Some((raw, rendered)) = persona_description {
            push_segment(
                &mut segments,
                tokenizer,
                "persona-description",
                ChatRole::System,
                raw,
                rendered,
            );
        }
        for (source, source_field, raw, rendered) in definitions {
            if rendered.content.is_empty() {
                continue;
            }
            push_segment(
                &mut segments,
                tokenizer,
                source,
                ChatRole::System,
                raw,
                rendered,
            );
            let segment = segments.last_mut().unwrap();
            segment.source_revision = Some(configuration.configuration.character_revision.clone());
            segment.source_field = source_field.map(str::to_owned);
        }
        push_lore_segment(
            &mut segments,
            tokenizer,
            &lore.activated,
            &lore_segment_effects,
            LorePosition::After,
            "world-info-after",
        );
        for (block_index, message_index, role, example) in parse_example_messages(
            data.get("mes_example")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            &configuration.configuration.persona_name,
            character_name,
        ) {
            let source = format!("example:{block_index}:{message_index}");
            let content = render_segment_macro_text(
                &mut engine,
                &context,
                &mut state,
                &example,
                &mut evaluations,
                &mut warnings,
            )?;
            let mut segment = PromptSegment::new(
                tokenizer,
                &source,
                "dialogueExamples",
                role,
                content.content,
            );
            segment.raw_content = example;
            segment.macro_evaluations = content.macro_evaluations;
            segment.state_mutations = content.state_mutations;
            segment.source_revision = Some(configuration.configuration.character_revision.clone());
            segment.truncation_priority = 50;
            segments.push(segment);
        }
        push_segment(
            &mut segments,
            tokenizer,
            "greeting",
            ChatRole::Assistant,
            branch.greeting.clone(),
            greeting,
        );
        segments.last_mut().unwrap().source_revision =
            Some(configuration.configuration.character_revision.clone());
        for turn in history {
            push_segment(
                &mut segments,
                tokenizer,
                format!("turn:{}:user", turn.turn_id),
                ChatRole::User,
                turn.user_content.clone(),
                RenderedPromptContent::plain(turn.user_content),
            );
            if let Some(candidate_id) = turn.selected_candidate_id {
                let candidate = self
                    .candidate(candidate_id)?
                    .ok_or(TurnError::CandidateNotFound(candidate_id))?;
                if !candidate.hidden {
                    push_segment(
                        &mut segments,
                        tokenizer,
                        format!("turn:{}:assistant", turn.turn_id),
                        ChatRole::Assistant,
                        candidate.content.clone(),
                        RenderedPromptContent::plain(candidate.content),
                    );
                }
            }
        }
        for position in [
            LorePosition::ExampleTop,
            LorePosition::ExampleBottom,
            LorePosition::AuthorNoteTop,
            LorePosition::AuthorNoteBottom,
            LorePosition::AtDepth,
        ] {
            push_lore_segment(
                &mut segments,
                tokenizer,
                &lore.activated,
                &lore_segment_effects,
                position,
                "world-info-in-chat",
            );
        }
        if !current_user.content.is_empty() {
            push_segment(
                &mut segments,
                tokenizer,
                "current-user-action",
                ChatRole::User,
                user_content.to_owned(),
                current_user,
            );
        }
        if generation_type == GenerationType::Continue
            && let Some(nudge) = effective_generation_settings
                .values
                .get("continue_nudge_prompt")
                .and_then(Value::as_str)
            && !nudge.is_empty()
        {
            let nudge_raw = nudge;
            let nudge = render_segment_macro_text(
                &mut engine,
                &context,
                &mut state,
                nudge_raw,
                &mut evaluations,
                &mut warnings,
            )?;
            push_segment(
                &mut segments,
                tokenizer,
                "continue-nudge",
                ChatRole::System,
                nudge_raw.to_owned(),
                nudge,
            );
            if effective_generation_settings
                .provenance
                .get("continue_nudge_prompt")
                == Some(&GenerationSettingSource::Preset)
            {
                segments.last_mut().unwrap().source_revision =
                    configuration.configuration.prompt_preset_revision.clone();
            }
        }
        inject_plugin_contributions(tokenizer, &mut segments, plugin_contributions)?;
        let mut segments =
            if configuration.configuration.provider.format_mode == FormatMode::TextCompletion {
                insert_in_chat_segments(segments)
            } else {
                let preset = preset_value
                    .map(|value| PromptPreset::parse(value, CHAT_COMPLETION_CHARACTER_ID))
                    .transpose()?
                    .map(|mut preset| {
                        for entry in &mut preset.order {
                            if let Some(enabled) = configuration
                                .configuration
                                .prompt_order_overrides
                                .get(&entry.identifier)
                            {
                                entry.enabled = *enabled;
                            }
                        }
                        preset
                    });
                let mut segments =
                    apply_prompt_preset(tokenizer, preset.as_ref(), segments, |_, input| {
                        let before = state.mutations();
                        let rendered = engine
                            .render(input, &context, &mut state)
                            .map_err(|error| PromptError::Render(error.to_string()))?;
                        let state_mutations = state_mutation_delta(&before, &state.mutations());
                        evaluations.extend(rendered.evaluations.iter().cloned());
                        warnings.extend(rendered.warnings);
                        Ok(RenderedPromptContent {
                            content: rendered.text,
                            macro_evaluations: rendered.evaluations,
                            state_mutations,
                        })
                    })?;
                if let Some(preset_revision) = &configuration.configuration.prompt_preset_revision {
                    for segment in &mut segments {
                        if segment.source.starts_with("preset:") {
                            segment.source_revision = Some(preset_revision.clone());
                        }
                    }
                }
                segments
            };
        let regex_applications = apply_regex_scripts_to_segments(
            regex_scripts,
            &mut segments,
            tokenizer,
            &mut engine,
            &context,
            &mut state,
            &mut evaluations,
            &mut warnings,
        )?;
        let effective = effective_generation_settings
            .values
            .as_object()
            .expect("effective settings are an object");
        if !effective
            .get("use_sysprompt")
            .and_then(Value::as_bool)
            .unwrap_or(true)
        {
            segments.retain(|segment| segment.slot != "main");
        }
        if configuration.configuration.provider.format_mode == FormatMode::ChatCompletion
            && effective
                .get("squash_system_messages")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        {
            segments = squash_system_segments(tokenizer, segments);
        }
        let context_limit = effective
            .get("max_context")
            .and_then(Value::as_u64)
            .unwrap_or(8_192) as usize;
        let response_reserve = effective
            .get("max_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(512) as usize;
        let (pruning, total_tokens, text_prompt, stop_sequences) =
            if configuration.configuration.provider.format_mode == FormatMode::TextCompletion {
                let provider = &configuration.configuration.provider;
                let (projection, pruning) = prune_text_completion(
                    tokenizer,
                    &mut segments,
                    provider
                        .instruct_template
                        .as_ref()
                        .expect("validated Text Completion instruct template"),
                    provider
                        .context_formatting
                        .as_ref()
                        .expect("validated Text Completion context formatting"),
                    &configuration.configuration.persona_name,
                    character_name,
                    text_prefill,
                    context_limit,
                    response_reserve,
                )?;
                let total_tokens = pruning.kept_tokens;
                (
                    pruning,
                    total_tokens,
                    Some(projection.prompt),
                    projection.stop_sequences,
                )
            } else {
                let pruning = prune_segments(&mut segments, context_limit, response_reserve)?;
                let total_tokens = pruning.kept_tokens;
                (pruning, total_tokens, None, Vec::new())
            };
        let messages = segments
            .iter()
            .filter(|segment| !segment.pruned)
            .map(|segment| ChatMessage {
                role: segment.role,
                content: segment.content.clone(),
            })
            .collect();
        Ok(PromptPlan {
            tokenizer,
            rng_seed: context.random_seed,
            segments,
            messages,
            format_mode: configuration.configuration.provider.format_mode,
            text_prompt,
            stop_sequences,
            total_tokens,
            macro_evaluations: evaluations,
            macro_warnings: warnings,
            state_mutations: state.mutations(),
            regex_applications,
            plugin_receipts,
            lore,
            generation_type,
            parent_candidate_id: None,
            continuation_prefix: None,
            pruning,
        })
    }

    fn begin_turn(
        &mut self,
        session_id: EntityId,
        branch_id: EntityId,
        user_content: String,
        config_hash: ContentHash,
        retry_of_attempt_id: Option<EntityId>,
        preparation: TurnPreparation,
    ) -> Result<(TurnProjection, AttemptProjection), TurnError> {
        let turn_id = EntityId::new();
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(session_id),
            "turn.created",
            &json!({
                "turn_id": turn_id,
                "branch_id": branch_id,
                "user_content": user_content,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
                params![
                    turn_id.to_string(),
                    session_id.to_string(),
                    branch_id.to_string(),
                    user_content,
                    event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        let turn = TurnProjection {
            turn_id,
            session_id,
            branch_id,
            user_content,
            selected_candidate_id: None,
            hidden: false,
            created_event_id: event.event_id.to_string(),
        };
        let attempt = self.begin_attempt(&turn, config_hash, retry_of_attempt_id, preparation)?;
        Ok((turn, attempt))
    }

    fn begin_attempt(
        &mut self,
        turn: &TurnProjection,
        config_hash: ContentHash,
        retry_of_attempt_id: Option<EntityId>,
        preparation: TurnPreparation,
    ) -> Result<AttemptProjection, TurnError> {
        let attempt_id = EntityId::new();
        let TurnPreparation {
            prompt_plan,
            effective_generation_settings,
            provider_request: request,
            compatibility_warnings,
            preset_transformations,
        } = preparation;
        let request_hash = provider_request_hash(&request)?;
        let effect_receipt = AttemptEffectReceipt {
            rng_seed: prompt_plan.rng_seed,
            clock_outcomes: Vec::new(),
            lore: prompt_plan.lore.clone(),
            macro_evaluations: prompt_plan.macro_evaluations.clone(),
            macro_warnings: prompt_plan.macro_warnings.clone(),
            effective_generation_settings,
            compatibility_warnings,
            preset_transformations,
            state_mutations: prompt_plan.state_mutations.clone(),
            plugins: prompt_plan.plugin_receipts.clone(),
            provider_request: request,
            provider_request_hash: request_hash.clone(),
        };
        let prompt_bytes = canonical_json(&serde_json::to_value(&prompt_plan)?)?;
        let effect_bytes = canonical_json(&serde_json::to_value(&effect_receipt)?)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(turn.session_id),
            "attempt.started",
            &json!({
                "attempt_id": attempt_id,
                "turn_id": turn.turn_id,
                "config_hash": config_hash,
                "retry_of_attempt_id": retry_of_attempt_id,
                "prompt_plan": prompt_plan,
                "effect_receipt": effect_receipt,
            }),
        )?;
        transaction
            .execute(
                "INSERT INTO attempts(attempt_id, turn_id, config_hash, retry_of_attempt_id, status, prompt_plan, provider_request_hash, effect_receipt, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    attempt_id.to_string(),
                    turn.turn_id.to_string(),
                    config_hash.to_string(),
                    retry_of_attempt_id.map(|id| id.to_string()),
                    AttemptStatus::Running.as_str(),
                    prompt_bytes,
                    request_hash.to_string(),
                    effect_bytes,
                    event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(AttemptProjection {
            attempt_id,
            turn_id: turn.turn_id,
            config_hash,
            retry_of_attempt_id,
            status: AttemptStatus::Running,
            prompt_plan,
            provider_request_hash: Some(request_hash),
            provider_receipt: None,
            effect_receipt: Some(effect_receipt),
            error_message: None,
            created_event_id: event.event_id.to_string(),
            completed_event_id: None,
        })
    }

    async fn execute_attempt(
        &mut self,
        turn: TurnProjection,
        attempt: AttemptProjection,
        configuration: SessionConfigurationRecord,
        on_event: &mut impl FnMut(&ProviderEvent),
    ) -> Result<CompletedTurn, TurnError> {
        let provider = match OpenAiProvider::new(configuration.configuration.provider.clone()) {
            Ok(provider) => provider,
            Err(error) => {
                self.fail_attempt(&turn, &attempt, &error.to_string())?;
                return Err(TurnError::Provider(error));
            }
        };
        let partial_text = Arc::new(Mutex::new(String::new()));
        let result = {
            let captured_text = Arc::clone(&partial_text);
            let request = &attempt
                .effect_receipt
                .as_ref()
                .ok_or(TurnError::AttemptEffectReceiptMissing(attempt.attempt_id))?
                .provider_request;
            let generation = provider.generate_request(request, |event| {
                if let ProviderEvent::TextDelta { text } = event {
                    captured_text.lock().push_str(text);
                }
                on_event(event);
            });
            tokio::pin!(generation);
            loop {
                tokio::select! {
                    result = &mut generation => break result,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                        let current = self
                            .attempt(attempt.attempt_id)?
                            .ok_or(TurnError::AttemptNotFound(attempt.attempt_id))?;
                        if current.status != AttemptStatus::Running {
                            let partial_text = partial_text.lock().clone();
                            self.record_cancellation_receipt(&turn, &attempt, &partial_text)?;
                            return Err(TurnError::AttemptNotRunning {
                                attempt_id: attempt.attempt_id,
                                status: current.status,
                            });
                        }
                    }
                }
            }
        };
        let reasoning_scripts = self.granted_scripts_for_attempt(&configuration)?;
        match result {
            Ok(result) => match self.complete_attempt(turn.clone(), attempt.clone(), result) {
                Ok(mut completed) => {
                    completed.scrubbed_reasoning =
                        scrub_reasoning(&completed.provider_events, &reasoning_scripts);
                    Ok(completed)
                }
                Err(error) => {
                    self.fail_attempt(&turn, &attempt, &error.to_string())?;
                    Err(error)
                }
            },
            Err(error) => {
                self.fail_attempt(&turn, &attempt, &error.to_string())?;
                Err(TurnError::Provider(error))
            }
        }
    }

    pub(crate) fn granted_scripts_for_attempt(
        &self,
        configuration: &SessionConfigurationRecord,
    ) -> Result<Vec<RegexScript>, TurnError> {
        let mut all_scripts = Vec::new();
        if let Some(revision) = &configuration.configuration.prompt_preset_revision {
            let preset = self.decoded_artifact(revision)?;
            let transformation = transform_preset_content(
                &configuration.configuration.compatibility_profile,
                revision,
                &preset.semantic,
                &configuration.configuration.script_grants,
            );
            all_scripts.extend(transformation.scripts);
        }
        let character = self.decoded_artifact(&configuration.configuration.character_revision)?;
        all_scripts.extend(extract_character_scripts(
            &character.semantic,
            &configuration.configuration.script_grants,
        ));
        Ok(granted_regex_scripts(&all_scripts))
    }

    fn complete_attempt(
        &mut self,
        mut turn: TurnProjection,
        mut attempt: AttemptProjection,
        result: ProviderResult,
    ) -> Result<CompletedTurn, TurnError> {
        let candidate_id = EntityId::new();
        let parent_candidate_id = attempt.prompt_plan.parent_candidate_id;
        let origin = if attempt.prompt_plan.generation_type == GenerationType::Continue {
            CandidateOrigin::Continued
        } else {
            CandidateOrigin::Generated
        };
        let origin_name = match origin {
            CandidateOrigin::Generated => "generated",
            CandidateOrigin::Continued => "continued",
            CandidateOrigin::Manual => unreachable!(),
            CandidateOrigin::AcceptedPartial => unreachable!(),
        };
        let content = attempt
            .prompt_plan
            .continuation_prefix
            .as_deref()
            .map(|prefix| format!("{prefix}{}", result.text))
            .unwrap_or_else(|| result.text.clone());
        if attempt.provider_request_hash.as_ref() != Some(&result.request_hash) {
            return Err(TurnError::ProviderRequestHashMismatch);
        }
        let post_commit_receipts =
            self.run_post_commit_plugins(&attempt, turn.session_id, turn.branch_id, &content)?;
        if !post_commit_receipts.is_empty() {
            let effect_receipt = attempt
                .effect_receipt
                .as_mut()
                .ok_or(TurnError::AttemptEffectReceiptMissing(attempt.attempt_id))?;
            effect_receipt.plugins.extend(post_commit_receipts);
        }
        let receipt_bytes = canonical_json(&result.receipt)?;
        let effect_receipt_bytes = attempt
            .effect_receipt
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .as_ref()
            .map(canonical_json)
            .transpose()?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(turn.session_id),
            "attempt.completed",
            &json!({
                "attempt_id": attempt.attempt_id,
                "turn_id": turn.turn_id,
                "candidate_id": candidate_id,
                "parent_candidate_id": parent_candidate_id,
                "origin": origin,
                "provider_request_hash": result.request_hash,
                "provider_receipt": result.receipt,
                "plugin_receipts": attempt
                    .effect_receipt
                    .as_ref()
                    .map(|receipt| &receipt.plugins),
                "content": content,
            }),
        )?;
        let updated = transaction
            .execute(
                "UPDATE attempts SET status = ?1, provider_request_hash = ?2, provider_receipt = ?3, effect_receipt = ?4, completed_event_id = ?5 WHERE attempt_id = ?6 AND status = 'running'",
                params![
                    AttemptStatus::Completed.as_str(),
                    result.request_hash.to_string(),
                    receipt_bytes,
                    effect_receipt_bytes,
                    event.event_id.to_string(),
                    attempt.attempt_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        if updated == 0 {
            transaction.rollback().map_err(StorageError::Sqlite)?;
            return Err(self.attempt_not_running(attempt.attempt_id)?);
        }
        transaction
            .execute(
                "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    candidate_id.to_string(),
                    turn.turn_id.to_string(),
                    attempt.attempt_id.to_string(),
                    parent_candidate_id.map(|id| id.to_string()),
                    origin_name,
                    content,
                    event.event_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        transaction
            .execute(
                "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                params![candidate_id.to_string(), turn.turn_id.to_string()],
            )
            .map_err(StorageError::Sqlite)?;
        apply_state_mutations(
            &transaction,
            turn.session_id,
            attempt.attempt_id,
            &attempt.prompt_plan.state_mutations,
        )?;
        transaction.commit().map_err(StorageError::Sqlite)?;

        turn.selected_candidate_id = Some(candidate_id);
        attempt.status = AttemptStatus::Completed;
        attempt.provider_request_hash = Some(result.request_hash);
        attempt.provider_receipt = Some(result.receipt);
        attempt.completed_event_id = Some(event.event_id.to_string());
        let candidate = CandidateProjection {
            candidate_id,
            turn_id: turn.turn_id,
            attempt_id: Some(attempt.attempt_id),
            parent_candidate_id,
            origin,
            content,
            rendered_content: None,
            hidden: false,
            created_event_id: event.event_id.to_string(),
        };
        Ok(CompletedTurn {
            turn,
            attempt,
            candidate,
            provider_events: result.events,
            scrubbed_reasoning: None,
        })
    }

    fn record_cancellation_receipt(
        &mut self,
        turn: &TurnProjection,
        attempt: &AttemptProjection,
        partial_text: &str,
    ) -> Result<(), TurnError> {
        let receipt = json!({
            "cancelled": true,
            "partial_text": partial_text,
        });
        let candidate_id = (!partial_text.is_empty()).then(EntityId::new);
        let accepted_content = candidate_id.map(|_| {
            attempt
                .prompt_plan
                .continuation_prefix
                .as_deref()
                .map(|prefix| format!("{prefix}{partial_text}"))
                .unwrap_or_else(|| partial_text.to_owned())
        });
        let receipt_bytes = canonical_json(&receipt)?;
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(turn.session_id),
            "attempt.cancellation-receipt",
            &json!({
                "attempt_id": attempt.attempt_id,
                "turn_id": turn.turn_id,
                "partial_text": partial_text,
                "candidate_content": accepted_content,
                "parent_candidate_id": attempt.prompt_plan.parent_candidate_id,
                "candidate_id": candidate_id,
                "origin": candidate_id.map(|_| CandidateOrigin::AcceptedPartial),
            }),
        )?;
        transaction
            .execute(
                "UPDATE attempts SET provider_receipt = ?1, completed_event_id = ?2 WHERE attempt_id = ?3 AND status = 'cancelled'",
                params![
                    receipt_bytes,
                    event.event_id.to_string(),
                    attempt.attempt_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        if let (Some(candidate_id), Some(content)) = (candidate_id, accepted_content) {
            transaction
                .execute(
                    "INSERT INTO candidates(candidate_id, turn_id, attempt_id, parent_candidate_id, origin, content, created_event_id) VALUES (?1, ?2, ?3, ?4, 'accepted-partial', ?5, ?6)",
                    params![
                        candidate_id.to_string(),
                        turn.turn_id.to_string(),
                        attempt.attempt_id.to_string(),
                        attempt.prompt_plan.parent_candidate_id.map(|id| id.to_string()),
                        content,
                        event.event_id.to_string(),
                    ],
                )
                .map_err(StorageError::Sqlite)?;
            transaction
                .execute(
                    "UPDATE turns SET selected_candidate_id = ?1 WHERE turn_id = ?2",
                    params![candidate_id.to_string(), turn.turn_id.to_string()],
                )
                .map_err(StorageError::Sqlite)?;
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        Ok(())
    }

    fn fail_attempt(
        &mut self,
        turn: &TurnProjection,
        attempt: &AttemptProjection,
        message: &str,
    ) -> Result<FailedTurn, TurnError> {
        let transaction = self
            .connection
            .transaction()
            .map_err(StorageError::Sqlite)?;
        let event = append_event(
            &transaction,
            Some(turn.session_id),
            "attempt.failed",
            &json!({
                "attempt_id": attempt.attempt_id,
                "turn_id": turn.turn_id,
                "message": message,
            }),
        )?;
        let updated = transaction
            .execute(
                "UPDATE attempts SET status = ?1, error_message = ?2, completed_event_id = ?3 WHERE attempt_id = ?4 AND status = 'running'",
                params![
                    AttemptStatus::Failed.as_str(),
                    message,
                    event.event_id.to_string(),
                    attempt.attempt_id.to_string(),
                ],
            )
            .map_err(StorageError::Sqlite)?;
        if updated == 0 {
            transaction.rollback().map_err(StorageError::Sqlite)?;
            return Err(self.attempt_not_running(attempt.attempt_id)?);
        }
        transaction.commit().map_err(StorageError::Sqlite)?;
        let mut failed = attempt.clone();
        failed.status = AttemptStatus::Failed;
        failed.error_message = Some(message.to_owned());
        failed.completed_event_id = Some(event.event_id.to_string());
        Ok(FailedTurn {
            turn: turn.clone(),
            attempt: failed,
        })
    }

    fn attempt_not_running(&self, attempt_id: EntityId) -> Result<TurnError, TurnError> {
        let status = self
            .attempt(attempt_id)?
            .ok_or(TurnError::AttemptNotFound(attempt_id))?
            .status;
        Ok(TurnError::AttemptNotRunning { attempt_id, status })
    }
}
fn inject_plugin_contributions(
    tokenizer: TokenizerId,
    segments: &mut Vec<PromptSegment>,
    mut contributions: Vec<PromptContribution>,
) -> Result<(), TurnError> {
    contributions.sort_by(|left, right| {
        left.slot
            .cmp(&right.slot)
            .then_with(|| left.order.cmp(&right.order))
            .then_with(|| left.name.cmp(&right.name))
    });
    for contribution in contributions {
        let role = match contribution.role.as_str() {
            "system" => ChatRole::System,
            "user" => ChatRole::User,
            "assistant" => ChatRole::Assistant,
            role => return Err(TurnError::InvalidPluginRole(role.to_owned())),
        };
        let mut segment = PromptSegment::new(
            tokenizer,
            format!("runtime-plugin:{}", contribution.name),
            plugin_slot_name(contribution.slot),
            role,
            contribution.content,
        );
        segment.in_chat_depth = contribution.depth;
        segment.in_chat_order = contribution.order;
        let index = match contribution.slot {
            PromptSlot::BeforeCharacterDefinitions => segments
                .iter()
                .position(|segment| segment.source.starts_with("character-"))
                .unwrap_or(segments.len()),
            PromptSlot::AfterCharacterDefinitions => segments
                .iter()
                .rposition(|segment| segment.source.starts_with("character-"))
                .map_or(segments.len(), |index| index + 1),
            PromptSlot::BeforeExampleMessages => segments
                .iter()
                .position(|segment| segment.source.starts_with("example:"))
                .unwrap_or(segments.len()),
            PromptSlot::AfterExampleMessages => segments
                .iter()
                .rposition(|segment| segment.source.starts_with("example:"))
                .map_or(segments.len(), |index| index + 1),
            PromptSlot::NamedLoreOutlet => segments
                .iter()
                .rposition(|segment| segment.source.starts_with("world-info"))
                .map_or(segments.len(), |index| index + 1),
            PromptSlot::BeforeHistory => segments
                .iter()
                .position(|segment| segment.source == "greeting")
                .unwrap_or(segments.len()),
            PromptSlot::AfterHistory | PromptSlot::PostHistoryInstructions => segments
                .iter()
                .position(|segment| segment.source == "current-user-action")
                .unwrap_or(segments.len()),
            PromptSlot::InChat => segments.len(),
        };
        segments.insert(index, segment);
    }
    Ok(())
}

fn plugin_slot_name(slot: PromptSlot) -> &'static str {
    match slot {
        PromptSlot::BeforeCharacterDefinitions => "pluginBeforeCharacterDefinitions",
        PromptSlot::AfterCharacterDefinitions => "pluginAfterCharacterDefinitions",
        PromptSlot::BeforeExampleMessages => "pluginBeforeExampleMessages",
        PromptSlot::AfterExampleMessages => "pluginAfterExampleMessages",
        PromptSlot::NamedLoreOutlet => "pluginLoreOutlet",
        PromptSlot::InChat => "pluginInChat",
        PromptSlot::BeforeHistory => "pluginBeforeHistory",
        PromptSlot::AfterHistory => "pluginAfterHistory",
        PromptSlot::PostHistoryInstructions => "pluginPostHistoryInstructions",
    }
}

fn empty_effective_generation_settings() -> EffectiveGenerationSettings {
    EffectiveGenerationSettings {
        values: json!({}),
        provenance: BTreeMap::new(),
    }
}

fn resolve_effective_generation_settings(
    configuration: &SessionConfigurationRecord,
    preset: Option<&Value>,
) -> EffectiveGenerationSettings {
    let session = configuration.configuration.generation_settings.as_object();
    let preset = preset.and_then(Value::as_object);
    let mut values = session.cloned().unwrap_or_default();
    let mut provenance = values
        .keys()
        .map(|name| (name.clone(), GenerationSettingSource::Session))
        .collect::<BTreeMap<_, _>>();
    let fields = [
        ("temperature", "temperature", None),
        ("top_p", "top_p", None),
        ("frequency_penalty", "frequency_penalty", None),
        ("presence_penalty", "presence_penalty", None),
        ("top_k", "top_k", None),
        ("min_p", "min_p", None),
        ("repetition_penalty", "repetition_penalty", None),
        ("reasoning_effort", "reasoning_effort", None),
        ("seed", "seed", None),
        ("n", "n", None),
        ("max_tokens", "openai_max_tokens", Some(json!(512))),
        ("max_context", "openai_max_context", Some(json!(8192))),
        (
            "squash_system_messages",
            "squash_system_messages",
            Some(json!(false)),
        ),
        ("use_sysprompt", "use_sysprompt", Some(json!(true))),
        ("continue_prefill", "continue_prefill", Some(json!(false))),
        ("assistant_prefill", "assistant_prefill", None),
        ("continue_nudge_prompt", "continue_nudge_prompt", None),
        ("max_context_unlocked", "max_context_unlocked", None),
        ("names_behavior", "names_behavior", None),
    ];
    for (name, preset_name, profile_default) in fields {
        let resolved = session
            .and_then(|settings| settings.get(name))
            .map(|value| (value.clone(), GenerationSettingSource::Session))
            .or_else(|| {
                preset
                    .and_then(|settings| settings.get(preset_name))
                    .map(|value| (value.clone(), GenerationSettingSource::Preset))
            })
            .or_else(|| profile_default.map(|value| (value, GenerationSettingSource::Profile)));
        if let Some((value, source)) = resolved {
            values.insert(name.to_owned(), value);
            provenance.insert(name.to_owned(), source);
        }
    }
    EffectiveGenerationSettings {
        values: Value::Object(values),
        provenance,
    }
}

fn provider_generation_settings(settings: &EffectiveGenerationSettings) -> Value {
    let source = settings
        .values
        .as_object()
        .expect("effective settings are an object");
    let mut provider = source.clone();
    for name in [
        "assistant_prefill",
        "continue_nudge_prompt",
        "continue_prefill",
        "max_context",
        "max_context_unlocked",
        "names_behavior",
        "squash_system_messages",
        "use_sysprompt",
    ] {
        provider.remove(name);
    }
    if source.get("seed").and_then(Value::as_i64) == Some(-1) {
        provider.remove("seed");
    }
    if source.get("min_p").and_then(Value::as_f64) == Some(0.0) {
        provider.remove("min_p");
    }
    if source.get("n").and_then(Value::as_i64) == Some(1) {
        provider.remove("n");
    }
    if source
        .get("reasoning_effort")
        .and_then(Value::as_str)
        .map(str::trim)
        .is_some_and(|value| {
            value.eq_ignore_ascii_case("auto") || value.eq_ignore_ascii_case("default")
        })
    {
        provider.remove("reasoning_effort");
    }
    Value::Object(provider)
}

fn squash_system_segments(
    tokenizer: TokenizerId,
    segments: Vec<PromptSegment>,
) -> Vec<PromptSegment> {
    let mut squashed: Vec<PromptSegment> = Vec::with_capacity(segments.len());
    for segment in segments {
        if segment.role == ChatRole::System
            && let Some(previous) = squashed.last_mut()
            && previous.role == ChatRole::System
        {
            previous.source.push('+');
            previous.source.push_str(&segment.source);
            previous.raw_content.push('\n');
            previous.raw_content.push_str(&segment.raw_content);
            previous.content.push('\n');
            previous.content.push_str(&segment.content);
            previous.token_count = tokenizer.count(&previous.content);
            previous.truncation_priority = previous
                .truncation_priority
                .max(segment.truncation_priority);
            if previous.source_revision != segment.source_revision {
                previous.source_revision = None;
            }
            previous.macro_evaluations.extend(segment.macro_evaluations);
            previous
                .regex_applications
                .extend(segment.regex_applications);
            previous.state_mutations.extend(segment.state_mutations);
            continue;
        }
        squashed.push(segment);
    }
    squashed
}

/// Parse the granted, enabled scripts into applicable [`RegexScript`]s, tagging
/// each with its grant digest for application receipts.
fn granted_regex_scripts(scripts: &[PresetScriptMetadata]) -> Vec<RegexScript> {
    scripts
        .iter()
        .filter(|script| script.granted && script.enabled)
        .filter_map(|script| {
            let mut parsed = RegexScript::from_value(&script.metadata)?;
            parsed.id = script.digest.to_string();
            Some(parsed)
        })
        .collect()
}

fn scrub_reasoning(events: &[ProviderEvent], scripts: &[RegexScript]) -> Option<String> {
    let reasoning_scripts: Vec<&RegexScript> = scripts
        .iter()
        .filter(|script| {
            !script.disabled
                && script
                    .placements
                    .contains(&RegexPlacement::Reasoning.code())
        })
        .collect();
    if reasoning_scripts.is_empty() {
        return None;
    }
    let mut reasoning = String::new();
    for event in events {
        if let ProviderEvent::ReasoningDelta { text } = event {
            reasoning.push_str(text);
        }
    }
    if reasoning.is_empty() {
        return None;
    }
    let worker = match EcmaRegexWorker::current(std::time::Duration::from_millis(250)) {
        Ok(worker) => worker,
        Err(_) => return Some(reasoning),
    };
    let finder = |pattern: &str, flags: &str, text: &str| worker.find_matches(pattern, flags, text);
    let mut current = reasoning;
    for script in &reasoning_scripts {
        let matches = match finder(&script.find_pattern, &script.find_flags, &current) {
            Ok(matches) => matches,
            Err(_) => continue,
        };
        if matches.is_empty() {
            continue;
        }
        let mut output = String::with_capacity(current.len());
        let mut cursor = 0;
        for matched in &matches {
            output.push_str(&current[cursor..matched.start]);
            let replacement = regex_script::expand_replacement(
                &script.replace_string,
                matched,
                &script.trim_strings,
            );
            output.push_str(&replacement);
            cursor = matched.end;
        }
        output.push_str(&current[cursor..]);
        current = output;
    }
    Some(current)
}

/// Apply user-input and AI-output regex scripts to the assembled prompt
/// segments, transforming each user/assistant message by its placement and
/// depth. System segments carry neither placement and are left untouched. Token
/// counts are recomputed for any segment a script rewrites.
#[allow(clippy::too_many_arguments)]
fn apply_regex_scripts_to_segments(
    scripts: &[RegexScript],
    segments: &mut [PromptSegment],
    tokenizer: TokenizerId,
    engine: &mut MacroEngine,
    context: &MacroContext,
    state: &mut StateTransaction,
    evaluations: &mut Vec<MacroEvaluation>,
    warnings: &mut Vec<MacroWarning>,
) -> Result<Vec<RegexScriptApplication>, TurnError> {
    if scripts.is_empty() {
        return Ok(Vec::new());
    }
    let worker = EcmaRegexWorker::current(std::time::Duration::from_millis(250))
        .map_err(TurnError::Regex)?;
    let mut finder =
        |pattern: &str, flags: &str, text: &str| worker.find_matches(pattern, flags, text);
    let chat_indices = segments
        .iter()
        .enumerate()
        .filter(|(_, segment)| matches!(segment.role, ChatRole::User | ChatRole::Assistant))
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let total = chat_indices.len();
    let mut applications = Vec::new();
    for (rank, &index) in chat_indices.iter().enumerate() {
        let channel = match segments[index].role {
            ChatRole::User => RegexPlacement::UserInput,
            ChatRole::Assistant => RegexPlacement::AiOutput,
            ChatRole::System => continue,
        };
        let depth = (total - 1 - rank) as i64;
        let before = state.mutations();
        let mut segment_evaluations = Vec::new();
        let mut segment_warnings = Vec::new();
        let mut expander = |input: &str, escape: bool| -> String {
            let rendered = if escape {
                engine.render_with_transform(
                    input,
                    context,
                    state,
                    Some(|value: &str| regex_script::regex_escape(value)),
                )
            } else {
                engine.render(input, context, state)
            };
            match rendered {
                Ok(rendered) => {
                    segment_evaluations.extend(rendered.evaluations);
                    segment_warnings.extend(rendered.warnings);
                    rendered.text
                }
                Err(_) => input.to_owned(),
            }
        };
        let (content, applied) = apply_scripts(
            scripts,
            channel,
            depth,
            &segments[index].content,
            &mut finder,
            &mut Some(&mut expander),
        )
        .map_err(TurnError::Regex)?;
        let state_mutations = state_mutation_delta(&before, &state.mutations());
        evaluations.extend(segment_evaluations.iter().cloned());
        warnings.extend(segment_warnings);
        segments[index]
            .macro_evaluations
            .extend(segment_evaluations);
        segments[index].state_mutations.extend(state_mutations);
        if !applied.is_empty() {
            segments[index].token_count = tokenizer.count(&content);
            segments[index].content = content;
            segments[index]
                .regex_applications
                .extend(applied.iter().cloned());
            applications.extend(applied);
        }
    }
    Ok(applications)
}

pub fn transform_preset_content(
    profile_id: &str,
    source_revision: &ContentHash,
    content: &Value,
    granted_digests: &[ContentHash],
) -> PresetTransformationResult {
    let scripts = content
        .pointer("/extensions/regex_scripts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|script| {
            let digest = crate::canonical_json_hash("stcli:preset-script:v1", script).ok()?;
            let enabled = !script
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(PresetScriptMetadata {
                granted: granted_digests.contains(&digest),
                digest,
                source: ScriptSource::Preset,
                enabled,
                placement: script.get("placement").cloned().unwrap_or(Value::Null),
                metadata: script.clone(),
            })
        })
        .collect::<Vec<_>>();
    let warnings = preset_compatibility_warnings(profile_id, source_revision, content, &scripts);
    PresetTransformationResult {
        content: content.clone(),
        scripts,
        warnings,
    }
}

pub fn extract_character_scripts(
    character_semantic: &Value,
    granted_digests: &[ContentHash],
) -> Vec<PresetScriptMetadata> {
    let data = character_semantic
        .get("data")
        .and_then(Value::as_object)
        .map(|data| &data["extensions"])
        .or_else(|| character_semantic.get("extensions"));
    let scripts_value = data
        .and_then(|extensions| extensions.get("regex_scripts"))
        .and_then(Value::as_array);
    scripts_value
        .into_iter()
        .flatten()
        .filter_map(|script| {
            let digest = crate::canonical_json_hash("stcli:preset-script:v1", script).ok()?;
            let enabled = !script
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            Some(PresetScriptMetadata {
                granted: granted_digests.contains(&digest),
                digest,
                source: ScriptSource::Character,
                enabled,
                placement: script.get("placement").cloned().unwrap_or(Value::Null),
                metadata: script.clone(),
            })
        })
        .collect()
}

fn disabled_structural_markers(preset: &Value, overrides: &BTreeMap<String, bool>) -> Vec<String> {
    let markers = preset
        .get("prompts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|prompt| prompt.get("marker").and_then(Value::as_bool) == Some(true))
        .filter_map(|prompt| prompt.get("identifier").and_then(Value::as_str))
        .collect::<BTreeSet<_>>();
    PromptPreset::parse(preset, CHAT_COMPLETION_CHARACTER_ID)
        .map(|prompt| {
            prompt
                .order
                .into_iter()
                .filter(|entry| markers.contains(entry.identifier.as_str()))
                .filter(|entry| {
                    !overrides
                        .get(&entry.identifier)
                        .copied()
                        .unwrap_or(entry.enabled)
                })
                .map(|entry| entry.identifier)
                .collect()
        })
        .unwrap_or_default()
}

fn preset_compatibility_warnings(
    profile_id: &str,
    source_revision: &ContentHash,
    preset: &Value,
    scripts: &[PresetScriptMetadata],
) -> Vec<CompatibilityWarning> {
    let mut warnings = Vec::new();
    let unauthorized = scripts
        .iter()
        .filter(|script| script.enabled && !script.granted)
        .collect::<Vec<_>>();
    if !unauthorized.is_empty() {
        warnings.push(CompatibilityWarning {
            code: "preset-scripts-not-authorized".to_owned(),
            profile_id: profile_id.to_owned(),
            non_blocking: true,
            source_revision: source_revision.clone(),
            affected_identifiers: unauthorized
                .iter()
                .map(|script| script.digest.to_string())
                .collect(),
            count: unauthorized.len(),
            detail: "Enabled preset scripts have no matching Preset Script Grant.".to_owned(),
        });
    }
    let unhandled_placement = scripts
        .iter()
        .filter(|script| script.enabled && script.granted)
        .filter(|script| {
            let placements = script
                .placement
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_u64)
                .collect::<Vec<_>>();
            !placements.iter().any(|code| {
                *code == RegexPlacement::CODE_USER_INPUT || *code == RegexPlacement::CODE_AI_OUTPUT
            })
        })
        .collect::<Vec<_>>();
    if !unhandled_placement.is_empty() {
        warnings.push(CompatibilityWarning {
            code: "preset-scripts-placement-unsupported".to_owned(),
            profile_id: profile_id.to_owned(),
            non_blocking: true,
            source_revision: source_revision.clone(),
            affected_identifiers: unhandled_placement
                .iter()
                .map(|script| script.digest.to_string())
                .collect(),
            count: unhandled_placement.len(),
            detail: "Granted preset scripts target placements the engine does not yet apply \
                     (only user-input and AI-output run)."
                .to_owned(),
        });
    }
    if profile_id == "sillytavern-1.18-core"
        && let Ok(profile) = serde_json::from_str::<crate::CompatibilityProfile>(include_str!(
            "../../../compat/profiles/sillytavern-1.18-core.json"
        ))
        && let Some(fields) = preset.as_object()
    {
        let exact_order_profile = preset
            .get("prompt_order")
            .and_then(Value::as_array)
            .is_some_and(|profiles| {
                profiles.iter().any(|profile| {
                    profile.get("character_id").and_then(Value::as_u64)
                        == Some(CHAT_COMPLETION_CHARACTER_ID)
                })
            });
        if !exact_order_profile {
            warnings.push(CompatibilityWarning {
            code: "prompt-order-profile-fallback".to_owned(),
            profile_id: profile_id.to_owned(),
            non_blocking: true,
            source_revision: source_revision.clone(),
            affected_identifiers: vec![CHAT_COMPLETION_CHARACTER_ID.to_string()],
            count: 1,
            detail: format!(
                "Prompt order profile {CHAT_COMPLETION_CHARACTER_ID} is absent; the first available order is used."
            ),
        });
        }
        for field in fields.keys() {
            let outcome = profile.preset_fields.get(field);
            let (code, detail) = match outcome {
                Some(crate::CompatibilityOutcome::DocumentedFallback) => (
                    "preset-field-documented-fallback",
                    "The preset field uses the Compatibility Profile's documented fallback.",
                ),
                Some(crate::CompatibilityOutcome::HardUnsupported) => (
                    "preset-field-hard-unsupported",
                    "The preset field is unsupported by the selected Compatibility Profile.",
                ),
                None => (
                    "preset-field-unclassified",
                    "The preset field has no declared Compatibility Profile outcome.",
                ),
                _ => continue,
            };
            warnings.push(CompatibilityWarning {
                code: code.to_owned(),
                profile_id: profile_id.to_owned(),
                non_blocking: true,
                source_revision: source_revision.clone(),
                affected_identifiers: vec![field.clone()],
                count: 1,
                detail: detail.to_owned(),
            });
        }
    }
    warnings
}

fn parse_example_messages(
    input: &str,
    user_name: &str,
    character_name: &str,
) -> Vec<(usize, usize, ChatRole, String)> {
    let mut messages = Vec::<(usize, usize, ChatRole, String)>::new();
    let mut block_index = 0;
    let mut message_index = 0;
    let mut started_block = false;
    for line in input.lines() {
        let line = line.trim();
        if line == "<START>" {
            if started_block {
                block_index += 1;
            }
            message_index = 0;
            started_block = true;
            continue;
        }
        if line.is_empty() {
            continue;
        }
        let parsed = if let Some(content) = line.strip_prefix(&format!("{user_name}:")) {
            Some((ChatRole::User, content.trim().to_owned()))
        } else {
            line.strip_prefix(&format!("{character_name}:"))
                .map(|content| (ChatRole::Assistant, content.trim().to_owned()))
        };
        if let Some((role, content)) = parsed {
            messages.push((block_index, message_index, role, content));
            message_index += 1;
        } else if let Some((_, _, _, content)) = messages.last_mut() {
            content.push('\n');
            content.push_str(line);
        }
    }
    messages
}

fn lore_settings(
    settings: &Value,
    rng_seed: u64,
    message_count: usize,
    prior_activations: BTreeMap<String, usize>,
    generation_type: GenerationType,
) -> LoreSettings {
    let number = |name: &str| settings.get(name).and_then(Value::as_u64);
    let flag = |name: &str, default: bool| {
        settings
            .get(name)
            .and_then(Value::as_bool)
            .unwrap_or(default)
    };
    let max_context = number("max_context").unwrap_or(8_192);
    let budget_percent = number("world_info_budget").unwrap_or(25);
    let budget_cap = number("world_info_budget_cap").unwrap_or(0);
    let mut budget_tokens = max_context.saturating_mul(budget_percent) / 100;
    if budget_cap > 0 {
        budget_tokens = budget_tokens.min(budget_cap);
    }
    LoreSettings {
        scan_depth: number("world_info_scan_depth").unwrap_or(2) as usize,
        recursive: flag("world_info_recursive", true),
        max_recursion_steps: number("world_info_max_recursion_steps").unwrap_or(32) as usize,
        budget_tokens: budget_tokens.max(1) as usize,
        case_sensitive: flag("world_info_case_sensitive", false),
        match_whole_words: flag("world_info_match_whole_words", false),
        use_group_scoring: flag("world_info_use_group_scoring", false),
        generation_type: generation_type.as_str().to_owned(),
        rng_seed,
        message_count,
        prior_activations,
    }
}

fn push_lore_segment(
    segments: &mut Vec<PromptSegment>,
    tokenizer: TokenizerId,
    activated: &[ActivatedLore],
    effects: &BTreeMap<String, (String, Vec<MacroEvaluation>, Vec<StateMutation>)>,
    position: LorePosition,
    source: &str,
) {
    if position == LorePosition::AtDepth {
        let mut grouped = BTreeMap::<(usize, i64), Vec<&ActivatedLore>>::new();
        for entry in activated.iter().filter(|entry| entry.position == position) {
            grouped
                .entry((entry.depth, entry.role))
                .or_default()
                .push(entry);
        }
        for ((depth, role), entries) in grouped {
            let content = entries
                .iter()
                .rev()
                .map(|entry| entry.content.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let mut segment = PromptSegment::new(
                tokenizer,
                source,
                "worldInfoDepth",
                lore_role(role),
                content,
            );
            segment.in_chat_depth = Some(depth);
            segment.source_field = Some("entries".to_owned());
            segment.truncation_priority = 300;
            apply_lore_segment_metadata(&mut segment, &entries, effects);
            segments.push(segment);
        }
        return;
    }
    let entries = activated
        .iter()
        .filter(|entry| entry.position == position)
        .collect::<Vec<_>>();
    let content = entries
        .iter()
        .rev()
        .map(|entry| entry.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    if !content.is_empty() {
        let slot = match position {
            LorePosition::Before => "worldInfoBefore",
            LorePosition::After => "worldInfoAfter",
            LorePosition::ExampleTop | LorePosition::ExampleBottom => "dialogueExamples",
            LorePosition::AuthorNoteTop | LorePosition::AuthorNoteBottom => "authorsNote",
            LorePosition::Outlet | LorePosition::AtDepth => "worldInfoDepth",
        };
        let mut segment = PromptSegment::new(tokenizer, source, slot, ChatRole::System, content);
        segment.source_field = Some("entries".to_owned());
        segment.truncation_priority = 300;
        apply_lore_segment_metadata(&mut segment, &entries, effects);
        segments.push(segment);
    }
}

fn apply_lore_segment_metadata(
    segment: &mut PromptSegment,
    entries: &[&ActivatedLore],
    effects: &BTreeMap<String, (String, Vec<MacroEvaluation>, Vec<StateMutation>)>,
) {
    segment.raw_content = entries
        .iter()
        .rev()
        .filter_map(|entry| {
            effects
                .get(&entry.entry_key)
                .map(|effect| effect.0.as_str())
        })
        .collect::<Vec<_>>()
        .join("\n");
    if let Some(first) = entries.first()
        && entries
            .iter()
            .all(|entry| entry.source_revision == first.source_revision)
    {
        segment.source_revision = Some(first.source_revision.clone());
    }
    for entry in entries {
        if let Some((_, macro_evaluations, state_mutations)) = effects.get(&entry.entry_key) {
            segment
                .macro_evaluations
                .extend(macro_evaluations.iter().cloned());
            segment
                .state_mutations
                .extend(state_mutations.iter().cloned());
        }
    }
}

fn lore_role(role: i64) -> ChatRole {
    match role {
        1 => ChatRole::User,
        2 => ChatRole::Assistant,
        _ => ChatRole::System,
    }
}

fn render_macro_text(
    engine: &mut MacroEngine,
    context: &MacroContext,
    state: &mut crate::StateTransaction,
    input: &str,
    evaluations: &mut Vec<MacroEvaluation>,
    warnings: &mut Vec<MacroWarning>,
) -> Result<String, TurnError> {
    let rendered = engine.render(input, context, state)?;
    evaluations.extend(rendered.evaluations);
    warnings.extend(rendered.warnings);
    Ok(rendered.text)
}

fn render_segment_macro_text(
    engine: &mut MacroEngine,
    context: &MacroContext,
    state: &mut crate::StateTransaction,
    input: &str,
    evaluations: &mut Vec<MacroEvaluation>,
    warnings: &mut Vec<MacroWarning>,
) -> Result<RenderedPromptContent, TurnError> {
    let before = state.mutations();
    let rendered = engine.render(input, context, state)?;
    let state_mutations = state_mutation_delta(&before, &state.mutations());
    evaluations.extend(rendered.evaluations.iter().cloned());
    warnings.extend(rendered.warnings);
    Ok(RenderedPromptContent {
        content: rendered.text,
        macro_evaluations: rendered.evaluations,
        state_mutations,
    })
}

fn state_mutation_delta(before: &[StateMutation], after: &[StateMutation]) -> Vec<StateMutation> {
    after
        .iter()
        .filter_map(|mutation| {
            let prior = before.iter().find(|prior| prior.key == mutation.key);
            if prior == Some(mutation) {
                return None;
            }
            Some(StateMutation {
                key: mutation.key.clone(),
                before: prior
                    .and_then(|prior| prior.after.clone())
                    .or_else(|| mutation.before.clone()),
                after: mutation.after.clone(),
            })
        })
        .collect()
}

fn push_segment(
    segments: &mut Vec<PromptSegment>,
    tokenizer: TokenizerId,
    source: impl Into<String>,
    role: ChatRole,
    raw_content: String,
    rendered: RenderedPromptContent,
) {
    let source = source.into();
    let slot = slot_for_source(&source);
    let mut segment = PromptSegment::new(tokenizer, &source, slot, role, rendered.content);
    segment.raw_content = raw_content;
    segment.macro_evaluations = rendered.macro_evaluations;
    segment.state_mutations = rendered.state_mutations;
    segment.truncation_priority = if source == "main-prompt" || source == "current-user-action" {
        u32::MAX
    } else if source.starts_with("character-") {
        800
    } else if source == "greeting" {
        75
    } else if source.starts_with("turn:") {
        100
    } else {
        300
    };
    segments.push(segment);
}

fn slot_for_source(source: &str) -> &'static str {
    if source == "main-prompt" {
        "main"
    } else if source == "persona-description" {
        "personaDescription"
    } else if source.starts_with("character-") {
        "charDescription"
    } else if source == "world-info-before" {
        "worldInfoBefore"
    } else if source == "world-info-after" {
        "worldInfoAfter"
    } else if source == "current-user-action" {
        "userInput"
    } else {
        "chatHistory"
    }
}

fn decode_turn(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnProjection> {
    Ok(TurnProjection {
        turn_id: parse_column(row, 0)?,
        session_id: parse_column(row, 1)?,
        branch_id: parse_column(row, 2)?,
        user_content: row.get(3)?,
        selected_candidate_id: parse_optional_column(row, 4)?,
        hidden: row.get::<_, i64>(6)? != 0,
        created_event_id: row.get(5)?,
    })
}

pub(crate) fn decode_attempt(row: &rusqlite::Row<'_>) -> rusqlite::Result<AttemptProjection> {
    let status: String = row.get(4)?;
    let prompt_plan: Vec<u8> = row.get(5)?;
    let request_hash: Option<String> = row.get(6)?;
    let receipt: Option<Vec<u8>> = row.get(7)?;
    let effect_receipt: Option<Vec<u8>> = row.get(8)?;
    Ok(AttemptProjection {
        attempt_id: parse_column(row, 0)?,
        turn_id: parse_column(row, 1)?,
        config_hash: parse_column(row, 2)?,
        retry_of_attempt_id: parse_optional_column(row, 3)?,
        status: match status.as_str() {
            "running" => AttemptStatus::Running,
            "completed" => AttemptStatus::Completed,
            "failed" => AttemptStatus::Failed,
            "cancelled" => AttemptStatus::Cancelled,
            "incomplete" => AttemptStatus::Incomplete,
            _ => return Err(conversion_error(4, InvalidStatus(status))),
        },
        prompt_plan: serde_json::from_slice(&prompt_plan)
            .map_err(|error| conversion_error(5, error))?,
        provider_request_hash: request_hash
            .map(|value| value.parse().map_err(|error| conversion_error(6, error)))
            .transpose()?,
        provider_receipt: receipt
            .map(|value| serde_json::from_slice(&value).map_err(|error| conversion_error(7, error)))
            .transpose()?,
        effect_receipt: effect_receipt
            .map(|value| serde_json::from_slice(&value).map_err(|error| conversion_error(8, error)))
            .transpose()?,
        error_message: row.get(9)?,
        created_event_id: row.get(10)?,
        completed_event_id: row.get(11)?,
    })
}

pub(crate) fn decode_candidate(row: &rusqlite::Row<'_>) -> rusqlite::Result<CandidateProjection> {
    let origin: String = row.get(4)?;
    Ok(CandidateProjection {
        candidate_id: parse_column(row, 0)?,
        turn_id: parse_column(row, 1)?,
        attempt_id: parse_optional_column(row, 2)?,
        parent_candidate_id: parse_optional_column(row, 3)?,
        origin: match origin.as_str() {
            "generated" => CandidateOrigin::Generated,
            "continued" => CandidateOrigin::Continued,
            "manual" => CandidateOrigin::Manual,
            "accepted-partial" => CandidateOrigin::AcceptedPartial,
            _ => return Err(conversion_error(4, InvalidOrigin(origin))),
        },
        content: row.get(5)?,
        rendered_content: None,
        hidden: row.get::<_, i64>(7)? != 0,
        created_event_id: row.get(6)?,
    })
}

fn parse_column<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<T>
where
    T: std::str::FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    let value: String = row.get(index)?;
    value
        .parse()
        .map_err(|error| conversion_error(index, error))
}

fn parse_optional_column<T>(row: &rusqlite::Row<'_>, index: usize) -> rusqlite::Result<Option<T>>
where
    T: std::str::FromStr,
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

#[derive(Debug, Error)]
#[error("invalid attempt status '{0}'")]
struct InvalidStatus(String);

#[derive(Debug, Error)]
#[error("invalid candidate origin '{0}'")]
struct InvalidOrigin(String);

#[derive(Debug, Error)]
pub enum TurnError {
    #[error("turn storage failed: {0}")]
    Storage(#[from] StorageError),
    #[error("session operation failed: {0}")]
    Session(#[from] SessionError),
    #[error("artifact operation failed: {0}")]
    Artifact(#[from] ArtifactError),
    #[error("provider operation failed: {0}")]
    Provider(#[from] ProviderError),
    #[error("prompt operation failed: {0}")]
    Prompt(#[from] PromptError),
    #[error("tokenizer operation failed: {0}")]
    Tokenizer(#[from] TokenizerError),
    #[error("macro operation failed: {0}")]
    Macro(#[from] MacroError),
    #[error("lore operation failed: {0}")]
    Lore(#[from] LoreError),
    #[error("regex script failed: {0}")]
    Regex(#[from] EcmaRegexError),
    #[error("state operation failed: {0}")]
    State(#[from] StateError),
    #[error("Plugin operation failed: {0}")]
    Plugin(#[from] PluginError),
    #[error("turn JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session {0} was not found")]
    SessionNotFound(EntityId),
    #[error("branch {0} was not found")]
    BranchNotFound(EntityId),
    #[error("turn {0} was not found")]
    TurnNotFound(EntityId),
    #[error("attempt {0} was not found")]
    AttemptNotFound(EntityId),
    #[error("attempt {attempt_id} is {status:?}, not running")]
    AttemptNotRunning {
        attempt_id: EntityId,
        status: AttemptStatus,
    },
    #[error("candidate {0} was not found")]
    CandidateNotFound(EntityId),
    #[error("turn {0} has no selected candidate")]
    TurnHasNoSelection(EntityId),
    #[error("attempt {0} belongs to the first Turn of its Branch and has no previous Turn")]
    NoPreviousTurnForAttempt(EntityId),
    #[error(
        "attempt {attempt_id} cannot diff the previous Turn {previous_turn_id}: its selected Candidate has no Generation Attempt"
    )]
    PreviousTurnSelectionHasNoGenerationAttempt {
        attempt_id: EntityId,
        previous_turn_id: EntityId,
    },
    #[error("candidate belongs to another turn")]
    CandidateTurnMismatch,
    #[error("branch ancestry contains a cycle at {0}")]
    BranchCycle(EntityId),
    #[error("branch fork turn {0} was not found in its parent history")]
    ForkTurnNotFound(EntityId),
    #[error("retry attempt belongs to another turn")]
    RetryAttemptMismatch,
    #[error("branch belongs to another session")]
    BranchSessionMismatch,
    #[error("provider request hash changed between preparation and execution")]
    ProviderRequestHashMismatch,
    #[error("attempt {0} has no complete effect receipt")]
    AttemptEffectReceiptMissing(EntityId),
    #[error("attempt {0} is still running and cannot be rerun")]
    AttemptStillRunning(EntityId),
    #[error("Plugin '{0}' with its pinned digest is not installed")]
    PluginNotInstalled(String),
    #[error("Plugin version '{0}' is invalid")]
    PluginVersion(String),
    #[error("Plugin '{id}' aborted preparation with {code}: {message}")]
    PluginAbort {
        id: String,
        code: String,
        message: String,
    },
    #[error("Plugin returned invalid prompt role '{0}'")]
    InvalidPluginRole(String),
    #[error("configuration revision {0} was not found")]
    ConfigurationNotFound(ContentHash),
    #[error("character card data is missing")]
    CharacterDataMissing,
    #[error("generation settings must be an object")]
    InvalidGenerationSettings,
    #[error("content exceeds {limit} byte limit ({size} bytes)")]
    ContentTooLarge { size: usize, limit: usize },
    #[error("branch nesting exceeds {limit} level limit ({depth} levels)")]
    BranchTooDeep { depth: usize, limit: usize },
}

fn check_content_size(content: &str) -> Result<(), TurnError> {
    if content.len() > crate::limits::MAX_MESSAGE_CONTENT_BYTES {
        return Err(TurnError::ContentTooLarge {
            size: content.len(),
            limit: crate::limits::MAX_MESSAGE_CONTENT_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup_store_with_turn(candidate_count: usize) -> (Store, EntityId, EntityId, Vec<EntityId>) {
        let dir = tempdir().unwrap();
        let store = Store::open(dir.path().join("test.sqlite3")).unwrap();

        let session_id = EntityId::new();
        let branch_id = EntityId::new();
        let turn_id = EntityId::new();
        let event_id = EntityId::new();

        store
            .connection
            .execute_batch("PRAGMA foreign_keys = OFF;")
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, created_event_id) VALUES (?1, 'cfg', ?2, 0, ?3)",
                params![session_id.to_string(), branch_id.to_string(), event_id.to_string()],
            )
            .unwrap();
        store
            .connection
            .execute(
                "INSERT INTO branches(branch_id, session_id, parent_branch_id, greeting_revision_hash, greeting_index, created_event_id, deleted) VALUES (?1, ?2, NULL, 'rev', 0, ?3, 0)",
                params![branch_id.to_string(), session_id.to_string(), event_id.to_string()],
            )
            .unwrap();

        let mut candidate_ids = Vec::new();
        let first_candidate = EntityId::new();
        store
            .connection
            .execute(
                "INSERT INTO turns(turn_id, session_id, branch_id, user_content, selected_candidate_id, created_event_id) VALUES (?1, ?2, ?3, 'hello', ?4, ?5)",
                params![turn_id.to_string(), session_id.to_string(), branch_id.to_string(), first_candidate.to_string(), event_id.to_string()],
            )
            .unwrap();

        for i in 0..candidate_count {
            let cid = if i == 0 {
                first_candidate
            } else {
                EntityId::new()
            };
            store
                .connection
                .execute(
                    "INSERT INTO candidates(candidate_id, turn_id, attempt_id, origin, content, created_event_id) VALUES (?1, ?2, NULL, 'manual', ?3, ?4)",
                    params![cid.to_string(), turn_id.to_string(), format!("response {i}"), event_id.to_string()],
                )
                .unwrap();
            candidate_ids.push(cid);
        }

        store
            .connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();

        (store, turn_id, session_id, candidate_ids)
    }

    #[test]
    fn delete_candidate_auto_selects_next() {
        let (mut store, turn_id, _, candidates) = setup_store_with_turn(3);
        let selected = candidates[0];

        store.delete_candidate(selected).unwrap();

        let turn = store.turn(turn_id).unwrap().unwrap();
        assert!(
            turn.selected_candidate_id.is_some(),
            "should auto-select another candidate after deletion"
        );
        assert_ne!(turn.selected_candidate_id.unwrap(), selected);

        let remaining = store.candidates_for_turn(turn_id).unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|c| c.candidate_id != selected));
    }

    #[test]
    fn delete_last_candidate_leaves_turn_with_null_selection() {
        let (mut store, turn_id, _, candidates) = setup_store_with_turn(1);

        store.delete_candidate(candidates[0]).unwrap();

        let turn = store.turn(turn_id).unwrap().unwrap();
        assert_eq!(turn.selected_candidate_id, None);
        assert!(store.candidates_for_turn(turn_id).unwrap().is_empty());
    }

    #[test]
    fn segment_state_mutations_preserve_each_write_boundary() {
        let key = crate::StateKey {
            scope: crate::VariableScope::Local,
            name: "mood".to_owned(),
        };
        let cell = |revision, raw_value: &str| crate::StateCell {
            key: key.clone(),
            value: json!(raw_value),
            raw_value: raw_value.to_owned(),
            owner: "macro".to_owned(),
            origin: "setvar".to_owned(),
            revision,
        };
        let first = StateMutation {
            key: key.clone(),
            before: None,
            after: Some(cell(1, "calm")),
        };
        let second = StateMutation {
            key: key.clone(),
            before: None,
            after: Some(cell(2, "alert")),
        };

        let delta =
            state_mutation_delta(std::slice::from_ref(&first), std::slice::from_ref(&second));

        assert_eq!(
            delta,
            vec![StateMutation {
                before: first.after,
                ..second
            }]
        );
    }

    #[test]
    fn extract_character_scripts_from_card_v2() {
        let card: Value = serde_json::from_str(
            r#"{
                "spec": "chara_card_v2",
                "data": {
                    "name": "Test",
                    "extensions": {
                        "regex_scripts": [
                            {
                                "id": "strip-ooc",
                                "scriptName": "Strip OOC",
                                "findRegex": "/\\(OOC:.*?\\)/g",
                                "replaceString": "",
                                "placement": [2]
                            }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        let scripts = extract_character_scripts(&card, &[]);
        assert_eq!(scripts.len(), 1);
        assert_eq!(scripts[0].source, ScriptSource::Character);
        assert!(!scripts[0].granted, "ungranted without matching digest");
    }

    #[test]
    fn character_script_granted_when_digest_matches() {
        let card: Value = serde_json::from_str(
            r#"{
                "spec": "chara_card_v2",
                "data": {
                    "name": "Test",
                    "extensions": {
                        "regex_scripts": [
                            {
                                "id": "bold",
                                "scriptName": "Bold",
                                "findRegex": "/plain/g",
                                "replaceString": "**bold**",
                                "placement": [2]
                            }
                        ]
                    }
                }
            }"#,
        )
        .unwrap();
        let scripts_no_grant = extract_character_scripts(&card, &[]);
        let digest = scripts_no_grant[0].digest.clone();
        let scripts_granted = extract_character_scripts(&card, &[digest]);
        assert!(scripts_granted[0].granted);
    }

    #[test]
    fn unified_grants_enforce_across_sources() {
        let preset_script = json!({
            "id": "ps",
            "scriptName": "Preset Script",
            "findRegex": "/foo/g",
            "replaceString": "bar",
            "placement": [1]
        });
        let character_card: Value = serde_json::from_str(
            r#"{
                "spec": "chara_card_v2",
                "data": {
                    "name": "Test",
                    "extensions": {
                        "regex_scripts": [{
                            "id": "cs",
                            "scriptName": "Char Script",
                            "findRegex": "/baz/g",
                            "replaceString": "qux",
                            "placement": [2]
                        }]
                    }
                }
            }"#,
        )
        .unwrap();
        let preset_result = transform_preset_content(
            "sillytavern-1.18-core",
            &crate::ContentHash::new([0u8; 32]),
            &json!({"extensions": {"regex_scripts": [preset_script]}}),
            &[],
        );
        let char_scripts = extract_character_scripts(&character_card, &[]);
        let mut all = preset_result.scripts;
        all.extend(char_scripts);
        let granted = granted_regex_scripts(&all);
        assert!(
            granted.is_empty(),
            "no scripts should be granted without digests"
        );
    }
}
