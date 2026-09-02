//! Capability-limited Plugin package, grant, execution, and receipt boundary.
//!
//! `st-bridge` Plugins run SillyTavern Extensions in persistent per-Session
//! QuickJS contexts; stateless `script` Plugins use the separate script path.

use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};
use thiserror::Error;
use wasmtime::component::{Component, Linker};
use wasmtime::{Config, Engine, Store, StoreLimits, StoreLimitsBuilder};

use crate::{ChatRole, ContentHash, StateKey, decode_unique_json};

const MANIFEST_SCHEMA: &str = "stcli.plugin-manifest/v1";
const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginCapability {
    ObserveLifecycle,
    RegisterMacro,
    RegisterCommand,
    ContributePrompt,
    ReadSession,
    WriteOwnState,
    AbortPreRequest,
    InspectArtifact,
    BrokeredEgress,
    #[serde(rename = "secondary-inference")]
    InferenceCapability,
}
impl std::str::FromStr for PluginCapability {
    type Err = PluginError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "observe-lifecycle" => Ok(Self::ObserveLifecycle),
            "register-macro" => Ok(Self::RegisterMacro),
            "register-command" => Ok(Self::RegisterCommand),
            "contribute-prompt" => Ok(Self::ContributePrompt),
            "read-session" => Ok(Self::ReadSession),
            "write-own-state" => Ok(Self::WriteOwnState),
            "abort-pre-request" => Ok(Self::AbortPreRequest),
            "inspect-artifact" => Ok(Self::InspectArtifact),
            "brokered-egress" => Ok(Self::BrokeredEgress),
            "secondary-inference" => Ok(Self::InferenceCapability),
            _ => Err(PluginError::UnknownCapability(value.to_owned())),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginEvent {
    PreLore,
    PrePrompt,
    PreRequest,
    PostCommit,
    Command,
    ChatCompletionPromptReady,
    InspectArtifact,
    GenerateInterceptor,
    StBridgeLifecycle,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginRuntime {
    #[default]
    Wasm,
    Script,
    StBridge,
}

impl PluginRuntime {
    fn is_wasm(&self) -> bool {
        matches!(self, Self::Wasm)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum PromptSlot {
    BeforeCharacterDefinitions,
    AfterCharacterDefinitions,
    BeforeExampleMessages,
    AfterExampleMessages,
    NamedLoreOutlet,
    InChat,
    BeforeHistory,
    AfterHistory,
    PostHistoryInstructions,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PluginDependency {
    pub id: String,
    pub version: VersionReq,
}
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginManifest {
    pub schema: String,
    pub id: String,
    pub version: Version,
    pub engine: VersionReq,
    #[serde(default, skip_serializing_if = "PluginRuntime::is_wasm")]
    pub runtime: PluginRuntime,
    pub component: String,
    pub component_sha256: ContentHash,
    pub dependencies: Vec<PluginDependency>,
    pub license: String,
    pub subscriptions: BTreeSet<PluginEvent>,
    pub prompt_slots: BTreeSet<PromptSlot>,
    pub commands: BTreeSet<String>,
    pub macros: BTreeSet<String>,
    pub settings_schema: Option<String>,
    pub requested_capabilities: BTreeSet<PluginCapability>,
    #[serde(default)]
    pub before: BTreeSet<String>,
    #[serde(default)]
    pub after: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate_interceptor: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InstalledPlugin {
    pub manifest: PluginManifest,
    pub directory: PathBuf,
    #[serde(default)]
    pub inspection_enabled: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginGrant {
    pub id: String,
    pub version: Version,
    pub component_sha256: ContentHash,
    pub capabilities: BTreeSet<PluginCapability>,
    pub settings: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress_allow_list: Vec<crate::EgressAllowance>,
    pub enabled: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ArtifactInspectorRegistration {
    pub id: String,
    pub version: Version,
    pub component_sha256: ContentHash,
    pub capabilities: BTreeSet<PluginCapability>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginInput {
    pub event: PluginEvent,
    pub plugin_id: String,
    pub settings: Value,
    #[serde(default)]
    pub context: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub state: Value,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub artifact: Value,
    pub session: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptContribution {
    pub slot: PromptSlot,
    pub name: String,
    pub role: String,
    pub content: String,
    pub depth: Option<usize>,
    pub order: usize,
    pub outlet: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptRewriteMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "effect", rename_all = "kebab-case")]
pub enum PluginEffect {
    Observe { value: Value },
    Output { value: Value },
    RegisterMacro { name: String, value: String },
    RegisterCommand { name: String, description: String },
    Prompt { contribution: PromptContribution },
    PromptRewrite { messages: Vec<PromptRewriteMessage> },
    StateWrite { key: StateKey, value: Value },
    Abort { code: String, message: String },
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginOutput {
    pub effects: Vec<PluginEffect>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct PluginReceipt {
    pub id: String,
    pub manifest: PluginManifest,
    pub version: Version,
    pub component_sha256: ContentHash,
    pub grants: BTreeSet<PluginCapability>,
    pub event: PluginEvent,
    pub input: PluginInput,
    pub effects: Vec<PluginEffect>,
    pub fuel_consumed: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub script_logs: Vec<ScriptLog>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub egress: Vec<crate::EgressReceipt>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inference: Vec<crate::InferenceReceipt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prng_seed: Option<u64>,
}
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ScriptLog {
    pub level: String,
    pub message: String,
}

#[derive(Clone, Copy, Debug)]
pub struct ScriptLimits {
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub interrupt_ticks: u64,
    pub log_entries: usize,
    pub log_message_bytes: usize,
    pub microtask_jobs: usize,
}

impl Default for ScriptLimits {
    fn default() -> Self {
        Self {
            memory_bytes: 16 * 1024 * 1024,
            stack_bytes: 256 * 1024,
            interrupt_ticks: 200,
            log_entries: 64,
            log_message_bytes: 2048,
            microtask_jobs: 64,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PluginLimits {
    pub component_bytes: usize,
    pub input_bytes: usize,
    pub output_bytes: usize,
    pub memory_bytes: usize,
    pub fuel: u64,
    pub timeout: Duration,
    pub script: ScriptLimits,
}

impl Default for PluginLimits {
    fn default() -> Self {
        Self {
            component_bytes: 4 * 1024 * 1024,
            input_bytes: 256 * 1024,
            output_bytes: 256 * 1024,
            memory_bytes: 16 * 1024 * 1024,
            fuel: 10_000_000,
            timeout: Duration::from_millis(250),
            script: ScriptLimits::default(),
        }
    }
}

pub struct PluginHost {
    limits: PluginLimits,
    egress: Option<crate::EgressBroker>,
    inference: Option<crate::InferenceBroker>,
}

impl PluginHost {
    pub fn new(limits: PluginLimits) -> Self {
        Self {
            limits,
            egress: None,
            inference: None,
        }
    }

    pub fn with_egress(limits: PluginLimits, broker: crate::EgressBroker) -> Self {
        Self {
            limits,
            egress: Some(broker),
            inference: None,
        }
    }

    pub fn with_inference(mut self, broker: crate::InferenceBroker) -> Self {
        self.inference = Some(broker);
        self
    }

    pub fn execute(
        &self,
        installed: &InstalledPlugin,
        grant: &PluginGrant,
        mut input: PluginInput,
    ) -> Result<PluginReceipt, PluginError> {
        validate_grant(installed, grant)?;
        if input.event != PluginEvent::Command
            && !installed.manifest.subscriptions.contains(&input.event)
        {
            return Err(PluginError::EventNotSubscribed(input.event));
        }
        input.plugin_id = installed.manifest.id.clone();
        input.settings = grant.settings.clone();
        if installed.manifest.runtime.is_wasm() {
            input.state = Value::Null;
        }
        let input_json = serde_json::to_string(&input)?;
        if input_json.len() > self.limits.input_bytes {
            return Err(PluginError::InputLimit);
        }
        let component_path = installed.directory.join(&installed.manifest.component);
        let component_bytes = fs::read(&component_path).map_err(|source| PluginError::Read {
            path: component_path.clone(),
            source,
        })?;
        if component_bytes.len() > self.limits.component_bytes {
            return Err(PluginError::ComponentLimit);
        }
        let digest = plugin_digest(&component_bytes);
        if digest != installed.manifest.component_sha256 || digest != grant.component_sha256 {
            return Err(PluginError::DigestMismatch);
        }
        let mut egress_receipts = Vec::new();
        let mut inference_receipts = Vec::new();
        let (effects, fuel_consumed, script_logs, prng_seed) = match installed.manifest.runtime {
            PluginRuntime::Wasm => {
                let (output_json, fuel_consumed) =
                    self.execute_wasm(&component_bytes, &input_json)?;
                if output_json.len() > self.limits.output_bytes {
                    return Err(PluginError::OutputLimit);
                }
                let output = serde_json::from_str::<PluginOutput>(&output_json)?;
                (output.effects, fuel_consumed, Vec::new(), None)
            }
            PluginRuntime::Script => {
                #[cfg(feature = "scripting")]
                {
                    let source = String::from_utf8(component_bytes)
                        .map_err(|_| PluginError::ScriptNotUtf8(installed.manifest.id.clone()))?;
                    let outcome = crate::script::execute(
                        &installed.manifest.id,
                        &source,
                        input.event,
                        &input_json,
                        self.limits.script,
                    )?;
                    let output_json = serde_json::to_string(&PluginOutput {
                        effects: outcome.effects.clone(),
                    })?;
                    if output_json.len() > self.limits.output_bytes {
                        return Err(PluginError::OutputLimit);
                    }
                    (outcome.effects, 0, outcome.logs, outcome.prng_seed)
                }
                #[cfg(not(feature = "scripting"))]
                return Err(PluginError::ScriptingUnavailable(
                    installed.manifest.id.clone(),
                ));
            }
            PluginRuntime::StBridge => {
                #[cfg(feature = "scripting")]
                {
                    let source = String::from_utf8(component_bytes)
                        .map_err(|_| PluginError::ScriptNotUtf8(installed.manifest.id.clone()))?;
                    let policy = crate::EgressPolicy {
                        capability_granted: grant
                            .capabilities
                            .contains(&PluginCapability::BrokeredEgress),
                        allowances: grant.egress_allow_list.clone(),
                    };
                    let invocation = self.egress.clone().map(|broker| {
                        let mode = if input
                            .session
                            .get("dry_run")
                            .and_then(Value::as_bool)
                            .unwrap_or(false)
                        {
                            crate::EgressMode::DryRun
                        } else {
                            crate::EgressMode::Live
                        };
                        crate::EgressInvocation {
                            broker,
                            policy,
                            mode,
                        }
                    });
                    let inference =
                        self.inference
                            .clone()
                            .map(|broker| crate::InferenceInvocation {
                                broker,
                                policy: crate::InferencePolicy {
                                    capability_granted: grant
                                        .capabilities
                                        .contains(&PluginCapability::InferenceCapability),
                                    mode: if input
                                        .session
                                        .get("dry_run")
                                        .and_then(Value::as_bool)
                                        .unwrap_or(false)
                                    {
                                        crate::InferenceMode::DryRun
                                    } else {
                                        crate::InferenceMode::Live
                                    },
                                },
                                default_profile: input
                                    .session
                                    .get("provider_profile")
                                    .and_then(Value::as_str)
                                    .unwrap_or_default()
                                    .to_owned(),
                            });
                    let outcome = crate::st_bridge::execute(
                        installed,
                        &input,
                        &source,
                        self.limits.script,
                        invocation,
                        inference,
                    )?;
                    let output_json = serde_json::to_string(&PluginOutput {
                        effects: outcome.effects.clone(),
                    })?;
                    if output_json.len() > self.limits.output_bytes {
                        return Err(PluginError::OutputLimit);
                    }
                    egress_receipts = outcome.egress_receipts;
                    inference_receipts = outcome.inference_receipts;
                    (outcome.effects, 0, outcome.logs, outcome.prng_seed)
                }
                #[cfg(not(feature = "scripting"))]
                {
                    return Err(PluginError::ScriptingUnavailable(
                        installed.manifest.id.clone(),
                    ));
                }
            }
        };
        validate_effects(installed, grant, input.event, &effects)?;
        Ok(PluginReceipt {
            manifest: installed.manifest.clone(),
            id: installed.manifest.id.clone(),
            version: installed.manifest.version.clone(),
            component_sha256: digest,
            grants: grant.capabilities.clone(),
            event: input.event,
            input,
            effects,
            egress: egress_receipts,
            inference: inference_receipts,
            fuel_consumed,
            script_logs,
            prng_seed,
        })
    }

    fn execute_wasm(
        &self,
        component_bytes: &[u8],
        input_json: &str,
    ) -> Result<(String, u64), PluginError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        let engine = Engine::new(&config).map_err(PluginError::Wasmtime)?;
        let component = Component::new(&engine, component_bytes).map_err(PluginError::Wasmtime)?;
        let linker = Linker::new(&engine);
        let limits = StoreLimitsBuilder::new()
            .memory_size(self.limits.memory_bytes)
            .instances(1)
            .memories(2)
            .tables(2)
            .build();
        let mut store = Store::new(&engine, limits);
        store.limiter(|limits: &mut StoreLimits| limits);
        store
            .set_fuel(self.limits.fuel)
            .map_err(PluginError::Wasmtime)?;
        store.set_epoch_deadline(1);
        let (cancel_timer, timer_signal) = mpsc::sync_channel(1);
        let timer_engine = engine.clone();
        let timeout = self.limits.timeout;
        let timer = thread::spawn(move || {
            if timer_signal.recv_timeout(timeout).is_err() {
                timer_engine.increment_epoch();
            }
        });
        let result = (|| {
            let instance = linker
                .instantiate(&mut store, &component)
                .map_err(PluginError::Wasmtime)?;
            let run = instance
                .get_typed_func::<(String,), (Result<String, String>,)>(&mut store, "run")
                .map_err(PluginError::Wasmtime)?;
            run.call(&mut store, (input_json.to_owned(),))
                .map_err(PluginError::Wasmtime)
        })();
        let _ = cancel_timer.send(());
        timer.join().map_err(|_| PluginError::TimerPanicked)?;
        let (guest_result,) = result?;
        let output_json = guest_result.map_err(PluginError::Guest)?;
        let remaining = store.get_fuel().map_err(PluginError::Wasmtime)?;
        Ok((output_json, self.limits.fuel - remaining))
    }
}

pub struct PluginRegistry {
    root: PathBuf,
}

impl PluginRegistry {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn doctor(&self, directory: &Path) -> Result<InstalledPlugin, PluginError> {
        let manifest_path = directory.join("manifest.json");
        let source = fs::read(&manifest_path).map_err(|source| PluginError::Read {
            path: manifest_path,
            source,
        })?;
        let value = decode_unique_json(&source).map_err(PluginError::Artifact)?;
        let manifest = serde_json::from_value::<PluginManifest>(value)?;
        validate_manifest(&manifest, directory)?;
        Ok(InstalledPlugin {
            manifest,
            directory: directory.to_owned(),
            inspection_enabled: false,
        })
    }

    pub fn install(&self, directory: &Path) -> Result<InstalledPlugin, PluginError> {
        let checked = self.doctor(directory)?;
        let destination = self
            .root
            .join(&checked.manifest.id)
            .join(checked.manifest.version.to_string())
            .join(
                checked
                    .manifest
                    .component_sha256
                    .to_string()
                    .replace(':', "-"),
            );
        if destination.exists() {
            return self.doctor(&destination);
        }
        fs::create_dir_all(&destination).map_err(|source| PluginError::Create {
            path: destination.clone(),
            source,
        })?;
        for name in ["manifest.json", checked.manifest.component.as_str()] {
            fs::copy(directory.join(name), destination.join(name)).map_err(|source| {
                PluginError::Copy {
                    from: directory.join(name),
                    to: destination.join(name),
                    source,
                }
            })?;
        }
        if let Some(schema) = &checked.manifest.settings_schema {
            fs::copy(directory.join(schema), destination.join(schema)).map_err(|source| {
                PluginError::Copy {
                    from: directory.join(schema),
                    to: destination.join(schema),
                    source,
                }
            })?;
        }
        self.doctor(&destination)
    }

    pub fn list(&self) -> Result<Vec<InstalledPlugin>, PluginError> {
        if !self.root.exists() {
            return Ok(Vec::new());
        }
        let mut installed = Vec::new();
        for id in read_directories(&self.root)? {
            for version in read_directories(&id)? {
                for digest in read_directories(&version)? {
                    installed.push(self.doctor(&digest)?);
                }
            }
        }
        installed.sort_by(|left, right| {
            left.manifest
                .id
                .cmp(&right.manifest.id)
                .then_with(|| left.manifest.version.cmp(&right.manifest.version))
                .then_with(|| {
                    left.manifest
                        .component_sha256
                        .to_string()
                        .cmp(&right.manifest.component_sha256.to_string())
                })
        });
        Ok(installed)
    }

    pub fn find(
        &self,
        id: &str,
        digest: &ContentHash,
    ) -> Result<Option<InstalledPlugin>, PluginError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|plugin| plugin.manifest.id == id && &plugin.manifest.component_sha256 == digest))
    }
    pub fn find_pinned(
        &self,
        id: &str,
        version: &Version,
        digest: &ContentHash,
    ) -> Result<Option<InstalledPlugin>, PluginError> {
        Ok(self.list()?.into_iter().find(|plugin| {
            plugin.manifest.id == id
                && &plugin.manifest.version == version
                && &plugin.manifest.component_sha256 == digest
        }))
    }

    pub fn remove(&self, id: &str) -> Result<bool, PluginError> {
        let path = self.root.join(id);
        if !path.exists() {
            return Ok(false);
        }
        fs::remove_dir_all(&path).map_err(|source| PluginError::Remove { path, source })?;
        Ok(true)
    }
}

pub fn order_plugins(plugins: &[InstalledPlugin]) -> Result<Vec<InstalledPlugin>, PluginError> {
    let by_id = plugins
        .iter()
        .map(|plugin| (plugin.manifest.id.as_str(), plugin))
        .collect::<BTreeMap<_, _>>();
    let mut edges = BTreeMap::<&str, BTreeSet<&str>>::new();
    let mut indegree = plugins
        .iter()
        .map(|plugin| (plugin.manifest.id.as_str(), 0usize))
        .collect::<BTreeMap<_, _>>();
    for plugin in plugins {
        for dependency in &plugin.manifest.dependencies {
            let target = by_id.get(dependency.id.as_str()).ok_or_else(|| {
                PluginError::MissingDependency {
                    plugin: plugin.manifest.id.clone(),
                    dependency: dependency.id.clone(),
                }
            })?;
            if !dependency.version.matches(&target.manifest.version) {
                return Err(PluginError::DependencyVersion {
                    plugin: plugin.manifest.id.clone(),
                    dependency: dependency.id.clone(),
                });
            }
            add_edge(
                &mut edges,
                &mut indegree,
                dependency.id.as_str(),
                plugin.manifest.id.as_str(),
            );
        }
        for before in &plugin.manifest.before {
            if by_id.contains_key(before.as_str()) {
                add_edge(
                    &mut edges,
                    &mut indegree,
                    plugin.manifest.id.as_str(),
                    before,
                );
            }
        }
        for after in &plugin.manifest.after {
            if by_id.contains_key(after.as_str()) {
                add_edge(
                    &mut edges,
                    &mut indegree,
                    after,
                    plugin.manifest.id.as_str(),
                );
            }
        }
    }
    let mut ready = indegree
        .iter()
        .filter_map(|(id, degree)| (*degree == 0).then_some(*id))
        .collect::<VecDeque<_>>();
    let mut ordered = Vec::with_capacity(plugins.len());
    while let Some(id) = ready.pop_front() {
        ordered.push((*by_id[id]).clone());
        if let Some(targets) = edges.get(id) {
            for target in targets {
                let degree = indegree.get_mut(target).expect("known plugin");
                *degree -= 1;
                if *degree == 0 {
                    let position = ready
                        .iter()
                        .position(|queued| queued > target)
                        .unwrap_or(ready.len());
                    ready.insert(position, target);
                }
            }
        }
    }
    if ordered.len() != plugins.len() {
        return Err(PluginError::DependencyCycle);
    }
    Ok(ordered)
}

pub fn plugin_digest(bytes: &[u8]) -> ContentHash {
    ContentHash::new(Sha256::digest(bytes).into())
}

fn validate_manifest(manifest: &PluginManifest, directory: &Path) -> Result<(), PluginError> {
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(PluginError::UnsupportedManifest(manifest.schema.clone()));
    }
    if !valid_id(&manifest.id) {
        return Err(PluginError::InvalidId(manifest.id.clone()));
    }
    let engine = Version::parse(ENGINE_VERSION).expect("package version is semantic");
    if !manifest.engine.matches(&engine) {
        return Err(PluginError::EngineVersion);
    }
    spdx::Expression::parse(&manifest.license)
        .map_err(|_| PluginError::InvalidLicense(manifest.license.clone()))?;
    let component_path = safe_child(directory, &manifest.component)?;
    let bytes = fs::read(&component_path).map_err(|source| PluginError::Read {
        path: component_path,
        source,
    })?;
    if plugin_digest(&bytes) != manifest.component_sha256 {
        return Err(PluginError::DigestMismatch);
    }
    if let Some(schema) = &manifest.settings_schema {
        let path = safe_child(directory, schema)?;
        let source = fs::read(&path).map_err(|source| PluginError::Read { path, source })?;
        decode_unique_json(&source).map_err(PluginError::Artifact)?;
    }

    // Validate generate_interceptor field
    if let Some(interceptor_name) = &manifest.generate_interceptor {
        if manifest.runtime != PluginRuntime::StBridge {
            return Err(PluginError::InvalidManifest(
                "generate_interceptor is only valid for st-bridge runtime".to_owned(),
            ));
        }
        if interceptor_name.is_empty()
            || !interceptor_name
                .chars()
                .all(|c| c.is_alphanumeric() || c == '_' || c == '$')
        {
            return Err(PluginError::InvalidManifest(format!(
                "generate_interceptor must be a valid JavaScript identifier, got: {}",
                interceptor_name
            )));
        }
        if !manifest
            .subscriptions
            .contains(&PluginEvent::GenerateInterceptor)
        {
            return Err(PluginError::InvalidManifest(
                "generate_interceptor requires generate-interceptor subscription".to_owned(),
            ));
        }
    } else if manifest
        .subscriptions
        .contains(&PluginEvent::GenerateInterceptor)
    {
        return Err(PluginError::InvalidManifest(
            "generate-interceptor subscription requires generate_interceptor field".to_owned(),
        ));
    }

    Ok(())
}

pub fn validate_recorded_receipt(receipt: &PluginReceipt) -> Result<(), PluginError> {
    if receipt.id != receipt.manifest.id
        || receipt.version != receipt.manifest.version
        || receipt.component_sha256 != receipt.manifest.component_sha256
        || !receipt
            .grants
            .is_subset(&receipt.manifest.requested_capabilities)
        || receipt.input.plugin_id != receipt.id
        || receipt.input.event != receipt.event
    {
        return Err(PluginError::RecordedReceiptInvalid(receipt.id.clone()));
    }
    if receipt
        .inference
        .iter()
        .any(|inference| crate::validate_inference_receipt(inference).is_err())
    {
        return Err(PluginError::RecordedReceiptInvalid(receipt.id.clone()));
    }
    let installed = InstalledPlugin {
        manifest: receipt.manifest.clone(),
        directory: PathBuf::new(),
        inspection_enabled: false,
    };
    let grant = PluginGrant {
        id: receipt.id.clone(),
        version: receipt.version.clone(),
        component_sha256: receipt.component_sha256.clone(),
        capabilities: receipt.grants.clone(),
        settings: receipt.input.settings.clone(),
        egress_allow_list: Vec::new(),
        enabled: true,
    };
    validate_effects(&installed, &grant, receipt.event, &receipt.effects)
}

fn validate_grant(installed: &InstalledPlugin, grant: &PluginGrant) -> Result<(), PluginError> {
    if !grant.enabled {
        return Err(PluginError::Disabled);
    }
    if grant.id != installed.manifest.id
        || grant.version != installed.manifest.version
        || grant.component_sha256 != installed.manifest.component_sha256
    {
        return Err(PluginError::GrantPinMismatch);
    }
    if !grant
        .capabilities
        .is_subset(&installed.manifest.requested_capabilities)
    {
        return Err(PluginError::GrantExceedsRequest);
    }
    Ok(())
}

fn validate_effects(
    installed: &InstalledPlugin,
    grant: &PluginGrant,
    event: PluginEvent,
    effects: &[PluginEffect],
) -> Result<(), PluginError> {
    if event == PluginEvent::InspectArtifact
        && effects
            .iter()
            .any(|effect| !matches!(effect, PluginEffect::Output { .. }))
    {
        return Err(PluginError::ArtifactInspectionMutationDenied);
    }
    for effect in effects {
        if event != PluginEvent::InspectArtifact && matches!(effect, PluginEffect::Output { .. }) {
            return Err(PluginError::ArtifactInspectionOutputPhaseDenied);
        }
        if event == PluginEvent::Command
            && !matches!(
                effect,
                PluginEffect::Observe { .. } | PluginEffect::StateWrite { .. }
            )
        {
            return Err(PluginError::CommandEffectDenied);
        }
        if event == PluginEvent::PostCommit && !matches!(effect, PluginEffect::Observe { .. }) {
            return Err(PluginError::PostCommitMutationDenied);
        }
        if event == PluginEvent::StBridgeLifecycle
            && !matches!(effect, PluginEffect::Observe { .. })
        {
            return Err(PluginError::LifecycleObservationOnly);
        }
        let capability = match effect {
            PluginEffect::Output { .. } => PluginCapability::InspectArtifact,
            PluginEffect::Observe { .. } => PluginCapability::ObserveLifecycle,
            PluginEffect::RegisterMacro { name, .. } => {
                if !installed.manifest.macros.contains(name) {
                    return Err(PluginError::UndeclaredMacro(name.clone()));
                }
                PluginCapability::RegisterMacro
            }
            PluginEffect::RegisterCommand { name, .. } => {
                if !installed.manifest.commands.contains(name) {
                    return Err(PluginError::UndeclaredCommand(name.clone()));
                }
                PluginCapability::RegisterCommand
            }
            PluginEffect::Prompt { contribution } => {
                if !installed.manifest.prompt_slots.contains(&contribution.slot) {
                    return Err(PluginError::ClosedPromptSlot(contribution.slot));
                }
                PluginCapability::ContributePrompt
            }
            PluginEffect::PromptRewrite { .. } => {
                if installed.manifest.runtime != PluginRuntime::StBridge {
                    return Err(PluginError::PromptRewriteDenied);
                }
                if event != PluginEvent::GenerateInterceptor
                    && event != PluginEvent::ChatCompletionPromptReady
                {
                    return Err(PluginError::PromptRewritePhaseDenied);
                }
                PluginCapability::ContributePrompt
            }
            PluginEffect::StateWrite { key, .. } => {
                if key.scope != crate::VariableScope::Local
                    || !key.name.starts_with(&format!("{}.", installed.manifest.id))
                {
                    return Err(PluginError::StateScopeDenied);
                }
                PluginCapability::WriteOwnState
            }
            PluginEffect::Abort { .. } => {
                if event != PluginEvent::PreRequest {
                    return Err(PluginError::AbortPhaseDenied);
                }
                PluginCapability::AbortPreRequest
            }
        };
        if !grant.capabilities.contains(&capability) {
            return Err(PluginError::CapabilityDenied(capability));
        }
    }
    Ok(())
}

fn add_edge<'a>(
    edges: &mut BTreeMap<&'a str, BTreeSet<&'a str>>,
    indegree: &mut BTreeMap<&'a str, usize>,
    source: &'a str,
    target: &'a str,
) {
    if edges.entry(source).or_default().insert(target) {
        *indegree.get_mut(target).expect("known plugin") += 1;
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        })
}

fn safe_child(root: &Path, relative: &str) -> Result<PathBuf, PluginError> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(PluginError::UnsafePath(relative.to_owned()));
    }
    Ok(root.join(path))
}

fn read_directories(root: &Path) -> Result<Vec<PathBuf>, PluginError> {
    let mut paths = fs::read_dir(root)
        .map_err(|source| PluginError::ReadDirectory {
            path: root.to_owned(),
            source,
        })?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|kind| kind.is_dir())
                .map(|_| entry.path())
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin artifact decode failed: {0}")]
    Artifact(#[from] crate::ArtifactError),
    #[error("plugin JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to read plugin file '{path}': {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write plugin file '{path}': {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to read plugin directory '{path}': {source}")]
    ReadDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create plugin directory '{path}': {source}")]
    Create {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to copy plugin file from '{from}' to '{to}': {source}")]
    Copy {
        from: PathBuf,
        to: PathBuf,
        source: std::io::Error,
    },
    #[error("unsupported plugin manifest schema '{0}'")]
    UnsupportedManifest(String),
    #[error("invalid plugin ID '{0}'")]
    InvalidId(String),
    #[error("plugin engine range excludes this engine")]
    EngineVersion,
    #[error("invalid SPDX license expression '{0}'")]
    InvalidLicense(String),
    #[error("failed to remove plugin directory '{path}': {source}")]
    Remove {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unknown Plugin capability '{0}'")]
    UnknownCapability(String),
    #[error("unsafe plugin-relative path '{0}'")]
    UnsafePath(String),
    #[error("plugin component digest does not match its pin")]
    DigestMismatch,
    #[error("plugin component exceeds its size limit")]
    ComponentLimit,
    #[error("plugin input exceeds its size limit")]
    InputLimit,
    #[error("artifact inspection plugin returned {0} outputs; expected exactly one")]
    ArtifactInspectionOutputCount(usize),
    #[error("artifact inspection output is only valid during Artifact inspection")]
    ArtifactInspectionOutputPhaseDenied,
    #[error("plugin output exceeds its size limit")]
    OutputLimit,
    #[error("plugin grant does not match the installed version and digest")]
    GrantPinMismatch,
    #[error("plugin grant exceeds requested capabilities")]
    GrantExceedsRequest,
    #[error("artifact inspection plugins may only return typed output")]
    ArtifactInspectionMutationDenied,
    #[error("Plugin post-commit lifecycle is observational only")]
    PostCommitMutationDenied,
    #[error("plugin is disabled")]
    Disabled,
    #[error("Plugin commands may only observe or write namespaced state")]
    CommandEffectDenied,
    #[error("plugin does not subscribe to {0:?}")]
    EventNotSubscribed(PluginEvent),
    #[error("plugin denied capability {0:?}")]
    CapabilityDenied(PluginCapability),
    #[error("plugin returned undeclared macro '{0}'")]
    UndeclaredMacro(String),
    #[error("plugin returned undeclared command '{0}'")]
    UndeclaredCommand(String),
    #[error("plugin returned contribution to closed prompt slot {0:?}")]
    ClosedPromptSlot(PromptSlot),
    #[error("plugin state writes are limited to local own-namespace state")]
    StateScopeDenied,
    #[error("plugin abort is only valid before the provider request")]
    AbortPhaseDenied,
    #[error("plugin guest failed: {0}")]
    Guest(String),
    #[error("recorded Plugin receipt for '{0}' is invalid")]
    RecordedReceiptInvalid(String),
    #[error("plugin execution timer panicked")]
    TimerPanicked,
    #[error("Wasmtime plugin execution failed: {0}")]
    Wasmtime(anyhow::Error),
    #[error("plugin '{plugin}' requires missing dependency '{dependency}'")]
    MissingDependency { plugin: String, dependency: String },
    #[error("plugin '{plugin}' dependency '{dependency}' has an incompatible version")]
    DependencyVersion { plugin: String, dependency: String },
    #[error("plugin dependency ordering contains a cycle")]
    DependencyCycle,
    #[error("plugin '{0}' requires scripting support but this build has scripting disabled")]
    ScriptingUnavailable(String),
    #[error("plugin script source for '{0}' is not valid UTF-8")]
    ScriptNotUtf8(String),
    #[error("plugin script '{plugin}' does not export the '{hook}' hook")]
    ScriptHookMissing { plugin: String, hook: String },
    #[error("plugin script '{plugin}' failed: {message}")]
    ScriptTrap { plugin: String, message: String },
    #[error("plugin script exceeded its execution step budget")]
    ScriptStepLimit,
    #[error("st-bridge requires a valid Session identity")]
    StBridgeSessionIdentity,
    #[error("st-bridge worker stopped")]
    StBridgeWorkerStopped,
    #[error("st-bridge only supports CHAT_COMPLETION_PROMPT_READY in this build")]
    UnsupportedStBridgeEvent,
    #[error("st-bridge Extension '{plugin}' initialization failed: {message}")]
    StBridgeInitialization { plugin: String, message: String },
    #[error("st-bridge Extension '{plugin}' handler failed: {message}")]
    StBridgeHandler { plugin: String, message: String },
    #[error("st-bridge payload read-back failed: {0}")]
    StBridgePayload(String),
    #[error("st-bridge Extension attempted an unsupported prompt mutation")]
    UnsupportedStBridgeMutation,
    #[error("QuickJS runtime setup failed: {0}")]
    ScriptRuntime(String),
    #[error("invalid plugin manifest: {0}")]
    InvalidManifest(String),
    #[error("st-bridge lifecycle events are observation-only")]
    LifecycleObservationOnly,
    #[error("PromptRewrite effect is only valid for st-bridge runtime")]
    PromptRewriteDenied,
    #[error(
        "PromptRewrite is only valid during generate-interceptor or chat-completion-prompt-ready"
    )]
    PromptRewritePhaseDenied,
    #[error("st-bridge async callback exceeded the microtask bound")]
    StBridgeAsyncTimeout,
    #[error(
        "st-bridge Extension '{plugin}' manifest declares generate_interceptor '{name}' but it was not found"
    )]
    StBridgeInterceptorMissing { plugin: String, name: String },
}
