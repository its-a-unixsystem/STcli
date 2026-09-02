use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ArtifactError, ArtifactInspectorRegistration, ArtifactKind, ArtifactRecord, AttemptProjection,
    BranchProjection, CandidateProjection, CapsuleError, CapsuleKind, CompactionReport,
    CompletedTurn, ContentHash, CreatedSession, DryRunResult, EcmaRegexWorker, EditedCandidate,
    EntityId, ImportedCapsule, InstalledPlugin, NativeExtensionImport, PluginCapability,
    PluginCommandResult, PluginEffect, PluginError, PluginEvent, PluginGrant, PluginHost,
    PluginInput, PluginPin, PluginRegistry, PromptDiff, PromptPlan, PromptSegmentInspection,
    ProviderEvent, RecoveryReport, ReplayReport, SessionConfiguration, SessionConfigurationRecord,
    SessionError, SessionProjection, StorageError, Store, StscriptError, StscriptLimits,
    StscriptResult, TokenizerError, TokenizerId, TurnCapsule, TurnError, TurnProjection,
    apply_display_scripts, diff_prompt_plans, extract_character_scripts, st_bridge_capability_tier,
    transform_preset_content,
};

pub const DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID: &str = "org.stcli.nemo-directives";
const NEMO_PLUGIN_MANIFEST: &str = include_str!("../../../plugins/nemo-directives/manifest.json");
const NEMO_PLUGIN_SCRIPT: &str = include_str!("../../../plugins/nemo-directives/script.js");

#[derive(Clone, Debug)]
pub struct StcliEngine {
    database: PathBuf,
    egress: Option<crate::EgressBroker>,
    inference: Option<crate::InferenceBroker>,
}

impl StcliEngine {
    pub fn new(database: impl AsRef<Path>) -> Self {
        Self {
            database: database.as_ref().to_owned(),
            egress: None,
            inference: None,
        }
    }

    pub fn with_egress_broker(database: impl AsRef<Path>, broker: crate::EgressBroker) -> Self {
        Self {
            database: database.as_ref().to_owned(),
            egress: Some(broker),
            inference: None,
        }
    }

    pub fn with_effect_brokers(
        database: impl AsRef<Path>,
        egress: crate::EgressBroker,
        inference: crate::InferenceBroker,
    ) -> Self {
        Self {
            database: database.as_ref().to_owned(),
            egress: Some(egress),
            inference: Some(inference),
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

    fn default_plugin_state(&self) -> PathBuf {
        self.database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".default-plugins")
    }

    fn default_opt_out(&self, id: &str) -> PathBuf {
        self.default_plugin_state().join("opt-outs").join(id)
    }

    fn clear_default_opt_out(&self, id: &str) -> Result<(), PluginError> {
        let path = self.default_opt_out(id);
        if path.exists() {
            fs::remove_file(&path).map_err(|source| PluginError::Remove { path, source })?;
        }
        Ok(())
    }

    fn ensure_default_plugins(&self) -> Result<(), EngineError> {
        if self
            .default_opt_out(DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID)
            .exists()
        {
            return Ok(());
        }
        let manifest: crate::PluginManifest =
            serde_json::from_str(NEMO_PLUGIN_MANIFEST).map_err(PluginError::Json)?;
        let registration = ArtifactInspectorRegistration {
            id: manifest.id.clone(),
            version: manifest.version.clone(),
            component_sha256: manifest.component_sha256.clone(),
            capabilities: [PluginCapability::InspectArtifact].into_iter().collect(),
        };
        let store = Store::open(&self.database)?;
        if store
            .artifact_inspector(DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID)?
            .as_ref()
            == Some(&registration)
            && self
                .plugin_registry()
                .find(
                    DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID,
                    &manifest.component_sha256,
                )?
                .is_some()
        {
            return Ok(());
        }
        let root = self
            .default_plugin_state()
            .join("packages")
            .join(DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID);
        fs::create_dir_all(&root).map_err(|source| PluginError::Create {
            path: root.clone(),
            source,
        })?;
        for (path, content) in [
            (root.join("manifest.json"), NEMO_PLUGIN_MANIFEST),
            (root.join("script.js"), NEMO_PLUGIN_SCRIPT),
        ] {
            fs::write(&path, content).map_err(|source| PluginError::Write { path, source })?;
        }
        self.plugin_registry().install(&root)?;
        store.register_artifact_inspector(&registration)?;
        Ok(())
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
        self.ensure_default_plugins()?;
        if let EngineQuery::DoctorPlugin { directory } = &query {
            return Ok(EngineInspection::InstalledPlugin(
                self.plugin_registry().doctor(directory)?,
            ));
        }
        if let EngineQuery::Plugins { plugin_id } = &query {
            let store = Store::open(&self.database)?;
            let registered = store.artifact_inspectors()?;
            let plugins = self
                .plugin_registry()
                .list()?
                .into_iter()
                .filter(|plugin| {
                    plugin_id
                        .as_ref()
                        .is_none_or(|expected| plugin.manifest.id == *expected)
                })
                .map(|mut plugin| {
                    plugin.inspection_enabled = registered.iter().any(|registration| {
                        registration.id == plugin.manifest.id
                            && registration.version == plugin.manifest.version
                            && registration.component_sha256 == plugin.manifest.component_sha256
                    });
                    plugin
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
            EngineQuery::ArtifactInspectors => Ok(EngineInspection::ArtifactInspectors(
                store.artifact_inspectors()?,
            )),
            EngineQuery::InspectArtifactWithPlugin {
                plugin_id,
                revision_hash,
            } => {
                let registration = store.artifact_inspector(&plugin_id)?.ok_or_else(|| {
                    EngineError::ArtifactInspectorNotRegistered(plugin_id.clone())
                })?;
                if !registration
                    .capabilities
                    .contains(&PluginCapability::InspectArtifact)
                {
                    return Err(
                        PluginError::CapabilityDenied(PluginCapability::InspectArtifact).into(),
                    );
                }
                let installed = self.installed_plugin(
                    &registration.id,
                    &registration.version.to_string(),
                    &registration.component_sha256,
                )?;
                let artifact = store.decoded_artifact(&revision_hash)?;
                let grant = PluginGrant {
                    id: registration.id.clone(),
                    version: registration.version,
                    component_sha256: registration.component_sha256,
                    capabilities: registration.capabilities,
                    settings: serde_json::Value::Null,
                    egress_allow_list: Vec::new(),
                    enabled: true,
                };
                let receipt = PluginHost::new(Default::default()).execute(
                    &installed,
                    &grant,
                    PluginInput {
                        event: PluginEvent::InspectArtifact,
                        plugin_id: plugin_id.clone(),
                        settings: serde_json::Value::Null,
                        context: serde_json::Value::Null,
                        payload: serde_json::Value::Null,
                        state: serde_json::json!({}),
                        artifact: artifact.semantic,
                        session: serde_json::Value::Null,
                    },
                )?;
                let mut outputs = receipt
                    .effects
                    .into_iter()
                    .filter_map(|effect| match effect {
                        PluginEffect::Output { value } => Some(value),
                        _ => None,
                    });
                let value = outputs
                    .next()
                    .ok_or(PluginError::ArtifactInspectionOutputCount(0))?;
                if outputs.next().is_some() {
                    return Err(PluginError::ArtifactInspectionOutputCount(2).into());
                }
                Ok(EngineInspection::PluginArtifactOutput(
                    PluginArtifactOutput {
                        plugin_id,
                        revision_hash,
                        value,
                    },
                ))
            }
            EngineQuery::DoctorPlugin { .. } | EngineQuery::Plugins { .. } => unreachable!(),
        }
    }

    pub async fn execute(
        &self,
        command: EngineCommand,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<EngineResult, EngineError> {
        self.ensure_default_plugins()?;
        let mut store = Store::open(&self.database)?;
        if let Some(broker) = &self.egress {
            store.set_egress_broker(broker.clone());
        }
        if let Some(broker) = &self.inference {
            store.set_inference_broker(broker.clone());
        }
        match command {
            EngineCommand::InstallPlugin { directory } => {
                let installed = self.plugin_registry().install(&directory)?;
                if installed.manifest.id == DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID {
                    self.clear_default_opt_out(DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID)?;
                    store.register_artifact_inspector(&ArtifactInspectorRegistration {
                        id: installed.manifest.id.clone(),
                        version: installed.manifest.version.clone(),
                        component_sha256: installed.manifest.component_sha256.clone(),
                        capabilities: [PluginCapability::InspectArtifact].into_iter().collect(),
                    })?;
                }
                Ok(EngineResult::InstalledPlugin(installed))
            }
            EngineCommand::ImportExtension { directory } => Ok(EngineResult::ImportedExtension(
                self.plugin_registry().import_native_extension(&directory)?,
            )),
            EngineCommand::RestoreDefaultPlugins => {
                self.clear_default_opt_out(DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID)?;
                self.ensure_default_plugins()?;
                let mut installed = self
                    .plugin_registry()
                    .list()?
                    .into_iter()
                    .find(|plugin| plugin.manifest.id == DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID)
                    .ok_or_else(|| {
                        EngineError::ArtifactInspectorNotRegistered(
                            DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID.to_owned(),
                        )
                    })?;
                installed.inspection_enabled = true;
                Ok(EngineResult::InstalledPlugin(installed))
            }
            EngineCommand::RemovePlugin { plugin_id } => {
                if store.plugin_in_use(&plugin_id)? {
                    return Err(EngineError::PluginInUse(plugin_id));
                }
                store.unregister_artifact_inspector(&plugin_id)?;
                if plugin_id == DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID {
                    let opt_out = self.default_opt_out(&plugin_id);
                    let parent = opt_out.parent().expect("opt-out marker has a parent");
                    fs::create_dir_all(parent).map_err(|source| PluginError::Create {
                        path: parent.to_owned(),
                        source,
                    })?;
                    fs::write(&opt_out, []).map_err(|source| PluginError::Write {
                        path: opt_out,
                        source,
                    })?;
                }
                Ok(EngineResult::PluginRemoval(PluginRemovalReceipt {
                    removed: self.plugin_registry().remove(&plugin_id)?,
                    id: plugin_id,
                }))
            }
            EngineCommand::RegisterArtifactInspector {
                id,
                version,
                digest,
                capabilities,
            } => {
                let installed = self.installed_plugin(&id, &version, &digest)?;
                if !capabilities.is_subset(&installed.manifest.requested_capabilities) {
                    return Err(EngineError::PluginGrantExceeded);
                }
                let registration = ArtifactInspectorRegistration {
                    id,
                    version: installed.manifest.version,
                    component_sha256: installed.manifest.component_sha256,
                    capabilities,
                };
                store.register_artifact_inspector(&registration)?;
                Ok(EngineResult::ArtifactInspectorRegistration(registration))
            }
            EngineCommand::AdoptPlugin {
                session_id,
                id,
                version,
                digest,
                capabilities,
                settings,
                egress,
            } => {
                let installed = self.installed_plugin(&id, &version, &digest)?;
                if !capabilities.is_subset(&installed.manifest.requested_capabilities) {
                    return Err(EngineError::PluginGrantExceeded);
                }
                Ok(EngineResult::Configuration(Box::new(
                    adopt_plugin_configuration(
                        &mut store,
                        session_id,
                        PluginPin {
                            id,
                            version: installed.manifest.version.to_string(),
                            component_hash: installed.manifest.component_sha256,
                            capabilities,
                            settings,
                            egress_allow_list: egress,
                            enabled: true,
                        },
                    )?,
                )))
            }
            EngineCommand::AdoptExtension {
                session_id,
                id,
                version,
                digest,
                settings,
                egress,
            } => {
                let installed = self.installed_plugin(&id, &version, &digest)?;
                if installed.manifest.runtime != crate::PluginRuntime::StBridge {
                    return Err(EngineError::ExtensionRuntimeRequired(id));
                }
                let capabilities = st_bridge_capability_tier();
                if !capabilities.is_subset(&installed.manifest.requested_capabilities) {
                    return Err(EngineError::PluginGrantExceeded);
                }
                Ok(EngineResult::Configuration(Box::new(
                    adopt_plugin_configuration(
                        &mut store,
                        session_id,
                        PluginPin {
                            id,
                            version: installed.manifest.version.to_string(),
                            component_hash: installed.manifest.component_sha256,
                            capabilities,
                            settings,
                            egress_allow_list: egress,
                            enabled: true,
                        },
                    )?,
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
            EngineCommand::ExecuteStscript {
                session_id,
                execution_id,
                source,
                limits,
            } => Ok(EngineResult::Stscript(store.execute_stscript(
                session_id,
                execution_id,
                &source,
                limits,
            )?)),
            EngineCommand::CreateSession {
                configuration,
                greeting_index,
            } => Ok(EngineResult::CreatedSession(Box::new(
                store.create_session(*configuration, greeting_index)?,
            ))),
            EngineCommand::CreateBranch {
                session_id,
                source_branch_id,
                at_turn_id,
            } => {
                let session = store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?;
                let source_branch_id = source_branch_id.unwrap_or(session.root_branch_id);
                let source_branch = store
                    .branch(source_branch_id)?
                    .ok_or(SessionError::BranchNotFound(source_branch_id))?;
                if source_branch.session_id != session_id {
                    return Err(SessionError::BranchSessionMismatch.into());
                }
                Ok(EngineResult::Branch(store.create_branch_at(
                    session_id,
                    source_branch_id,
                    at_turn_id,
                    source_branch.greeting_index,
                )?))
            }
            EngineCommand::DuplicateSession {
                session_id,
                branch_id,
                up_to_turn_id,
                new_name,
            } => Ok(EngineResult::DuplicatedSession(Box::new(
                store.duplicate_session(session_id, branch_id, up_to_turn_id, new_name)?,
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
            EngineCommand::UpdatePromptOrder {
                session_id,
                revision_hash,
                character_id,
                changes,
            } => {
                let mut current = if let Some(session_id) = session_id {
                    let session = store
                        .session(session_id)?
                        .ok_or(SessionError::SessionNotFound(session_id))?;
                    let configuration = store.configuration(&session.current_config_hash)?.ok_or(
                        SessionError::ConfigurationNotFound(session.current_config_hash.clone()),
                    )?;
                    if configuration.configuration.prompt_preset_revision.as_ref()
                        != Some(&revision_hash)
                    {
                        return Err(EngineError::PromptPresetNotPinned(session_id));
                    }
                    Some((session_id, configuration.configuration))
                } else {
                    None
                };
                let artifact = store.patch_prompt_order(&revision_hash, character_id, &changes)?;
                let configuration = if let Some((session_id, mut configuration)) = current.take() {
                    if artifact.revision_hash == revision_hash {
                        None
                    } else {
                        configuration.prompt_preset_revision = Some(artifact.revision_hash.clone());
                        Some(Box::new(
                            store.update_session_configuration(session_id, configuration)?,
                        ))
                    }
                } else {
                    None
                };
                Ok(EngineResult::PromptOrderUpdated {
                    artifact,
                    configuration,
                })
            }
            EngineCommand::UpdatePromptOrderOverride {
                session_id,
                identifier,
                enabled,
            } => {
                let session = store
                    .session(session_id)?
                    .ok_or(SessionError::SessionNotFound(session_id))?;
                let mut configuration = store
                    .configuration(&session.current_config_hash)?
                    .ok_or(SessionError::ConfigurationNotFound(
                        session.current_config_hash,
                    ))?
                    .configuration;
                match enabled {
                    Some(value) => {
                        configuration
                            .prompt_order_overrides
                            .insert(identifier, value);
                    }
                    None => {
                        configuration.prompt_order_overrides.remove(&identifier);
                    }
                }
                Ok(EngineResult::Configuration(Box::new(
                    store.update_session_configuration(session_id, configuration)?,
                )))
            }
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
    ArtifactInspectors,
    InspectArtifactWithPlugin {
        plugin_id: String,
        revision_hash: ContentHash,
    },
}

#[derive(Clone, Debug)]
pub enum EngineCommand {
    InstallPlugin {
        directory: PathBuf,
    },
    ImportExtension {
        directory: PathBuf,
    },
    RestoreDefaultPlugins,
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
        egress: Vec<crate::EgressAllowance>,
    },
    AdoptExtension {
        session_id: EntityId,
        id: String,
        version: String,
        digest: ContentHash,
        settings: serde_json::Value,
        egress: Vec<crate::EgressAllowance>,
    },
    RegisterArtifactInspector {
        id: String,
        version: String,
        digest: ContentHash,
        capabilities: BTreeSet<PluginCapability>,
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
    ExecuteStscript {
        session_id: EntityId,
        execution_id: EntityId,
        source: String,
        limits: StscriptLimits,
    },
    CreateSession {
        configuration: Box<SessionConfiguration>,
        greeting_index: usize,
    },
    CreateBranch {
        session_id: EntityId,
        source_branch_id: Option<EntityId>,
        at_turn_id: Option<EntityId>,
    },
    DuplicateSession {
        session_id: EntityId,
        branch_id: Option<EntityId>,
        up_to_turn_id: Option<EntityId>,
        new_name: Option<String>,
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
    UpdatePromptOrderOverride {
        session_id: EntityId,
        identifier: String,
        enabled: Option<bool>,
    },
    UpdatePromptOrder {
        session_id: Option<EntityId>,
        revision_hash: ContentHash,
        character_id: Option<u64>,
        changes: BTreeMap<String, bool>,
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
    ImportedExtension(NativeExtensionImport),
    ArtifactInspectorRegistration(ArtifactInspectorRegistration),
    PluginRemoval(PluginRemovalReceipt),
    ArtifactBundle {
        primary: ArtifactRecord,
        supplementary_artifacts: Vec<ArtifactRecord>,
        asset_count: usize,
    },
    Stscript(StscriptResult),
    CreatedSession(Box<CreatedSession>),
    DuplicatedSession(Box<CreatedSession>),
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
    PromptOrderUpdated {
        artifact: ArtifactRecord,
        configuration: Option<Box<SessionConfigurationRecord>>,
    },
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
    ArtifactInspectors(Vec<ArtifactInspectorRegistration>),
    PluginArtifactOutput(PluginArtifactOutput),
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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PluginArtifactOutput {
    pub plugin_id: String,
    pub revision_hash: ContentHash,
    pub value: serde_json::Value,
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
    pub archived: bool,
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

fn adopt_plugin_configuration(
    store: &mut Store,
    session_id: EntityId,
    pin: PluginPin,
) -> Result<SessionConfigurationRecord, EngineError> {
    let mut configuration = selected_session_configuration(store, session_id)?;
    configuration
        .plugins
        .retain(|existing| existing.id != pin.id);
    configuration.plugins.push(pin);
    Ok(store.update_session_configuration(session_id, configuration)?)
}

fn session_summaries(store: &Store) -> Result<Vec<SessionSummary>, EngineError> {
    store
        .sessions()?
        .into_iter()
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
                archived: session.archived,
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
    #[error(transparent)]
    Stscript(#[from] StscriptError),
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
    #[error("Plugin '{0}' is not pinned by the Session")]
    PluginNotPinned(String),
    #[error("Plugin '{0}' is not registered for Artifact inspection")]
    ArtifactInspectorNotRegistered(String),
    #[error("Plugin '{0}' is not an st-bridge Extension")]
    ExtensionRuntimeRequired(String),
    #[error("prompt preset revision is not pinned by Session {0}")]
    PromptPresetNotPinned(EntityId),
    #[error("existing grants exceed the upgraded Plugin manifest request")]
    PluginUpgradeGrantExceeded,
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
