use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ArtifactError, ArtifactKind, ArtifactRecord, AttemptProjection, BranchProjection,
    CandidateProjection, CapsuleError, CapsuleKind, CompactionReport, CompletedTurn, ContentHash,
    CreatedSession, DryRunResult, EcmaRegexWorker, EditedCandidate, EntityId, ImportedCapsule,
    InstalledPlugin, PluginCapability, PluginCommandResult, PluginError, PluginPin, PluginRegistry,
    PromptDiff, PromptPlan, PromptSegmentInspection, ProviderEvent, RecoveryReport, ReplayReport,
    SessionConfiguration, SessionConfigurationRecord, SessionError, SessionProjection,
    StorageError, Store, TokenizerError, TokenizerId, TurnCapsule, TurnError, TurnProjection,
    apply_display_scripts, diff_prompt_plans, extract_character_scripts, transform_preset_content,
};

#[derive(Clone, Debug)]
pub struct StcliEngine {
    database: PathBuf,
}

impl StcliEngine {
    pub fn new(database: impl AsRef<Path>) -> Self {
        Self {
            database: database.as_ref().to_owned(),
        }
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    fn plugin_registry(&self) -> PluginRegistry {
        PluginRegistry::new(
            self.database
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("plugins"),
        )
    }

    fn installed_plugin(
        &self,
        id: &str,
        version: &str,
        digest: &ContentHash,
    ) -> Result<InstalledPlugin, EngineError> {
        self.plugin_registry()
            .list()?
            .into_iter()
            .find(|plugin| {
                plugin.manifest.id == id
                    && plugin.manifest.version.to_string() == version
                    && plugin.manifest.component_sha256 == *digest
            })
            .ok_or_else(|| EngineError::PluginNotFound {
                id: id.to_owned(),
                version: version.to_owned(),
                digest: digest.clone(),
            })
    }

    pub fn inspect(&self, query: EngineQuery) -> Result<EngineInspection, EngineError> {
        if let EngineQuery::DoctorPlugin { directory } = &query {
            return Ok(EngineInspection::InstalledPlugin(
                self.plugin_registry().doctor(directory)?,
            ));
        }
        if let EngineQuery::Plugins { plugin_id } = &query {
            let plugins = self
                .plugin_registry()
                .list()?
                .into_iter()
                .filter(|plugin| {
                    plugin_id
                        .as_ref()
                        .is_none_or(|expected| plugin.manifest.id == *expected)
                })
                .collect();
            return Ok(EngineInspection::Plugins(plugins));
        }
        let store = Store::open(&self.database)?;
        match query {
            EngineQuery::Sessions => Ok(EngineInspection::Sessions(session_summaries(&store)?)),
            EngineQuery::SessionProjections => {
                Ok(EngineInspection::SessionProjections(store.sessions()?))
            }
            EngineQuery::Session { session_id } => Ok(EngineInspection::Session(
                store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?,
            )),
            EngineQuery::SessionDetails { session_id } => {
                let session = store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?;
                let configuration = store.configuration(&session.current_config_hash)?;
                let branches = store.branches(session_id)?;
                let discovered_scripts = configuration
                    .as_ref()
                    .map(|config_record| {
                        let config = &config_record.configuration;
                        let mut scripts = config
                            .prompt_preset_revision
                            .as_ref()
                            .and_then(|rev| store.decoded_artifact(rev).ok())
                            .map(|artifact| {
                                transform_preset_content(
                                    &config.compatibility_profile,
                                    config.prompt_preset_revision.as_ref().unwrap(),
                                    &artifact.semantic,
                                    &config.script_grants,
                                )
                                .scripts
                            })
                            .unwrap_or_default();
                        if let Ok(character) = store.decoded_artifact(&config.character_revision) {
                            scripts.extend(extract_character_scripts(
                                &character.semantic,
                                &config.script_grants,
                            ));
                        }
                        scripts
                    })
                    .unwrap_or_default();
                Ok(EngineInspection::SessionDetails(SessionDetails {
                    session,
                    configuration,
                    branches,
                    discovered_scripts,
                }))
            }
            EngineQuery::Branches { session_id } => {
                Ok(EngineInspection::Branches(store.branches(session_id)?))
            }
            EngineQuery::BranchHistory {
                session_id,
                branch_id,
            } => Ok(EngineInspection::BranchHistory(Box::new(branch_history(
                &store, session_id, branch_id,
            )?))),
            EngineQuery::Configuration { session_id } => {
                let session = store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?;
                Ok(EngineInspection::Configuration(
                    store
                        .configuration(&session.current_config_hash)?
                        .ok_or_else(|| {
                            SessionError::ConfigurationNotFound(session.current_config_hash.clone())
                        })?,
                ))
            }
            EngineQuery::Artifacts { kind } => Ok(EngineInspection::Artifacts(
                store
                    .artifacts()?
                    .into_iter()
                    .filter(|artifact| kind.is_none_or(|expected| artifact.kind == expected))
                    .collect(),
            )),
            EngineQuery::Artifact { revision_hash } => Ok(EngineInspection::Artifact(
                store
                    .artifact(&revision_hash)?
                    .ok_or_else(|| ArtifactError::NotFound(revision_hash))?,
            )),
            EngineQuery::ArtifactSource { revision_hash } => Ok(EngineInspection::ArtifactSource(
                store.export_artifact(&revision_hash)?,
            )),
            EngineQuery::BranchTurns { branch_id } => Ok(EngineInspection::Turns(
                store
                    .turns_for_branch(branch_id)?
                    .into_iter()
                    .map(|turn| {
                        Ok(EngineTurn {
                            candidates: store.candidates_for_turn(turn.turn_id)?,
                            attempts: store.attempts_for_turn(turn.turn_id)?,
                            turn,
                        })
                    })
                    .collect::<Result<Vec<_>, TurnError>>()?,
            )),
            EngineQuery::Attempt { attempt_id } => Ok(EngineInspection::Attempt(
                store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?,
            )),
            EngineQuery::TurnDetails {
                session_id,
                attempt_id,
            } => {
                let attempt = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let turn = store
                    .turn(attempt.turn_id)?
                    .ok_or(TurnError::TurnNotFound(attempt.turn_id))?;
                if turn.session_id != session_id {
                    return Err(EngineError::AttemptSessionMismatch);
                }
                let candidate = store
                    .candidates_for_turn(turn.turn_id)?
                    .into_iter()
                    .find(|candidate| candidate.attempt_id == Some(attempt_id));
                Ok(EngineInspection::TurnDetails(Box::new(TurnDetails {
                    turn,
                    attempt,
                    candidate,
                })))
            }
            EngineQuery::PromptPlan { attempt_id } => {
                let attempt = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                Ok(EngineInspection::PromptPlan(attempt.prompt_plan))
            }
            EngineQuery::PromptSegments {
                attempt_id,
                selector,
            } => {
                let attempt = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let inspection = attempt.prompt_plan.inspect_segments(&selector).ok_or(
                    EngineError::PromptSegmentNotFound {
                        attempt_id,
                        selector,
                    },
                )?;
                Ok(EngineInspection::PromptSegments(inspection))
            }
            EngineQuery::PromptDiff {
                baseline_attempt_id,
                target_attempt_id,
            } => {
                let baseline = store
                    .attempt(baseline_attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(baseline_attempt_id))?;
                let target = store
                    .attempt(target_attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(target_attempt_id))?;
                Ok(EngineInspection::PromptDiff(diff_prompt_plans(
                    baseline_attempt_id,
                    &baseline.prompt_plan,
                    target_attempt_id,
                    &target.prompt_plan,
                )))
            }
            EngineQuery::PreviousPromptDiff { attempt_id } => {
                let target = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let baseline = previous_selected_attempt(&store, &target)?;
                Ok(EngineInspection::PromptDiff(diff_prompt_plans(
                    baseline.attempt_id,
                    &baseline.prompt_plan,
                    attempt_id,
                    &target.prompt_plan,
                )))
            }
            EngineQuery::ExportCapsule {
                session_id,
                attempt_id,
                kind,
                redact_content,
            } => {
                ensure_attempt_session(&store, session_id, attempt_id)?;
                Ok(EngineInspection::Capsule(Box::new(
                    store.export_turn_capsule(attempt_id, kind, redact_content)?,
                )))
            }
            EngineQuery::ReplayCapsule { capsule } => Ok(EngineInspection::ReplayReport(
                store.replay_turn_capsule(&capsule)?,
            )),
            EngineQuery::DryRunRerun {
                session_id,
                attempt_id,
            } => {
                let preview = store.dry_run_rerun(attempt_id)?;
                if preview.session_id != session_id {
                    return Err(EngineError::AttemptSessionMismatch);
                }
                Ok(EngineInspection::DryRun(Box::new(preview)))
            }
            EngineQuery::DoctorPlugin { .. } | EngineQuery::Plugins { .. } => unreachable!(),
        }
    }

    pub async fn execute(
        &self,
        command: EngineCommand,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<EngineResult, EngineError> {
        if let EngineCommand::InstallPlugin { directory } = &command {
            return Ok(EngineResult::InstalledPlugin(
                self.plugin_registry().install(directory)?,
            ));
        }
        let mut store = Store::open(&self.database)?;
        match command {
            EngineCommand::InstallPlugin { .. } => unreachable!(),
            EngineCommand::RemovePlugin { plugin_id } => {
                if store.plugin_in_use(&plugin_id)? {
                    return Err(EngineError::PluginInUse(plugin_id));
                }
                Ok(EngineResult::PluginRemoval(PluginRemovalReceipt {
                    removed: self.plugin_registry().remove(&plugin_id)?,
                    id: plugin_id,
                }))
            }
            EngineCommand::AdoptPlugin {
                session_id,
                id,
                version,
                digest,
                capabilities,
                settings,
            } => {
                let installed = self.installed_plugin(&id, &version, &digest)?;
                if !capabilities.is_subset(&installed.manifest.requested_capabilities) {
                    return Err(EngineError::PluginGrantExceeded);
                }
                let mut configuration = selected_session_configuration(&store, session_id)?;
                configuration.plugins.retain(|pin| pin.id != id);
                configuration.plugins.push(PluginPin {
                    id,
                    version: installed.manifest.version.to_string(),
                    component_hash: installed.manifest.component_sha256,
                    capabilities,
                    settings,
                    enabled: true,
                });
                Ok(EngineResult::Configuration(Box::new(
                    store.update_session_configuration(session_id, configuration)?,
                )))
            }
            EngineCommand::UpgradePlugin {
                session_id,
                id,
                version,
                digest,
            } => {
                let installed = self.installed_plugin(&id, &version, &digest)?;
                let mut configuration = selected_session_configuration(&store, session_id)?;
                let pin = configuration
                    .plugins
                    .iter_mut()
                    .find(|pin| pin.id == id)
                    .ok_or_else(|| EngineError::PluginNotPinned(id.clone()))?;
                if !pin
                    .capabilities
                    .is_subset(&installed.manifest.requested_capabilities)
                {
                    return Err(EngineError::PluginUpgradeGrantExceeded);
                }
                pin.version = installed.manifest.version.to_string();
                pin.component_hash = installed.manifest.component_sha256;
                Ok(EngineResult::Configuration(Box::new(
                    store.update_session_configuration(session_id, configuration)?,
                )))
            }
            EngineCommand::SetPluginEnabled {
                session_id,
                id,
                enabled,
            } => {
                let mut configuration = selected_session_configuration(&store, session_id)?;
                let pin = configuration
                    .plugins
                    .iter_mut()
                    .find(|pin| pin.id == id)
                    .ok_or_else(|| EngineError::PluginNotPinned(id))?;
                pin.enabled = enabled;
                Ok(EngineResult::Configuration(Box::new(
                    store.update_session_configuration(session_id, configuration)?,
                )))
            }
            EngineCommand::ImportArtifact { source } => {
                let bundle = store.import_artifact_bundle(&source)?;
                Ok(EngineResult::ArtifactBundle {
                    primary: bundle.primary,
                    supplementary_artifacts: bundle.supplementary_artifacts,
                    asset_count: bundle.asset_count,
                })
            }
            EngineCommand::CreateSession {
                configuration,
                greeting_index,
            } => Ok(EngineResult::CreatedSession(Box::new(
                store.create_session(*configuration, greeting_index)?,
            ))),
            EngineCommand::RenameSession { session_id, name } => {
                store.rename_session(session_id, &name)?;
                let session = store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?;
                Ok(EngineResult::Session(session))
            }
            EngineCommand::ArchiveSession { session_id } => {
                Ok(EngineResult::Session(store.archive_session(session_id)?))
            }
            EngineCommand::PurgeSession { session_id } => Ok(EngineResult::Purge(PurgeReport {
                removed_trace_events: store.purge_session(session_id)?,
            })),
            EngineCommand::CompactSession { session_id } => {
                Ok(EngineResult::Compaction(store.compact_session(session_id)?))
            }
            EngineCommand::Recover => Ok(EngineResult::Recovery(
                store.recover_interrupted_attempts()?,
            )),
            EngineCommand::RebuildSessionProjections => {
                store.rebuild_session_projections()?;
                Ok(EngineResult::Rebuild(RebuildReport {
                    sessions: store.sessions()?.len(),
                }))
            }
            EngineCommand::DeleteBranch { branch_id } => {
                store.delete_branch(branch_id)?;
                Ok(EngineResult::DeletedBranch(DeletionReceipt {
                    entity_id: branch_id,
                    deleted: true,
                }))
            }
            EngineCommand::HideCandidate { candidate_id } => {
                Ok(EngineResult::Candidate(store.hide_candidate(candidate_id)?))
            }
            EngineCommand::DeleteCandidate { candidate_id } => {
                store.delete_candidate(candidate_id)?;
                Ok(EngineResult::DeletedCandidate(DeletionReceipt {
                    entity_id: candidate_id,
                    deleted: true,
                }))
            }
            EngineCommand::HideTurn { turn_id } => {
                Ok(EngineResult::Turn(store.hide_turn(turn_id)?))
            }
            EngineCommand::DeleteTurn { turn_id } => {
                store.delete_turn(turn_id)?;
                Ok(EngineResult::DeletedTurn(DeletionReceipt {
                    entity_id: turn_id,
                    deleted: true,
                }))
            }
            EngineCommand::ImportCapsule { capsule } => Ok(EngineResult::ImportedCapsule(
                store.import_turn_capsule(&capsule)?,
            )),
            EngineCommand::InvokePlugin {
                session_id,
                plugin_id,
                command,
                arguments,
            } => Ok(EngineResult::PluginCommand(Box::new(
                store.invoke_plugin_command(session_id, &plugin_id, &command, arguments)?,
            ))),
            EngineCommand::Send {
                session_id,
                branch_id,
                content,
            } => Ok(EngineResult::CompletedTurn(Box::new(
                store
                    .send_message(session_id, branch_id, content, &mut on_event)
                    .await?,
            ))),
            EngineCommand::Retry {
                turn_id,
                attempt_id,
            } => Ok(EngineResult::CompletedTurn(Box::new(
                store.retry_turn(turn_id, attempt_id, &mut on_event).await?,
            ))),
            EngineCommand::Regenerate { turn_id } => Ok(EngineResult::CompletedTurn(Box::new(
                store.regenerate_turn(turn_id, &mut on_event).await?,
            ))),
            EngineCommand::Continue { turn_id } => Ok(EngineResult::CompletedTurn(Box::new(
                store.continue_turn(turn_id, &mut on_event).await?,
            ))),
            EngineCommand::GenerateSwipe { turn_id } => Ok(EngineResult::CompletedTurn(Box::new(
                store.swipe_turn(turn_id, &mut on_event).await?,
            ))),
            EngineCommand::SelectCandidate {
                turn_id,
                candidate_id,
            } => Ok(EngineResult::Turn(
                store.select_swipe(turn_id, candidate_id)?,
            )),
            EngineCommand::EditUser { turn_id, content } => {
                Ok(EngineResult::CompletedTurn(Box::new(
                    store
                        .edit_user_turn(turn_id, content, &mut on_event)
                        .await?,
                )))
            }
            EngineCommand::EditCandidate {
                candidate_id,
                content,
            } => Ok(EngineResult::EditedCandidate(
                store.edit_candidate(candidate_id, content)?,
            )),
            EngineCommand::Cancel { attempt_id } => Ok(EngineResult::Attempt(Box::new(
                store.cancel_attempt(attempt_id)?,
            ))),
            EngineCommand::SelectGreeting {
                session_id,
                branch_id,
                greeting_index,
            } => Ok(EngineResult::Branch(store.select_greeting(
                session_id,
                branch_id,
                greeting_index,
            )?)),
            EngineCommand::UpdateConfiguration {
                session_id,
                configuration,
            } => Ok(EngineResult::Configuration(Box::new(
                store.update_session_configuration(session_id, *configuration)?,
            ))),
            EngineCommand::DryRunSend {
                session_id,
                branch_id,
                content,
            } => Ok(EngineResult::DryRun(Box::new(
                store.dry_run_message(session_id, branch_id, &content)?,
            ))),
            EngineCommand::DryRunRegenerate { turn_id } => Ok(EngineResult::DryRun(Box::new(
                store.dry_run_regenerate(turn_id)?,
            ))),
            EngineCommand::DryRunContinue { turn_id } => Ok(EngineResult::DryRun(Box::new(
                store.dry_run_continue(turn_id)?,
            ))),
            EngineCommand::DryRunSwipe { turn_id } => Ok(EngineResult::DryRun(Box::new(
                store.dry_run_swipe(turn_id)?,
            ))),
            EngineCommand::Rerun {
                session_id,
                attempt_id,
            } => {
                ensure_attempt_session(&store, session_id, attempt_id)?;
                Ok(EngineResult::CompletedTurn(Box::new(
                    store.rerun_attempt(attempt_id, &mut on_event).await?,
                )))
            }
        }
    }
}

#[derive(Clone, Debug)]
pub enum EngineQuery {
    Sessions,
    SessionProjections,
    Session {
        session_id: EntityId,
    },
    SessionDetails {
        session_id: EntityId,
    },
    Branches {
        session_id: EntityId,
    },
    BranchHistory {
        session_id: EntityId,
        branch_id: EntityId,
    },
    Configuration {
        session_id: EntityId,
    },
    Artifacts {
        kind: Option<ArtifactKind>,
    },
    Artifact {
        revision_hash: ContentHash,
    },
    ArtifactSource {
        revision_hash: ContentHash,
    },
    BranchTurns {
        branch_id: EntityId,
    },
    Attempt {
        attempt_id: EntityId,
    },
    TurnDetails {
        session_id: EntityId,
        attempt_id: EntityId,
    },
    PromptPlan {
        attempt_id: EntityId,
    },
    PromptSegments {
        attempt_id: EntityId,
        selector: String,
    },
    PromptDiff {
        baseline_attempt_id: EntityId,
        target_attempt_id: EntityId,
    },
    PreviousPromptDiff {
        attempt_id: EntityId,
    },
    ExportCapsule {
        session_id: EntityId,
        attempt_id: EntityId,
        kind: CapsuleKind,
        redact_content: bool,
    },
    ReplayCapsule {
        capsule: Box<TurnCapsule>,
    },
    DryRunRerun {
        session_id: EntityId,
        attempt_id: EntityId,
    },
    DoctorPlugin {
        directory: PathBuf,
    },
    Plugins {
        plugin_id: Option<String>,
    },
}

#[derive(Clone, Debug)]
pub enum EngineCommand {
    InstallPlugin {
        directory: PathBuf,
    },
    RemovePlugin {
        plugin_id: String,
    },
    AdoptPlugin {
        session_id: EntityId,
        id: String,
        version: String,
        digest: ContentHash,
        capabilities: BTreeSet<PluginCapability>,
        settings: serde_json::Value,
    },
    UpgradePlugin {
        session_id: EntityId,
        id: String,
        version: String,
        digest: ContentHash,
    },
    SetPluginEnabled {
        session_id: EntityId,
        id: String,
        enabled: bool,
    },
    ImportArtifact {
        source: Vec<u8>,
    },
    CreateSession {
        configuration: Box<SessionConfiguration>,
        greeting_index: usize,
    },
    RenameSession {
        session_id: EntityId,
        name: String,
    },
    ArchiveSession {
        session_id: EntityId,
    },
    PurgeSession {
        session_id: EntityId,
    },
    CompactSession {
        session_id: EntityId,
    },
    Recover,
    RebuildSessionProjections,
    DeleteBranch {
        branch_id: EntityId,
    },
    HideCandidate {
        candidate_id: EntityId,
    },
    DeleteCandidate {
        candidate_id: EntityId,
    },
    HideTurn {
        turn_id: EntityId,
    },
    DeleteTurn {
        turn_id: EntityId,
    },
    ImportCapsule {
        capsule: Box<TurnCapsule>,
    },
    InvokePlugin {
        session_id: EntityId,
        plugin_id: String,
        command: String,
        arguments: serde_json::Value,
    },
    Send {
        session_id: EntityId,
        branch_id: EntityId,
        content: String,
    },
    Retry {
        turn_id: EntityId,
        attempt_id: EntityId,
    },
    Regenerate {
        turn_id: EntityId,
    },
    Continue {
        turn_id: EntityId,
    },
    GenerateSwipe {
        turn_id: EntityId,
    },
    SelectCandidate {
        turn_id: EntityId,
        candidate_id: EntityId,
    },
    EditUser {
        turn_id: EntityId,
        content: String,
    },
    EditCandidate {
        candidate_id: EntityId,
        content: String,
    },
    Cancel {
        attempt_id: EntityId,
    },
    SelectGreeting {
        session_id: EntityId,
        branch_id: EntityId,
        greeting_index: usize,
    },
    UpdateConfiguration {
        session_id: EntityId,
        configuration: Box<SessionConfiguration>,
    },
    DryRunSend {
        session_id: EntityId,
        branch_id: EntityId,
        content: String,
    },
    DryRunRegenerate {
        turn_id: EntityId,
    },
    DryRunContinue {
        turn_id: EntityId,
    },
    DryRunSwipe {
        turn_id: EntityId,
    },
    Rerun {
        session_id: EntityId,
        attempt_id: EntityId,
    },
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "result", content = "data", rename_all = "kebab-case")]
pub enum EngineResult {
    InstalledPlugin(InstalledPlugin),
    PluginRemoval(PluginRemovalReceipt),
    ArtifactBundle {
        primary: ArtifactRecord,
        supplementary_artifacts: Vec<ArtifactRecord>,
        asset_count: usize,
    },
    CreatedSession(Box<CreatedSession>),
    Session(SessionProjection),
    Purge(PurgeReport),
    Compaction(CompactionReport),
    Recovery(RecoveryReport),
    Rebuild(RebuildReport),
    DeletedBranch(DeletionReceipt),
    Candidate(CandidateProjection),
    DeletedCandidate(DeletionReceipt),
    DeletedTurn(DeletionReceipt),
    ImportedCapsule(ImportedCapsule),
    PluginCommand(Box<PluginCommandResult>),
    CompletedTurn(Box<CompletedTurn>),
    Turn(TurnProjection),
    Attempt(Box<AttemptProjection>),
    Branch(BranchProjection),
    Configuration(Box<SessionConfigurationRecord>),
    EditedCandidate(EditedCandidate),
    DryRun(Box<DryRunResult>),
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "inspection", content = "data", rename_all = "kebab-case")]
pub enum EngineInspection {
    Sessions(Vec<SessionSummary>),
    SessionProjections(Vec<SessionProjection>),
    Session(SessionProjection),
    SessionDetails(SessionDetails),
    Branches(Vec<BranchProjection>),
    BranchHistory(Box<BranchHistory>),
    Configuration(SessionConfigurationRecord),
    Turns(Vec<EngineTurn>),
    Artifacts(Vec<ArtifactRecord>),
    Artifact(ArtifactRecord),
    ArtifactSource(Vec<u8>),
    Attempt(AttemptProjection),
    TurnDetails(Box<TurnDetails>),
    PromptPlan(PromptPlan),
    PromptSegments(PromptSegmentInspection),
    PromptDiff(PromptDiff),
    Capsule(Box<TurnCapsule>),
    ReplayReport(ReplayReport),
    DryRun(Box<DryRunResult>),
    InstalledPlugin(InstalledPlugin),
    Plugins(Vec<InstalledPlugin>),
}

#[derive(Clone, Debug, Serialize)]
pub struct SessionDetails {
    pub session: SessionProjection,
    pub configuration: Option<SessionConfigurationRecord>,
    pub branches: Vec<BranchProjection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_scripts: Vec<crate::PresetScriptMetadata>,
}

#[derive(Clone, Debug, Serialize)]
pub struct TurnDetails {
    pub turn: TurnProjection,
    pub attempt: AttemptProjection,
    pub candidate: Option<CandidateProjection>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PluginRemovalReceipt {
    pub id: String,
    pub removed: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PurgeReport {
    pub removed_trace_events: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RebuildReport {
    pub sessions: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DeletionReceipt {
    pub entity_id: EntityId,
    pub deleted: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SessionSummary {
    pub session_id: EntityId,
    pub display_name: String,
    pub created_at_ms: u64,
    pub modified_at_ms: u64,
    pub turn_count: usize,
    pub character_label: String,
    pub persona_label: String,
    pub token_count: usize,
    pub last_message_preview: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct BranchHistory {
    pub session: SessionProjection,
    pub configuration: SessionConfigurationRecord,
    pub branch: BranchProjection,
    pub greeting: Option<GreetingProjection>,
    pub turns: Vec<EngineTurn>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct GreetingProjection {
    pub revision_hash: ContentHash,
    pub index: usize,
    pub total: usize,
    pub content: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EngineTurn {
    pub turn: TurnProjection,
    pub candidates: Vec<CandidateProjection>,
    pub attempts: Vec<AttemptProjection>,
}

fn ensure_attempt_session(
    store: &Store,
    session_id: EntityId,
    attempt_id: EntityId,
) -> Result<(), EngineError> {
    let attempt = store
        .attempt(attempt_id)?
        .ok_or(TurnError::AttemptNotFound(attempt_id))?;
    let turn = store
        .turn(attempt.turn_id)?
        .ok_or(TurnError::TurnNotFound(attempt.turn_id))?;
    if turn.session_id != session_id {
        return Err(EngineError::AttemptSessionMismatch);
    }
    Ok(())
}

fn selected_session_configuration(
    store: &Store,
    session_id: EntityId,
) -> Result<SessionConfiguration, EngineError> {
    let session = store
        .session(session_id)?
        .ok_or(SessionError::SessionNotFound(session_id))?;
    store
        .configuration(&session.current_config_hash)?
        .map(|record| record.configuration)
        .ok_or(EngineError::SelectedSessionConfigurationMissing)
}

fn session_summaries(store: &Store) -> Result<Vec<SessionSummary>, EngineError> {
    store
        .sessions()?
        .into_iter()
        .filter(|session| !session.archived)
        .map(|session| {
            let configuration = store
                .configuration(&session.current_config_hash)?
                .ok_or_else(|| {
                    SessionError::ConfigurationNotFound(session.current_config_hash.clone())
                })?;
            let character =
                store.decoded_artifact(&configuration.configuration.character_revision)?;
            let character_label = character
                .semantic
                .pointer("/data/name")
                .or_else(|| character.semantic.get("name"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or("Character")
                .to_owned();
            let branches = store.branches(session.session_id)?;
            let mut seen_turns = BTreeSet::new();
            let mut token_count = 0;
            let root_branch = branches
                .iter()
                .find(|branch| branch.branch_id == session.root_branch_id)
                .ok_or(SessionError::BranchNotFound(session.root_branch_id))?;
            let mut last_message_preview = truncate_preview(&root_branch.greeting, 200);
            let tokenizer = TokenizerId::from_str(&configuration.configuration.tokenizer)?;
            let root_turns = store.turns_for_branch(session.root_branch_id)?;
            for turn in &root_turns {
                seen_turns.insert(turn.turn_id);
                token_count += tokenizer.count(&turn.user_content);
                if let Some(candidate_id) = turn.selected_candidate_id
                    && let Some(candidate) = store.candidate(candidate_id)?
                {
                    token_count += tokenizer.count(&candidate.content);
                }
            }
            if let Some(last_turn) = root_turns.last() {
                let preview = last_turn
                    .selected_candidate_id
                    .and_then(|id| store.candidate(id).ok().flatten())
                    .map(|c| c.content.clone())
                    .unwrap_or_else(|| last_turn.user_content.clone());
                last_message_preview = truncate_preview(&preview, 200);
            }
            for branch in &branches {
                if branch.branch_id == session.root_branch_id {
                    continue;
                }
                for turn in store.turns_for_branch(branch.branch_id)? {
                    if !seen_turns.insert(turn.turn_id) {
                        continue;
                    }
                    token_count += tokenizer.count(&turn.user_content);
                    if let Some(candidate_id) = turn.selected_candidate_id
                        && let Some(candidate) = store.candidate(candidate_id)?
                    {
                        token_count += tokenizer.count(&candidate.content);
                    }
                }
            }
            let turn_count = seen_turns.len();
            let events = store.trace_events(Some(session.session_id))?;
            let modified_at_ms = events
                .last()
                .map(|event| event.event_id.into_ulid().timestamp_ms())
                .unwrap_or_else(|| session.session_id.into_ulid().timestamp_ms());
            let display_name = session
                .custom_name
                .clone()
                .unwrap_or_else(|| character_label.clone());
            Ok(SessionSummary {
                session_id: session.session_id,
                display_name,
                created_at_ms: session.session_id.into_ulid().timestamp_ms(),
                modified_at_ms,
                turn_count,
                character_label,
                persona_label: configuration.configuration.persona_name,
                token_count,
                last_message_preview,
            })
        })
        .collect()
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    if cleaned.len() <= max_chars {
        cleaned
    } else {
        let mut result: String = cleaned.chars().take(max_chars).collect();
        result.push('…');
        result
    }
}

fn branch_history(
    store: &Store,
    session_id: EntityId,
    branch_id: EntityId,
) -> Result<BranchHistory, EngineError> {
    let session = store
        .session(session_id)?
        .ok_or(SessionError::SessionNotFound(session_id))?;
    let branch = store
        .branch(branch_id)?
        .ok_or(SessionError::BranchNotFound(branch_id))?;
    if branch.session_id != session_id {
        return Err(EngineError::BranchSessionMismatch);
    }
    let configuration = store
        .configuration(&session.current_config_hash)?
        .ok_or_else(|| SessionError::ConfigurationNotFound(session.current_config_hash.clone()))?;
    let artifact = store.decoded_artifact(&branch.greeting_revision_hash)?;
    let greeting = artifact
        .greetings
        .get(branch.greeting_index)
        .map(|content| GreetingProjection {
            revision_hash: branch.greeting_revision_hash.clone(),
            index: branch.greeting_index,
            total: artifact.greetings.len(),
            content: content.clone(),
        });
    let display_scripts = store.granted_scripts_for_attempt(&configuration).ok();
    let worker = display_scripts
        .as_ref()
        .filter(|s| !s.is_empty())
        .and_then(|_| EcmaRegexWorker::current(std::time::Duration::from_millis(250)).ok());
    let turns = store
        .turns_for_branch(branch_id)?
        .into_iter()
        .map(|turn| {
            let mut candidates = store.candidates_for_turn(turn.turn_id)?;
            if let (Some(scripts), Some(worker)) = (&display_scripts, &worker) {
                for candidate in &mut candidates {
                    let mut finder = |p: &str, f: &str, t: &str| worker.find_matches(p, f, t);
                    if let Ok(rendered) =
                        apply_display_scripts(scripts, &candidate.content, &mut finder)
                        && rendered != candidate.content
                    {
                        candidate.rendered_content = Some(rendered);
                    }
                }
            }
            Ok(EngineTurn {
                candidates,
                attempts: store.attempts_for_turn(turn.turn_id)?,
                turn,
            })
        })
        .collect::<Result<Vec<_>, TurnError>>()?;
    Ok(BranchHistory {
        session,
        configuration,
        branch,
        greeting,
        turns,
    })
}

fn previous_selected_attempt(
    store: &Store,
    target: &AttemptProjection,
) -> Result<AttemptProjection, TurnError> {
    let turn = store
        .turn(target.turn_id)?
        .ok_or(TurnError::TurnNotFound(target.turn_id))?;
    let turns = store.turns_for_branch(turn.branch_id)?;
    let target_index = turns
        .iter()
        .position(|candidate| candidate.turn_id == turn.turn_id)
        .ok_or(TurnError::TurnNotFound(turn.turn_id))?;
    let previous_turn = target_index
        .checked_sub(1)
        .and_then(|index| turns.get(index))
        .ok_or(TurnError::NoPreviousTurnForAttempt(target.attempt_id))?;
    let candidate_id = previous_turn
        .selected_candidate_id
        .ok_or(TurnError::TurnHasNoSelection(previous_turn.turn_id))?;
    let candidate = store
        .candidate(candidate_id)?
        .ok_or(TurnError::CandidateNotFound(candidate_id))?;
    let attempt_id =
        candidate
            .attempt_id
            .ok_or(TurnError::PreviousTurnSelectionHasNoGenerationAttempt {
                attempt_id: target.attempt_id,
                previous_turn_id: previous_turn.turn_id,
            })?;
    store
        .attempt(attempt_id)?
        .ok_or(TurnError::AttemptNotFound(attempt_id))
}
#[derive(Debug, Error)]
pub enum EngineError {
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Session(#[from] SessionError),
    #[error(transparent)]
    Turn(#[from] TurnError),
    #[error(transparent)]
    Artifact(#[from] ArtifactError),
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    #[error(transparent)]
    Tokenizer(#[from] TokenizerError),
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error("Plugin '{0}' remains pinned by a Session Configuration Revision")]
    PluginInUse(String),
    #[error("Plugin '{id}' version {version} with digest {digest} was not found")]
    PluginNotFound {
        id: String,
        version: String,
        digest: ContentHash,
    },
    #[error("grants exceed the Plugin manifest request")]
    PluginGrantExceeded,
    #[error("existing grants exceed the upgraded Plugin manifest request")]
    PluginUpgradeGrantExceeded,
    #[error("Plugin '{0}' is not pinned by the Session")]
    PluginNotPinned(String),
    #[error("current Session Configuration Revision was not found")]
    SelectedSessionConfigurationMissing,
    #[error("Branch does not belong to the requested Session")]
    BranchSessionMismatch,
    #[error("attempt belongs to another session")]
    AttemptSessionMismatch,
    #[error("prompt segment selector '{selector}' did not match attempt {attempt_id}")]
    PromptSegmentNotFound {
        attempt_id: EntityId,
        selector: String,
    },
}
