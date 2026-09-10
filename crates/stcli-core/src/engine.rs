use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    ARTIFACT_CODEC_INTERFACE_VERSION, ArtifactCodecBundle, ArtifactCodecCompatibility,
    ArtifactCodecError, ArtifactCodecInput, ArtifactCodecOutput, ArtifactCodecProvenance,
    ArtifactError, ArtifactInspectorRegistration, ArtifactKind, ArtifactRecord, AttemptProjection,
    BranchProjection, CandidateProjection, CapsuleError, CapsuleKind, CompactionReport,
    CompatibilityWarning, CompletedTurn, Config, ConfigError, ContentHash, CreatedSession,
    DryRunResult, EcmaRegexWorker, EditedCandidate, EntityId, GlobalExtensionPin, ImportedCapsule,
    InstalledPlugin, InteractionResult, InteractionSubmission, InteractionSurface,
    NativeExtensionImport, PluginCapability, PluginCommandResult, PluginEffect, PluginError,
    PluginEvent, PluginGrant, PluginHost, PluginInput, PluginLimits, PluginPin, PluginRegistry,
    PluginRuntime, PromptDiff, PromptPlan, PromptSegmentInspection, ProviderEvent, RecoveryReport,
    ReplayReport, SessionConfiguration, SessionConfigurationRecord, SessionError,
    SessionProjection, StateError, StorageError, Store, StscriptError, StscriptLimits,
    StscriptResult, TokenizerError, TokenizerId, TurnCapsule, TurnError, TurnProjection,
    apply_display_scripts,
    artifact_codec::{
        MAX_CODEC_COMPATIBILITY_ITEMS, MAX_CODEC_COMPATIBILITY_MESSAGE_BYTES,
        MAX_CODEC_SOURCE_BYTES,
    },
    diff_prompt_plans, extract_character_scripts, st_bridge_capability_tier,
    transform_preset_content,
};

pub const DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID: &str = "org.stcli.nemo-directives";
pub const DEFAULT_MEMORY_EXTENSION_ID: &str = "memory";
pub const DEFAULT_SILLYTAVERN_CODEC_PLUGIN_ID: &str = "org.stcli.sillytavern-codec";
const NEMO_PLUGIN_MANIFEST: &str = include_str!("../../../plugins/nemo-directives/manifest.json");
const NEMO_PLUGIN_SCRIPT: &str = include_str!("../../../plugins/nemo-directives/script.js");
const MEMORY_EXTENSION_MANIFEST: &str = include_str!("../../../extensions/memory/manifest.json");
const MEMORY_EXTENSION_SCRIPT: &str = include_str!("../../../extensions/memory/index.js");
const MEMORY_EXTENSION_SETTINGS_SCHEMA: &str =
    include_str!("../../../extensions/memory/settings.schema.json");
const SILLYTAVERN_CODEC_MANIFEST: &str = include_str!("../../../plugins/ccv3-codec/manifest.json");
const SILLYTAVERN_CODEC_COMPONENT: &[u8] =
    include_bytes!("../../../plugins/ccv3-codec/component.wasm");

fn validate_codec_interface(actual: &str) -> Result<(), ArtifactCodecError> {
    if actual != ARTIFACT_CODEC_INTERFACE_VERSION {
        return Err(ArtifactCodecError::InterfaceVersion {
            expected: ARTIFACT_CODEC_INTERFACE_VERSION.to_owned(),
            actual: actual.to_owned(),
        });
    }
    Ok(())
}

fn validate_codec_compatibility(
    compatibility: &[ArtifactCodecCompatibility],
) -> Result<(), ArtifactCodecError> {
    if compatibility.len() > MAX_CODEC_COMPATIBILITY_ITEMS {
        return Err(ArtifactCodecError::CompatibilityCount {
            actual: compatibility.len(),
            limit: MAX_CODEC_COMPATIBILITY_ITEMS,
        });
    }
    for item in compatibility {
        if item.code.is_empty()
            || item.code.len() > 64
            || !item.code.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-' || byte == b'.'
            })
            || item.message.is_empty()
            || item.message.len() > MAX_CODEC_COMPATIBILITY_MESSAGE_BYTES
        {
            return Err(ArtifactCodecError::InvalidCompatibility {
                code: item.code.clone(),
            });
        }
    }
    Ok(())
}

fn codec_operation_error(
    expected: &'static str,
    output: &ArtifactCodecOutput,
) -> ArtifactCodecError {
    let actual = match output {
        ArtifactCodecOutput::Detect { .. } => "detect",
        ArtifactCodecOutput::Decode { .. } => "decode",
        ArtifactCodecOutput::Encode { .. } => "encode",
    };
    ArtifactCodecError::Operation { expected, actual }
}

#[derive(Clone, Copy)]
struct DefaultPackage {
    id: &'static str,
    manifest: &'static str,
    component_name: &'static str,
    component: &'static [u8],
    settings_schema: Option<&'static str>,
    artifact_inspector: bool,
}

const DEFAULT_PACKAGES: [DefaultPackage; 3] = [
    DefaultPackage {
        id: DEFAULT_NEMO_DIRECTIVES_PLUGIN_ID,
        manifest: NEMO_PLUGIN_MANIFEST,
        component_name: "script.js",
        component: NEMO_PLUGIN_SCRIPT.as_bytes(),
        settings_schema: None,
        artifact_inspector: true,
    },
    DefaultPackage {
        id: DEFAULT_MEMORY_EXTENSION_ID,
        manifest: MEMORY_EXTENSION_MANIFEST,
        component_name: "index.js",
        component: MEMORY_EXTENSION_SCRIPT.as_bytes(),
        settings_schema: Some(MEMORY_EXTENSION_SETTINGS_SCHEMA),
        artifact_inspector: false,
    },
    DefaultPackage {
        id: DEFAULT_SILLYTAVERN_CODEC_PLUGIN_ID,
        manifest: SILLYTAVERN_CODEC_MANIFEST,
        component_name: "component.wasm",
        component: SILLYTAVERN_CODEC_COMPONENT,
        settings_schema: None,
        artifact_inspector: true,
    },
];

#[derive(Clone, Debug)]
pub struct StcliEngine {
    database: PathBuf,
    config_directory: PathBuf,
    egress: Option<crate::EgressBroker>,
    inference: Option<crate::InferenceBroker>,
}

impl StcliEngine {
    pub fn new(database: impl AsRef<Path>) -> Self {
        let database = database.as_ref().to_owned();
        let config_directory = database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_owned();
        Self {
            database,
            config_directory,
            egress: None,
            inference: None,
        }
    }

    pub fn with_egress_broker(database: impl AsRef<Path>, broker: crate::EgressBroker) -> Self {
        let mut engine = Self::new(database);
        engine.egress = Some(broker);
        engine
    }

    pub fn with_effect_brokers(
        database: impl AsRef<Path>,
        egress: crate::EgressBroker,
        inference: crate::InferenceBroker,
    ) -> Self {
        let mut engine = Self::new(database);
        engine.egress = Some(egress);
        engine.inference = Some(inference);
        engine
    }

    pub fn with_config_directory(mut self, directory: impl AsRef<Path>) -> Self {
        self.config_directory = directory.as_ref().to_owned();
        self
    }

    pub fn database(&self) -> &Path {
        &self.database
    }

    fn plugin_registry(&self) -> PluginRegistry {
        PluginRegistry::new(self.data_directory().join("plugins"))
    }

    fn data_directory(&self) -> &Path {
        self.database.parent().unwrap_or_else(|| Path::new("."))
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
        let store = Store::open(&self.database)?;
        for package in DEFAULT_PACKAGES {
            if self.default_opt_out(package.id).exists() {
                continue;
            }
            let manifest: crate::PluginManifest =
                serde_json::from_str(package.manifest).map_err(PluginError::Json)?;
            let registration = package
                .artifact_inspector
                .then(|| ArtifactInspectorRegistration {
                    id: manifest.id.clone(),
                    version: manifest.version.clone(),
                    component_sha256: manifest.component_sha256.clone(),
                    capabilities: manifest.requested_capabilities.clone(),
                });
            let registered = match &registration {
                Some(registration) => {
                    store.artifact_inspector(package.id)?.as_ref() == Some(registration)
                }
                None => true,
            };
            let installed = self.plugin_registry().find_pinned(
                &manifest.id,
                &manifest.version,
                &manifest.component_sha256,
            )?;
            if registered && installed.is_some() {
                continue;
            }
            let root = self
                .default_plugin_state()
                .join("packages")
                .join(package.id);
            fs::create_dir_all(&root).map_err(|source| PluginError::Create {
                path: root.clone(),
                source,
            })?;
            for (path, content) in [
                (root.join("manifest.json"), package.manifest.as_bytes()),
                (root.join(package.component_name), package.component),
            ] {
                fs::write(&path, content).map_err(|source| PluginError::Write { path, source })?;
            }
            if let (Some(name), Some(content)) =
                (manifest.settings_schema.as_deref(), package.settings_schema)
            {
                let path = root.join(name);
                fs::write(&path, content).map_err(|source| PluginError::Write { path, source })?;
            }
            self.plugin_registry().install(&root)?;
            if let Some(registration) = &registration {
                store.register_artifact_inspector(registration)?;
            }
        }
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

    fn codec_plugin(
        &self,
        id: &str,
        version: &str,
        digest: &ContentHash,
        capabilities: &BTreeSet<PluginCapability>,
    ) -> Result<(InstalledPlugin, PluginGrant), EngineError> {
        let installed = self.installed_plugin(id, version, digest)?;
        let required = [
            PluginCapability::ArtifactCodec,
            PluginCapability::InspectArtifact,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        if installed.manifest.runtime != PluginRuntime::Wasm
            || installed.manifest.requested_capabilities != required
            || *capabilities != required
            || installed.manifest.subscriptions
                != [PluginEvent::InspectArtifact]
                    .into_iter()
                    .collect::<BTreeSet<_>>()
            || !installed.manifest.prompt_slots.is_empty()
            || !installed.manifest.commands.is_empty()
            || !installed.manifest.macros.is_empty()
            || installed.manifest.settings_schema.is_some()
            || installed
                .manifest
                .artifact_codec
                .as_ref()
                .is_none_or(|codec| {
                    codec.formats.is_empty()
                        || !codec
                            .interface_versions
                            .contains(ARTIFACT_CODEC_INTERFACE_VERSION)
                })
        {
            return Err(ArtifactCodecError::InvalidPluginContract(id.to_owned()).into());
        }
        let grant = PluginGrant {
            id: id.to_owned(),
            version: installed.manifest.version.clone(),
            component_sha256: digest.clone(),
            capabilities: capabilities.clone(),
            settings: serde_json::Value::Null,
            egress_allow_list: Vec::new(),
            enabled: true,
        };
        Ok((installed, grant))
    }

    fn execute_artifact_codec(
        &self,
        installed: &InstalledPlugin,
        grant: &PluginGrant,
        input: ArtifactCodecInput,
    ) -> Result<ArtifactCodecOutput, EngineError> {
        let input = PluginInput {
            event: PluginEvent::InspectArtifact,
            plugin_id: installed.manifest.id.clone(),
            settings: serde_json::Value::Null,
            context: serde_json::Value::Null,
            payload: serde_json::to_value(input).map_err(PluginError::Json)?,
            state: serde_json::Value::Null,
            artifact: serde_json::Value::Null,
            session: serde_json::Value::Null,
        };
        let limits = PluginLimits {
            input_bytes: 16 * 1024 * 1024,
            output_bytes: 16 * 1024 * 1024,
            memory_bytes: 64 * 1024 * 1024,
            ..PluginLimits::default()
        };
        let receipt = PluginHost::new(limits).execute(installed, grant, input)?;
        let outputs = receipt
            .effects
            .into_iter()
            .filter_map(|effect| match effect {
                PluginEffect::Output { value } => Some(value),
                _ => None,
            })
            .collect::<Vec<_>>();
        if outputs.len() != 1 {
            return Err(PluginError::ArtifactInspectionOutputCount(outputs.len()).into());
        }
        serde_json::from_value(outputs.into_iter().next().expect("one output"))
            .map_err(PluginError::Json)
            .map_err(EngineError::Plugin)
    }

    fn decode_artifact_with_codec(
        &self,
        store: &Store,
        source: &[u8],
    ) -> Result<Option<(ArtifactCodecBundle, ArtifactCodecProvenance)>, EngineError> {
        if source.len() > MAX_CODEC_SOURCE_BYTES {
            return Ok(None);
        }
        let encoded = BASE64.encode(source);
        let mut claims = Vec::new();
        for registration in store
            .artifact_inspectors()?
            .into_iter()
            .filter(|registration| {
                registration
                    .capabilities
                    .contains(&PluginCapability::ArtifactCodec)
            })
        {
            let (installed, grant) = self.codec_plugin(
                &registration.id,
                &registration.version.to_string(),
                &registration.component_sha256,
                &registration.capabilities,
            )?;
            let detection = self.execute_artifact_codec(
                &installed,
                &grant,
                ArtifactCodecInput::Detect {
                    interface_version: ARTIFACT_CODEC_INTERFACE_VERSION.to_owned(),
                    source: encoded.clone(),
                },
            )?;
            let ArtifactCodecOutput::Detect {
                interface_version,
                compatible,
                compatibility,
            } = detection
            else {
                return Err(codec_operation_error("detect", &detection).into());
            };
            validate_codec_interface(&interface_version)?;
            validate_codec_compatibility(&compatibility)?;
            if compatible {
                claims.push((installed, grant, compatibility));
            }
        }
        if claims.len() > 1 {
            let mut ids = claims
                .iter()
                .map(|(installed, _, _)| installed.manifest.id.clone())
                .collect::<Vec<_>>();
            ids.sort();
            return Err(ArtifactCodecError::AmbiguousFormatClaims(ids).into());
        }
        let Some((installed, grant, mut compatibility)) = claims.pop() else {
            return Ok(None);
        };
        let decoded = self.execute_artifact_codec(
            &installed,
            &grant,
            ArtifactCodecInput::Decode {
                interface_version: ARTIFACT_CODEC_INTERFACE_VERSION.to_owned(),
                source: encoded,
            },
        )?;
        let ArtifactCodecOutput::Decode {
            interface_version,
            format,
            bundle,
            compatibility: decoded_compatibility,
        } = decoded
        else {
            return Err(codec_operation_error("decode", &decoded).into());
        };
        validate_codec_interface(&interface_version)?;
        validate_codec_compatibility(&decoded_compatibility)?;
        if !installed
            .manifest
            .artifact_codec
            .as_ref()
            .is_some_and(|codec| codec.formats.contains(&format))
        {
            return Err(ArtifactCodecError::InvalidFormat(format).into());
        }
        if compatibility.len() + decoded_compatibility.len() > MAX_CODEC_COMPATIBILITY_ITEMS {
            return Err(ArtifactCodecError::CompatibilityCount {
                actual: compatibility.len() + decoded_compatibility.len(),
                limit: MAX_CODEC_COMPATIBILITY_ITEMS,
            }
            .into());
        }
        compatibility.extend(decoded_compatibility);
        let provenance = ArtifactCodecProvenance {
            plugin_id: installed.manifest.id,
            version: installed.manifest.version,
            component_sha256: installed.manifest.component_sha256,
            interface_version,
            format,
            compatibility,
            supplementary_artifacts: Vec::new(),
        };
        Ok(Some((bundle, provenance)))
    }

    fn export_artifact_with_codec(
        &self,
        store: &Store,
        revision_hash: &ContentHash,
    ) -> Result<Vec<u8>, EngineError> {
        let Some(provenance) = store.artifact_codec_provenance(revision_hash)? else {
            return Ok(store.export_artifact(revision_hash)?);
        };
        let capabilities = [
            PluginCapability::ArtifactCodec,
            PluginCapability::InspectArtifact,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let (installed, grant) = self.codec_plugin(
            &provenance.plugin_id,
            &provenance.version.to_string(),
            &provenance.component_sha256,
            &capabilities,
        )?;
        let bundle = store.artifact_codec_bundle(revision_hash, &provenance)?;
        let output = self.execute_artifact_codec(
            &installed,
            &grant,
            ArtifactCodecInput::Encode {
                interface_version: ARTIFACT_CODEC_INTERFACE_VERSION.to_owned(),
                format: provenance.format,
                bundle,
            },
        )?;
        let ArtifactCodecOutput::Encode {
            interface_version,
            source,
            compatibility,
        } = output
        else {
            return Err(codec_operation_error("encode", &output).into());
        };
        validate_codec_interface(&interface_version)?;
        validate_codec_compatibility(&compatibility)?;
        let source = BASE64
            .decode(source)
            .map_err(|source| ArtifactCodecError::InvalidBase64 {
                field: "encoded source",
                source,
            })?;
        if source.len() > MAX_CODEC_SOURCE_BYTES {
            return Err(ArtifactCodecError::EncodedSourceSize {
                actual: source.len(),
                limit: MAX_CODEC_SOURCE_BYTES,
            }
            .into());
        }
        Ok(source)
    }
    fn extension_interactions(
        &self,
        store: &Store,
        session_id: EntityId,
        branch_id: Option<EntityId>,
    ) -> Result<Vec<InteractionSurface>, EngineError> {
        if let Some(branch_id) = branch_id {
            let branch = store
                .branch(branch_id)?
                .ok_or(SessionError::BranchNotFound(branch_id))?;
            if branch.session_id != session_id {
                return Err(EngineError::BranchSessionMismatch);
            }
        }
        let session = store
            .session(session_id)?
            .ok_or(SessionError::SessionNotFound(session_id))?;
        let configuration = store
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| {
                SessionError::ConfigurationNotFound(session.current_config_hash.clone())
            })?;
        let providers = Config::load(&self.config_directory)?
            .providers
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        let characters = store
            .artifacts()?
            .into_iter()
            .filter(|record| {
                matches!(
                    record.kind,
                    crate::ArtifactKind::CharacterCardV1
                        | crate::ArtifactKind::CharacterCardV2
                        | crate::ArtifactKind::CharacterCardV3
                )
            })
            .map(|record| {
                let decoded = store.decoded_artifact(&record.revision_hash)?;
                let label = decoded
                    .semantic
                    .get("data")
                    .and_then(|data| data.get("name"))
                    .or_else(|| decoded.semantic.get("name"))
                    .and_then(serde_json::Value::as_str)
                    .filter(|name| !name.trim().is_empty())
                    .map(str::to_owned)
                    .unwrap_or_else(|| record.revision_hash.to_string());
                Ok((record.revision_hash, label))
            })
            .collect::<Result<Vec<_>, crate::ArtifactError>>()?;
        let state = store.state_transaction(session_id)?;
        let content_candidate = branch_id
            .map(|branch_id| selected_content_candidate(store, branch_id))
            .transpose()?
            .flatten();
        let completed_attempt = content_candidate.is_some();
        let mut surfaces = Vec::new();
        for pin in &configuration.configuration.plugins {
            let installed = self.installed_plugin(&pin.id, &pin.version, &pin.component_hash)?;
            if installed.manifest.runtime != crate::PluginRuntime::StBridge {
                continue;
            }
            let Some(declaration) = crate::interaction::load_interaction_declaration(&installed)?
            else {
                continue;
            };
            let persisted = state
                .get(
                    crate::VariableScope::Local,
                    &format!("extension.{}.settings", pin.id),
                )
                .map(|cell| &cell.value)
                .unwrap_or(&serde_json::Value::Null);
            surfaces.push(crate::interaction::build_surface(
                crate::interaction::SurfaceBuild {
                    session_id,
                    branch_id,
                    configuration_revision: &configuration.revision_hash,
                    installed: &installed,
                    declaration: &declaration,
                    pinned_settings: &pin.settings,
                    persisted_settings: persisted,
                    enabled: pin.enabled,
                    capabilities: &pin.capabilities,
                    provider_profiles: &providers,
                    characters: &characters,
                    completed_attempt,
                    content_candidate,
                    content_choices: &[],
                },
            )?);
        }
        Ok(surfaces)
    }
    fn submit_extension_interaction(
        &self,
        store: &mut Store,
        identity: &crate::InteractionIdentity,
        expected_revision: &ContentHash,
        submission: InteractionSubmission,
    ) -> Result<InteractionResult, EngineError> {
        if serde_json::to_vec(&submission)
            .map_err(PluginError::Json)?
            .len()
            > crate::PluginLimits::default().input_bytes
        {
            return Err(PluginError::InputLimit.into());
        }
        let current = self
            .extension_interactions(store, identity.session_id, identity.branch_id)?
            .into_iter()
            .find(|surface| surface.identity.extension_id == identity.extension_id)
            .ok_or_else(|| {
                EngineError::InteractionUnavailable(crate::interaction::bounded(
                    "Extension interaction is no longer available",
                ))
            })?;
        let reject = |reason: &str| InteractionResult {
            surface: current.clone(),
            outcome: crate::InteractionOutcome::Rejected {
                reason: crate::interaction::bounded(reason),
            },
        };
        if &current.identity != identity {
            return Ok(reject(
                "Interaction identity does not match the current Session, Branch, Extension, package, or adapter",
            ));
        }
        if &current.revision != expected_revision {
            return Ok(reject(
                "Interaction revision is stale; refresh before submitting",
            ));
        }

        let session = store
            .session(identity.session_id)?
            .ok_or(SessionError::SessionNotFound(identity.session_id))?;
        let configuration = store
            .configuration(&session.current_config_hash)?
            .ok_or_else(|| {
                SessionError::ConfigurationNotFound(session.current_config_hash.clone())
            })?;
        let pin = configuration
            .configuration
            .plugins
            .iter()
            .find(|pin| pin.id == identity.extension_id)
            .ok_or_else(|| {
                EngineError::InteractionUnavailable("Extension is not pinned".to_owned())
            })?;
        let installed = self.installed_plugin(&pin.id, &pin.version, &pin.component_hash)?;
        let declaration = crate::interaction::load_interaction_declaration(&installed)?
            .ok_or_else(|| {
                EngineError::InteractionUnavailable(
                    "Extension has no declared interaction".to_owned(),
                )
            })?;

        match submission {
            InteractionSubmission::Save { target, edits } => {
                if !crate::interaction::is_save_target(
                    &declaration,
                    &current.identity.surface_id,
                    &target,
                )? {
                    return Ok(reject("Save target does not belong to this interaction"));
                }
                if !current.save.enabled {
                    return Ok(reject(
                        current
                            .save
                            .unavailable_reason
                            .as_deref()
                            .unwrap_or("Save is unavailable"),
                    ));
                }
                let mut seen = Vec::new();
                let mut pending = Vec::with_capacity(edits.len());
                for edit in edits {
                    if seen.contains(&edit.target) {
                        return Ok(reject("Submission contains a duplicate field target"));
                    }
                    seen.push(edit.target.clone());
                    let Some(field) = crate::interaction::find_field(
                        &declaration,
                        &current.identity.surface_id,
                        &edit.target,
                    )?
                    else {
                        return Ok(reject("Field target does not belong to this interaction"));
                    };
                    let surface_field = current
                        .groups
                        .iter()
                        .flat_map(|group| &group.fields)
                        .find(|candidate| candidate.target == edit.target)
                        .expect("declared field is present on surface");
                    if let Err(reason) = crate::interaction::validate_edit(
                        field,
                        &edit.value,
                        &surface_field.choices,
                    ) {
                        let mut rejected = reject(&reason);
                        if let Some(candidate) = rejected
                            .surface
                            .groups
                            .iter_mut()
                            .flat_map(|group| &mut group.fields)
                            .find(|candidate| candidate.target == edit.target)
                        {
                            candidate.error = Some(reason);
                        }
                        return Ok(rejected);
                    }
                    pending.push((
                        crate::interaction::field_property(field).to_owned(),
                        edit.value.to_json(),
                    ));
                }
                let mut next = configuration.configuration.clone();
                let next_pin = next
                    .plugins
                    .iter_mut()
                    .find(|pin| pin.id == identity.extension_id)
                    .expect("pin was resolved above");
                let settings = match &mut next_pin.settings {
                    serde_json::Value::Object(settings) => settings,
                    serde_json::Value::Null => {
                        next_pin.settings = serde_json::json!({});
                        next_pin.settings.as_object_mut().expect("created object")
                    }
                    _ => {
                        return Ok(reject(
                            "Pinned Extension settings must be an object or null",
                        ));
                    }
                };
                let mut changed = false;
                for (property, value) in pending {
                    if settings.insert(property, value.clone()).as_ref() != Some(&value) {
                        changed = true;
                    }
                }
                if !changed {
                    return Ok(InteractionResult {
                        surface: current,
                        outcome: crate::InteractionOutcome::Saved {
                            configuration_revision: configuration.revision_hash,
                        },
                    });
                }
                let record = match store.update_session_configuration_if_current(
                    identity.session_id,
                    &configuration.revision_hash,
                    next,
                ) {
                    Ok(record) => record,
                    Err(SessionError::ConfigurationConflict) => {
                        let current = self
                            .extension_interactions(store, identity.session_id, identity.branch_id)?
                            .into_iter()
                            .find(|surface| surface.identity.extension_id == identity.extension_id)
                            .ok_or_else(|| {
                                EngineError::InteractionUnavailable(
                                    "Extension interaction disappeared during Save".to_owned(),
                                )
                            })?;
                        return Ok(InteractionResult {
                            surface: current,
                            outcome: crate::InteractionOutcome::Rejected {
                                reason: "Interaction revision changed during Save; refresh before submitting"
                                    .to_owned(),
                            },
                        });
                    }
                    Err(error) => return Err(error.into()),
                };
                let surface = self
                    .extension_interactions(store, identity.session_id, identity.branch_id)?
                    .into_iter()
                    .find(|surface| surface.identity.extension_id == identity.extension_id)
                    .ok_or_else(|| {
                        EngineError::InteractionUnavailable(
                            "Extension interaction disappeared after Save".to_owned(),
                        )
                    })?;
                Ok(InteractionResult {
                    surface,
                    outcome: crate::InteractionOutcome::Saved {
                        configuration_revision: record.revision_hash,
                    },
                })
            }
            InteractionSubmission::Invoke { target } => {
                let Some(action) = crate::interaction::find_action(
                    &declaration,
                    &current.identity.surface_id,
                    &target,
                )?
                else {
                    return Ok(reject("Action target does not belong to this interaction"));
                };
                let surface_action = current
                    .actions
                    .iter()
                    .find(|candidate| candidate.target == target)
                    .expect("declared action is present on surface");
                if !surface_action.enabled {
                    return Ok(reject(
                        surface_action
                            .unavailable_reason
                            .as_deref()
                            .unwrap_or("Action is unavailable"),
                    ));
                }
                let command = crate::interaction::action_command(action)
                    .expect("enabled actions have a validated command binding");
                let result = store.invoke_plugin_command(
                    identity.session_id,
                    identity.branch_id,
                    &identity.extension_id,
                    command,
                    serde_json::Value::Null,
                )?;
                let output = result
                    .receipt
                    .effects
                    .iter()
                    .find_map(|effect| match effect {
                        crate::PluginEffect::Observe { value } => {
                            value.get("output").and_then(serde_json::Value::as_str)
                        }
                        _ => None,
                    });
                let choices = output
                    .and_then(|output| serde_json::from_str::<Vec<String>>(output).ok())
                    .unwrap_or_default();
                let mut surface = self
                    .extension_interactions(store, identity.session_id, identity.branch_id)?
                    .into_iter()
                    .find(|surface| surface.identity.extension_id == identity.extension_id)
                    .ok_or_else(|| {
                        EngineError::InteractionUnavailable(
                            "Extension interaction disappeared after action".to_owned(),
                        )
                    })?;
                if !choices.is_empty()
                    && let Some(surface_action) = surface
                        .actions
                        .iter_mut()
                        .find(|surface_action| surface_action.target == target)
                    && let Some(candidate_id) = surface_action
                        .content
                        .as_ref()
                        .map(|content| content.candidate_id)
                    && let Some(declared) = action.presentation.as_ref()
                {
                    surface_action.content = Some(crate::interaction::build_content(
                        &surface.identity.surface_id,
                        candidate_id,
                        &choices,
                        declared,
                    )?);
                }
                Ok(InteractionResult {
                    surface,
                    outcome: crate::InteractionOutcome::Invoked {
                        result: Box::new(result),
                    },
                })
            }
        }
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
            EngineQuery::ExtensionInteractions {
                session_id,
                branch_id,
            } => Ok(EngineInspection::ExtensionInteractions(
                self.extension_interactions(&store, session_id, branch_id)?,
            )),
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
                self.export_artifact_with_codec(&store, &revision_hash)?,
            )),
            EngineQuery::ArtifactCodecProvenance { revision_hash } => {
                Ok(EngineInspection::ArtifactCodecProvenance(
                    store.artifact_codec_provenance(&revision_hash)?,
                ))
            }
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
            EngineQuery::BackgroundAttempts {
                session_id,
                branch_id,
            } => Ok(EngineInspection::Attempts(
                store.background_attempts(session_id, branch_id)?,
            )),
            EngineQuery::TurnDetails {
                session_id,
                attempt_id,
            } => {
                let attempt = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let (turn_id, _) = attempt.require_primary()?;
                let turn = store
                    .turn(turn_id)?
                    .ok_or(TurnError::TurnNotFound(turn_id))?;
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
                Ok(EngineInspection::PromptPlan(
                    attempt.require_primary()?.1.clone(),
                ))
            }
            EngineQuery::PromptSegments {
                attempt_id,
                selector,
            } => {
                let attempt = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let inspection = attempt
                    .require_primary()?
                    .1
                    .inspect_segments(&selector)
                    .ok_or(EngineError::PromptSegmentNotFound {
                        attempt_id,
                        selector,
                    })?;
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
                    baseline.require_primary()?.1,
                    target_attempt_id,
                    target.require_primary()?.1,
                )))
            }
            EngineQuery::PreviousPromptDiff { attempt_id } => {
                let target = store
                    .attempt(attempt_id)?
                    .ok_or(TurnError::AttemptNotFound(attempt_id))?;
                let baseline = previous_selected_attempt(&store, &target)?;
                Ok(EngineInspection::PromptDiff(diff_prompt_plans(
                    baseline.attempt_id,
                    baseline.require_primary()?.1,
                    attempt_id,
                    target.require_primary()?.1,
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
        let inference = match &self.inference {
            Some(broker) => broker.clone(),
            None => crate::InferenceBroker::live(Config::load(&self.config_directory)?),
        };
        store.set_inference_broker(inference);
        match command {
            EngineCommand::InstallPlugin { directory } => {
                let installed = self.plugin_registry().install(&directory)?;
                if let Some(package) = DEFAULT_PACKAGES
                    .iter()
                    .find(|package| package.id == installed.manifest.id)
                {
                    self.clear_default_opt_out(package.id)?;
                    if package.artifact_inspector {
                        store.register_artifact_inspector(&ArtifactInspectorRegistration {
                            id: installed.manifest.id.clone(),
                            version: installed.manifest.version.clone(),
                            component_sha256: installed.manifest.component_sha256.clone(),
                            capabilities: installed.manifest.requested_capabilities.clone(),
                        })?;
                    }
                }
                Ok(EngineResult::InstalledPlugin(installed))
            }
            EngineCommand::ImportExtension { directory } => Ok(EngineResult::ImportedExtension(
                self.plugin_registry().import_native_extension(&directory)?,
            )),
            EngineCommand::EnableGlobalExtension {
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
                let selection = GlobalExtensionSelection {
                    id: installed.manifest.id,
                    pin: GlobalExtensionPin {
                        version: installed.manifest.version.to_string(),
                        digest: installed.manifest.component_sha256,
                        settings,
                        egress_allow_list: egress,
                    },
                };
                Config::save_enabled_extension(
                    &self.config_directory,
                    &selection.id,
                    selection.pin.clone(),
                )?;
                Ok(EngineResult::GlobalExtensionEnabled(selection))
            }
            EngineCommand::DisableGlobalExtension { id } => {
                Ok(EngineResult::GlobalExtensionDisabled {
                    removed: Config::remove_enabled_extension(&self.config_directory, &id)?,
                    id,
                })
            }
            EngineCommand::RestoreDefaultPlugins => {
                for package in DEFAULT_PACKAGES {
                    self.clear_default_opt_out(package.id)?;
                }
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
                if store.plugin_in_use(&plugin_id)?
                    || store.artifact_codec_plugin_in_use(&plugin_id)?
                {
                    return Err(EngineError::PluginInUse(plugin_id));
                }
                store.unregister_artifact_inspector(&plugin_id)?;
                if DEFAULT_PACKAGES
                    .iter()
                    .any(|package| package.id == plugin_id)
                {
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
                if capabilities.contains(&PluginCapability::ArtifactCodec) {
                    self.codec_plugin(&id, &version, &digest, &capabilities)?;
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
                let pin = PluginPin {
                    id,
                    version: installed.manifest.version.to_string(),
                    component_hash: installed.manifest.component_sha256,
                    capabilities,
                    settings,
                    egress_allow_list: egress,
                    enabled: true,
                };
                Ok(EngineResult::Configuration(Box::new(
                    adopt_extension_configuration(&mut store, session_id, pin)?,
                )))
            }
            EngineCommand::SetExtensionEnabled {
                session_id,
                id,
                enabled,
            } => {
                let configuration = selected_session_configuration(&store, session_id)?;
                if configuration.plugins.iter().any(|pin| pin.id == id) {
                    Ok(EngineResult::Configuration(Box::new(set_plugin_enabled(
                        &mut store,
                        &self.plugin_registry(),
                        session_id,
                        &id,
                        enabled,
                        true,
                    )?)))
                } else if enabled {
                    let pin = Config::load(&self.config_directory)
                        .map_err(|error| EngineError::Config(Box::new(error)))?
                        .resolve_enabled_extension(&self.plugin_registry(), &id)
                        .map_err(|error| EngineError::Config(Box::new(error)))?;
                    Ok(EngineResult::Configuration(Box::new(
                        adopt_extension_configuration(&mut store, session_id, pin)?,
                    )))
                } else {
                    Err(EngineError::PluginNotPinned(id))
                }
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
            } => Ok(EngineResult::Configuration(Box::new(set_plugin_enabled(
                &mut store,
                &self.plugin_registry(),
                session_id,
                &id,
                enabled,
                false,
            )?))),
            EngineCommand::ImportArtifact { source } => {
                let bundle = match self.decode_artifact_with_codec(&store, &source)? {
                    Some((codec_bundle, provenance)) => {
                        store.import_artifact_from_codec(&source, &codec_bundle, &provenance)?
                    }
                    None => store.import_artifact_bundle(&source)?,
                };
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
            } => {
                let mut configuration = *configuration;
                let explicit_ids = configuration
                    .plugins
                    .iter()
                    .map(|pin| pin.id.as_str())
                    .collect::<BTreeSet<_>>();
                let mut enabled_extensions = Config::load(&self.config_directory)
                    .map_err(|error| EngineError::Config(Box::new(error)))?
                    .resolve_enabled_extensions(&self.plugin_registry())
                    .map_err(|error| EngineError::Config(Box::new(error)))?;
                enabled_extensions.retain(|pin| !explicit_ids.contains(pin.id.as_str()));
                enabled_extensions.extend(configuration.plugins);
                configuration.plugins = enabled_extensions;
                Ok(EngineResult::CreatedSession(Box::new(
                    store.create_session(configuration, greeting_index)?,
                )))
            }
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
            EngineCommand::SubmitExtensionInteraction {
                identity,
                expected_revision,
                submission,
            } => Ok(EngineResult::ExtensionInteraction(Box::new(
                self.submit_extension_interaction(
                    &mut store,
                    &identity,
                    &expected_revision,
                    submission,
                )?,
            ))),
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
                branch_id,
                plugin_id,
                command,
                arguments,
            } => Ok(EngineResult::PluginCommand(Box::new(
                store.invoke_plugin_command(
                    session_id, branch_id, &plugin_id, &command, arguments,
                )?,
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
    ExtensionInteractions {
        session_id: EntityId,
        branch_id: Option<EntityId>,
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
    ArtifactCodecProvenance {
        revision_hash: ContentHash,
    },
    BranchTurns {
        branch_id: EntityId,
    },
    Attempt {
        attempt_id: EntityId,
    },
    BackgroundAttempts {
        session_id: EntityId,
        branch_id: Option<EntityId>,
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
    EnableGlobalExtension {
        id: String,
        version: String,
        digest: ContentHash,
        settings: serde_json::Value,
        egress: Vec<crate::EgressAllowance>,
    },
    DisableGlobalExtension {
        id: String,
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
    SubmitExtensionInteraction {
        identity: crate::InteractionIdentity,
        expected_revision: ContentHash,
        submission: InteractionSubmission,
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
    SetExtensionEnabled {
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
        branch_id: Option<EntityId>,
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
    GlobalExtensionEnabled(GlobalExtensionSelection),
    GlobalExtensionDisabled {
        id: String,
        removed: bool,
    },
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
    ExtensionInteraction(Box<InteractionResult>),
    PromptOrderUpdated {
        artifact: ArtifactRecord,
        configuration: Option<Box<SessionConfigurationRecord>>,
    },
    EditedCandidate(EditedCandidate),
    DryRun(Box<DryRunResult>),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GlobalExtensionSelection {
    pub id: String,
    pub pin: GlobalExtensionPin,
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
    ExtensionInteractions(Vec<InteractionSurface>),
    Turns(Vec<EngineTurn>),
    Artifacts(Vec<ArtifactRecord>),
    Artifact(ArtifactRecord),
    ArtifactSource(Vec<u8>),
    ArtifactCodecProvenance(Option<ArtifactCodecProvenance>),
    Attempt(AttemptProjection),
    Attempts(Vec<AttemptProjection>),
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
    if attempt.session_id != session_id {
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

fn adopt_extension_configuration(
    store: &mut Store,
    session_id: EntityId,
    pin: PluginPin,
) -> Result<SessionConfigurationRecord, EngineError> {
    let mut configuration = selected_session_configuration(store, session_id)?;
    let already_pinned = configuration
        .plugins
        .iter()
        .any(|existing| existing.id == pin.id);
    let mid_session = !already_pinned
        && store
            .trace_events(Some(session_id))?
            .iter()
            .any(|event| event.event_type == "turn.created");
    configuration
        .plugins
        .retain(|existing| existing.id != pin.id);
    configuration.plugins.push(pin.clone());
    if mid_session {
        let warning = CompatibilityWarning {
            code: "extension-adopted-mid-session".to_owned(),
            profile_id: configuration.compatibility_profile.clone(),
            non_blocking: true,
            source_revision: pin.component_hash.clone(),
            affected_identifiers: vec![pin.id.clone()],
            count: 1,
            detail: format!(
                "Extension '{}' begins lifecycle observation at Session Configuration Revision adoption; prior lifecycle events are not re-emitted",
                pin.id
            ),
        };
        crate::st_bridge::reset_context(session_id, &pin.id, &pin.component_hash, true)?;
        Ok(store.adopt_extension_configuration(session_id, configuration, &pin, &warning)?)
    } else {
        Ok(store.update_session_configuration(session_id, configuration)?)
    }
}

fn set_plugin_enabled(
    store: &mut Store,
    registry: &PluginRegistry,
    session_id: EntityId,
    id: &str,
    enabled: bool,
    extension_required: bool,
) -> Result<SessionConfigurationRecord, EngineError> {
    let mut configuration = selected_session_configuration(store, session_id)?;
    let pin = configuration
        .plugins
        .iter_mut()
        .find(|pin| pin.id == id)
        .ok_or_else(|| EngineError::PluginNotPinned(id.to_owned()))?;
    let was_enabled = pin.enabled;
    let component_hash = pin.component_hash.clone();
    let installed = if enabled || extension_required {
        let version = semver::Version::parse(&pin.version)
            .map_err(|_| SessionError::InvalidPluginVersion(pin.version.clone()))?;
        Some(
            registry
                .find_pinned(id, &version, &component_hash)?
                .ok_or_else(|| EngineError::PluginNotFound {
                    id: id.to_owned(),
                    version: pin.version.clone(),
                    digest: component_hash.clone(),
                })?,
        )
    } else {
        None
    };
    if extension_required
        && installed
            .as_ref()
            .is_none_or(|plugin| plugin.manifest.runtime != crate::PluginRuntime::StBridge)
    {
        return Err(EngineError::ExtensionRuntimeRequired(id.to_owned()));
    }
    pin.enabled = enabled;
    if was_enabled != enabled
        && installed
            .as_ref()
            .is_some_and(|plugin| plugin.manifest.runtime == crate::PluginRuntime::StBridge)
    {
        crate::st_bridge::reset_context(session_id, id, &component_hash, false)?;
    }
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
fn selected_content_candidate(
    store: &Store,
    branch_id: EntityId,
) -> Result<Option<EntityId>, TurnError> {
    for turn in store.turns_for_branch(branch_id)?.into_iter().rev() {
        if let Some(candidate_id) = turn.selected_candidate_id {
            let completed = store
                .attempts_for_turn(turn.turn_id)?
                .into_iter()
                .any(|attempt| {
                    attempt.kind == crate::AttemptKind::Primary
                        && attempt.status == crate::AttemptStatus::Completed
                });
            if completed {
                return Ok(Some(candidate_id));
            }
        }
    }
    Ok(None)
}

fn previous_selected_attempt(
    store: &Store,
    target: &AttemptProjection,
) -> Result<AttemptProjection, TurnError> {
    let (turn_id, _) = target.require_primary()?;
    let turn = store
        .turn(turn_id)?
        .ok_or(TurnError::TurnNotFound(turn_id))?;
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
    ArtifactCodec(#[from] ArtifactCodecError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    #[error(transparent)]
    Tokenizer(#[from] TokenizerError),
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error(transparent)]
    Config(Box<ConfigError>),
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
    #[error("Extension interaction is unavailable: {0}")]
    InteractionUnavailable(String),
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

impl From<ConfigError> for EngineError {
    fn from(error: ConfigError) -> Self {
        Self::Config(Box::new(error))
    }
}
