//! Brokered Secondary Inference for Plugins and compatibility runtimes.

use std::{
    collections::BTreeMap,
    fmt,
    path::PathBuf,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    Config, ContentHash, EntityId, OpenAiProvider, ProviderResult, ProviderSettings, Store,
    canonical_json_hash, content_blob_hash,
    provider::{provider_request_hash, secondary_provider_request},
};

pub const INFERENCE_REQUEST_DOMAIN: &str = "stcli:secondary-inference-request:v1";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InferenceRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub prompt: String,
    pub profile_name: String,
    pub generation_settings: Value,
}

#[derive(Clone, Debug)]
pub struct InferencePolicy {
    pub capability_granted: bool,
    pub mode: InferenceMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InferenceMode {
    Live,
    DryRun,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum InferenceStatus {
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InferenceReceipt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt_id: Option<EntityId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_attempt_id: Option<EntityId>,
    pub caller: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system_prompt: Option<String>,
    pub profile_name: String,
    pub prompt: String,
    pub effective_settings: Value,
    pub request_hash: ContentHash,
    pub status: InferenceStatus,
    pub response_hash: ContentHash,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct InferenceResponse {
    pub text: String,
    pub receipt: InferenceReceipt,
}

#[derive(Debug, Error)]
#[error("secondary inference transport failed: {0}")]
pub struct InferenceTransportError(pub String);

pub trait InferenceTransport: Send + Sync {
    fn generate(
        &self,
        settings: &ProviderSettings,
        request: &Value,
    ) -> Result<ProviderResult, InferenceTransportError>;
}

#[derive(Default)]
pub struct ProviderInferenceTransport {
    runtime: OnceLock<tokio::runtime::Runtime>,
}

impl InferenceTransport for ProviderInferenceTransport {
    fn generate(
        &self,
        settings: &ProviderSettings,
        request: &Value,
    ) -> Result<ProviderResult, InferenceTransportError> {
        if self.runtime.get().is_none() {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| InferenceTransportError(error.to_string()))?;
            let _ = self.runtime.set(runtime);
        }
        let provider = OpenAiProvider::new(settings.clone())
            .map_err(|error| InferenceTransportError(error.to_string()))?;
        self.runtime
            .get()
            .expect("secondary inference runtime was initialized")
            .block_on(provider.generate_request(request, |_| {}))
            .map_err(|error| InferenceTransportError(error.to_string()))
    }
}

#[derive(Debug, Default)]
pub struct StubInferenceTransport {
    pub responses: BTreeMap<String, String>,
}

impl InferenceTransport for StubInferenceTransport {
    fn generate(
        &self,
        settings: &ProviderSettings,
        request: &Value,
    ) -> Result<ProviderResult, InferenceTransportError> {
        Ok(ProviderResult {
            text: self
                .responses
                .get(&settings.id)
                .cloned()
                .unwrap_or_default(),
            request_hash: provider_request_hash(request)
                .map_err(|error| InferenceTransportError(error.to_string()))?,
            receipt: Value::Null,
            events: Vec::new(),
        })
    }
}

#[derive(Clone)]
pub struct InferenceBroker {
    config: Arc<Config>,
    transport: Arc<dyn InferenceTransport>,
    database: Option<PathBuf>,
    stubbed: bool,
}

impl fmt::Debug for InferenceBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InferenceBroker")
            .field(
                "profiles",
                &self.config.providers.keys().collect::<Vec<_>>(),
            )
            .field("stubbed", &self.stubbed)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug)]
pub struct InferenceInvocation {
    pub broker: InferenceBroker,
    pub policy: InferencePolicy,
    pub default_profile: String,
    pub session_id: Option<EntityId>,
    pub branch_id: Option<EntityId>,
    pub parent_attempt_id: Option<EntityId>,
    pub config_hash: Option<ContentHash>,
    pub caller: String,
}

impl InferenceBroker {
    pub fn live(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            transport: Arc::new(ProviderInferenceTransport::default()),
            database: None,
            stubbed: false,
        }
    }

    pub fn stub(config: Config, transport: Arc<dyn InferenceTransport>) -> Self {
        Self {
            config: Arc::new(config),
            transport,
            database: None,
            stubbed: true,
        }
    }

    pub(crate) fn with_database(mut self, database: PathBuf) -> Self {
        self.database = Some(database);
        self
    }

    pub fn infer(
        &self,
        invocation: &InferenceInvocation,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        if !invocation.policy.capability_granted {
            return Err(InferenceError::Denied(format!(
                "secondary inference denied: plugin '{}' lacks the secondary-inference capability",
                invocation.caller
            )));
        }
        if !request.generation_settings.is_object() {
            return Err(InferenceError::InvalidGenerationSettings);
        }
        let profile_name = if request.profile_name.is_empty() {
            &invocation.default_profile
        } else {
            &request.profile_name
        };
        let settings = self
            .config
            .resolve_provider_profile(profile_name)
            .map_err(|error| InferenceError::Profile(error.to_string()))?;
        let provider_request = secondary_provider_request(
            settings,
            request.system_prompt.as_deref(),
            &request.prompt,
            &request.generation_settings,
        )
        .map_err(|error| InferenceError::Provider(error.to_string()))?;
        let effective_settings = effective_settings(&provider_request);
        let request_hash = inference_request_hash(
            profile_name,
            request.system_prompt.as_deref(),
            &request.prompt,
            &effective_settings,
        )
        .map_err(InferenceError::Canonicalize)?;
        let live = invocation.policy.mode == InferenceMode::Live;
        let mut persisted = if live {
            let database = self
                .database
                .as_ref()
                .ok_or(InferenceError::MissingInvocationMetadata)?;
            let session_id = invocation
                .session_id
                .ok_or(InferenceError::MissingInvocationMetadata)?;
            let branch_id = invocation
                .branch_id
                .ok_or(InferenceError::MissingInvocationMetadata)?;
            let parent_attempt_id = invocation
                .parent_attempt_id
                .ok_or(InferenceError::MissingInvocationMetadata)?;
            let config_hash = invocation
                .config_hash
                .clone()
                .ok_or(InferenceError::MissingInvocationMetadata)?;
            let mut store = Store::open(database).map_err(InferenceError::Store)?;
            let attempt_id = store
                .begin_background_attempt(
                    session_id,
                    branch_id,
                    parent_attempt_id,
                    &invocation.caller,
                    config_hash,
                    profile_name,
                    &effective_settings,
                    &request_hash,
                )
                .map_err(InferenceError::Attempt)?;
            Some((store, attempt_id))
        } else {
            None
        };
        let attempt_id = persisted.as_ref().map(|(_, attempt_id)| *attempt_id);
        let result = if !live && !self.stubbed {
            Ok(ProviderResult {
                text: String::new(),
                request_hash: provider_request_hash(&provider_request)
                    .map_err(|error| InferenceError::Provider(error.to_string()))?,
                receipt: Value::Null,
                events: Vec::new(),
            })
        } else {
            self.transport.generate(settings, &provider_request)
        };
        let result = match result {
            Ok(result) => result,
            Err(error) => {
                if let Some((store, attempt_id)) = persisted.as_mut() {
                    store
                        .fail_background_attempt(*attempt_id, &error.to_string())
                        .map_err(InferenceError::Attempt)?;
                }
                return Err(InferenceError::Transport(error.0));
            }
        };
        let response_hash = content_blob_hash(result.text.as_bytes());
        if let Some((store, attempt_id)) = persisted.as_mut() {
            store
                .complete_background_attempt(*attempt_id, &result, &response_hash)
                .map_err(InferenceError::Attempt)?;
        }
        let receipt = InferenceReceipt {
            attempt_id,
            parent_attempt_id: invocation.parent_attempt_id,
            caller: invocation.caller.clone(),
            system_prompt: request.system_prompt.clone(),
            profile_name: profile_name.to_owned(),
            prompt: request.prompt.clone(),
            effective_settings,
            request_hash,
            status: InferenceStatus::Completed,
            response_hash,
            text: result.text.clone(),
            usage: result.events.iter().rev().find_map(|event| match event {
                crate::ProviderEvent::Usage { usage } => Some(usage.clone()),
                _ => None,
            }),
            error: None,
        };
        Ok(InferenceResponse {
            text: result.text,
            receipt,
        })
    }
}

pub fn validate_inference_receipt(receipt: &InferenceReceipt) -> Result<(), InferenceError> {
    let request_hash = inference_request_hash(
        &receipt.profile_name,
        receipt.system_prompt.as_deref(),
        &receipt.prompt,
        &receipt.effective_settings,
    )
    .map_err(InferenceError::Canonicalize)?;
    let response_hash = match receipt.status {
        InferenceStatus::Completed if receipt.error.is_none() => {
            content_blob_hash(receipt.text.as_bytes())
        }
        _ => return Err(InferenceError::InvalidReceipt),
    };
    if receipt.request_hash != request_hash || receipt.response_hash != response_hash {
        return Err(InferenceError::InvalidReceipt);
    }
    Ok(())
}

pub fn validate_persisted_inference_receipt(
    store: &Store,
    receipt: &InferenceReceipt,
) -> Result<(), InferenceError> {
    validate_inference_receipt(receipt)?;
    let attempt_id = receipt.attempt_id.ok_or(InferenceError::InvalidReceipt)?;
    let attempt = store
        .attempt(attempt_id)
        .map_err(InferenceError::Attempt)?
        .ok_or(InferenceError::InvalidReceipt)?;
    if attempt.kind != crate::AttemptKind::Background
        || attempt.parent_attempt_id != receipt.parent_attempt_id
        || attempt.caller.as_deref() != Some(receipt.caller.as_str())
        || attempt.provider_profile.as_deref() != Some(receipt.profile_name.as_str())
        || attempt.effective_generation_settings.as_ref() != Some(&receipt.effective_settings)
        || attempt.provider_request_hash.as_ref() != Some(&receipt.request_hash)
        || attempt.response_hash.as_ref() != Some(&receipt.response_hash)
        || attempt.usage != receipt.usage
        || attempt.status != crate::AttemptStatus::Completed
    {
        return Err(InferenceError::InvalidReceipt);
    }
    Ok(())
}

fn effective_settings(request: &Value) -> Value {
    let mut settings = request.as_object().cloned().unwrap_or_default();
    settings.remove("messages");
    settings.remove("prompt");
    Value::Object(settings)
}

fn inference_request_hash(
    profile_name: &str,
    system_prompt: Option<&str>,
    prompt: &str,
    effective_settings: &Value,
) -> Result<ContentHash, serde_json::Error> {
    let mut request = serde_json::Map::from_iter([
        ("profile_name".to_owned(), json!(profile_name)),
        ("prompt".to_owned(), json!(prompt)),
        ("effective_settings".to_owned(), effective_settings.clone()),
    ]);
    if let Some(system_prompt) = system_prompt {
        request.insert("system_prompt".to_owned(), json!(system_prompt));
    }
    canonical_json_hash(INFERENCE_REQUEST_DOMAIN, &Value::Object(request))
}

#[derive(Debug, Error)]
pub enum InferenceError {
    #[error("{0}")]
    Denied(String),
    #[error("secondary inference generation settings must be a JSON object")]
    InvalidGenerationSettings,
    #[error("secondary inference provider profile resolution failed: {0}")]
    Profile(String),
    #[error("secondary inference provider request failed: {0}")]
    Provider(String),
    #[error("secondary inference request canonicalization failed: {0}")]
    Canonicalize(serde_json::Error),
    #[error("live secondary inference requires persisted invocation metadata")]
    MissingInvocationMetadata,
    #[error("secondary inference persistence failed: {0}")]
    Store(crate::StorageError),
    #[error("secondary inference attempt failed: {0}")]
    Attempt(crate::TurnError),
    #[error("secondary inference transport failed: {0}")]
    Transport(String),
    #[error("recorded Secondary Inference receipt is invalid")]
    InvalidReceipt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn config() -> Config {
        Config {
            providers: BTreeMap::from([(
                "summary".to_owned(),
                ProviderSettings {
                    id: "summary".to_owned(),
                    base_url: "https://example.invalid".to_owned(),
                    chat_completions_path: "/v1/chat/completions".to_owned(),
                    format_mode: Default::default(),
                    completions_path: None,
                    instruct_template: None,
                    context_formatting: None,
                    api_key_env: None,
                    credential_key: None,
                    static_headers: BTreeMap::new(),
                    timeout_seconds: 30,
                    ca_certificate_pem: None,
                    model: "stub".to_owned(),
                    stream: false,
                },
            )]),
            enabled_extensions: BTreeMap::new(),
        }
    }

    #[test]
    fn secondary_inference_stub_records_and_validates_hashes() {
        let broker = InferenceBroker::stub(
            config(),
            Arc::new(StubInferenceTransport {
                responses: BTreeMap::from([("summary".to_owned(), "Summary text".to_owned())]),
            }),
        );
        let invocation = InferenceInvocation {
            broker: broker.clone(),
            policy: InferencePolicy {
                capability_granted: true,
                mode: InferenceMode::DryRun,
            },
            default_profile: "summary".to_owned(),
            session_id: None,
            branch_id: None,
            parent_attempt_id: None,
            config_hash: None,
            caller: "fixture".to_owned(),
        };
        let response = broker
            .infer(
                &invocation,
                &InferenceRequest {
                    system_prompt: None,
                    prompt: "Summarize this".to_owned(),
                    profile_name: "summary".to_owned(),
                    generation_settings: json!({"temperature": 0.2}),
                },
            )
            .unwrap();
        assert_eq!(response.text, "Summary text");
        validate_inference_receipt(&response.receipt).unwrap();
        assert!(!response.receipt.request_hash.to_string().is_empty());
        assert_eq!(
            response.receipt.response_hash,
            content_blob_hash(b"Summary text")
        );
    }

    #[test]
    fn secondary_inference_denies_without_capability() {
        let broker = InferenceBroker::stub(config(), Arc::new(StubInferenceTransport::default()));
        let invocation = InferenceInvocation {
            broker: broker.clone(),
            policy: InferencePolicy {
                capability_granted: false,
                mode: InferenceMode::Live,
            },
            default_profile: "summary".to_owned(),
            session_id: None,
            branch_id: None,
            parent_attempt_id: None,
            config_hash: None,
            caller: "fixture".to_owned(),
        };
        assert!(matches!(
            broker.infer(
                &invocation,
                &InferenceRequest {
                    system_prompt: None,
                    prompt: "x".to_owned(),
                    profile_name: "summary".to_owned(),
                    generation_settings: json!({}),
                }
            ),
            Err(InferenceError::Denied(_))
        ));
    }

    #[test]
    fn cancelling_blocked_background_attempt_does_not_mutate_completed_parent() {
        // Regression test for GH-25: background cancellation is independently addressable.
        use parking_lot::Mutex;
        use std::sync::mpsc;

        struct BlockingTransport {
            started: mpsc::Sender<()>,
            release: Mutex<mpsc::Receiver<()>>,
        }
        impl InferenceTransport for BlockingTransport {
            fn generate(
                &self,
                _settings: &ProviderSettings,
                request: &Value,
            ) -> Result<ProviderResult, InferenceTransportError> {
                self.started.send(()).unwrap();
                self.release.lock().recv().unwrap();
                Ok(ProviderResult {
                    text: "too late".to_owned(),
                    request_hash: provider_request_hash(request)
                        .map_err(|error| InferenceTransportError(error.to_string()))?,
                    receipt: Value::Null,
                    events: Vec::new(),
                })
            }
        }

        let directory = tempfile::tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let session_id = EntityId::new();
        let branch_id = EntityId::new();
        let turn_id = EntityId::new();
        let parent_attempt_id = EntityId::new();
        let config_hash = format!("sha256:{}", "0".repeat(64))
            .parse::<ContentHash>()
            .unwrap();
        store
            .connection
            .execute_batch(&format!(
                "PRAGMA foreign_keys = OFF;
                 INSERT INTO session_config_revisions(revision_hash, body, created_event_id)
                 VALUES ('{config_hash}', x'7b7d', '{config_event}');
                 INSERT INTO sessions(session_id, current_config_hash, root_branch_id, archived, created_event_id)
                 VALUES ('{session_id}', '{config_hash}', '{branch_id}', 0, '{session_event}');
                 INSERT INTO branches(branch_id, session_id, greeting_revision_hash, greeting_index, created_event_id)
                 VALUES ('{branch_id}', '{session_id}', '{config_hash}', 0, '{branch_event}');
                 INSERT INTO turns(turn_id, session_id, branch_id, user_content, created_event_id)
                 VALUES ('{turn_id}', '{session_id}', '{branch_id}', 'parent', '{turn_event}');
                 INSERT INTO attempts(attempt_id, session_id, branch_id, kind, turn_id, config_hash, status, prompt_plan, created_event_id, completed_event_id)
                 VALUES ('{parent_attempt_id}', '{session_id}', '{branch_id}', 'primary', '{turn_id}', '{config_hash}', 'completed', NULL, '{attempt_event}', '{attempt_event}');
                 PRAGMA foreign_keys = ON;",
                config_event = EntityId::new(),
                session_event = EntityId::new(),
                branch_event = EntityId::new(),
                turn_event = EntityId::new(),
                attempt_event = EntityId::new(),
            ))
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let broker = InferenceBroker::stub(
            config(),
            Arc::new(BlockingTransport {
                started: started_tx,
                release: Mutex::new(release_rx),
            }),
        )
        .with_database(database);
        let invocation = InferenceInvocation {
            broker: broker.clone(),
            policy: InferencePolicy {
                capability_granted: true,
                mode: InferenceMode::Live,
            },
            default_profile: "summary".to_owned(),
            session_id: Some(session_id),
            branch_id: Some(branch_id),
            parent_attempt_id: Some(parent_attempt_id),
            config_hash: Some(config_hash),
            caller: "org.stcli.blocking-proof".to_owned(),
        };
        let worker = std::thread::spawn(move || {
            broker.infer(
                &invocation,
                &InferenceRequest {
                    system_prompt: None,
                    prompt: "block".to_owned(),
                    profile_name: "summary".to_owned(),
                    generation_settings: json!({}),
                },
            )
        });
        started_rx.recv().unwrap();
        let background = store
            .background_attempts(session_id, Some(branch_id))
            .unwrap();
        assert_eq!(background.len(), 1);
        let child_id = background[0].attempt_id;
        store.cancel_attempt(child_id).unwrap();
        assert_eq!(
            store.attempt(parent_attempt_id).unwrap().unwrap().status,
            crate::AttemptStatus::Completed
        );
        release_tx.send(()).unwrap();
        assert!(matches!(
            worker.join().unwrap(),
            Err(InferenceError::Attempt(crate::TurnError::AttemptNotRunning {
                attempt_id,
                status: crate::AttemptStatus::Cancelled,
            })) if attempt_id == child_id
        ));
        assert_eq!(
            store.attempt(child_id).unwrap().unwrap().status,
            crate::AttemptStatus::Cancelled
        );
        assert!(store.candidates_for_turn(turn_id).unwrap().is_empty());
    }
}
