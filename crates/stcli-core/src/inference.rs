//! Brokered Secondary Inference for Plugins and compatibility runtimes.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    Config, ContentHash, OpenAiProvider, ProviderSettings, canonical_json_hash, content_blob_hash,
    provider::secondary_provider_request,
};

pub const INFERENCE_REQUEST_DOMAIN: &str = "stcli:secondary-inference-request:v1";
const TRANSPORT_ERROR: &str = "secondary inference transport failed";

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InferenceRequest {
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
    TransportFailed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct InferenceReceipt {
    pub profile_name: String,
    pub prompt: String,
    pub effective_settings: Value,
    pub request_hash: ContentHash,
    pub status: InferenceStatus,
    pub response_hash: ContentHash,
    pub text: String,
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
    ) -> Result<String, InferenceTransportError>;
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
    ) -> Result<String, InferenceTransportError> {
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
            .map(|result| result.text)
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
        _request: &Value,
    ) -> Result<String, InferenceTransportError> {
        Ok(self
            .responses
            .get(&settings.id)
            .cloned()
            .unwrap_or_default())
    }
}

#[derive(Clone)]
pub struct InferenceBroker {
    config: Arc<Config>,
    transport: Arc<dyn InferenceTransport>,
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
}

impl InferenceBroker {
    pub fn live(config: Config) -> Self {
        Self {
            config: Arc::new(config),
            transport: Arc::new(ProviderInferenceTransport::default()),
            stubbed: false,
        }
    }

    pub fn stub(config: Config, transport: Arc<dyn InferenceTransport>) -> Self {
        Self {
            config: Arc::new(config),
            transport,
            stubbed: true,
        }
    }

    pub fn infer(
        &self,
        caller: &str,
        policy: &InferencePolicy,
        request: &InferenceRequest,
    ) -> Result<InferenceResponse, InferenceError> {
        if !policy.capability_granted {
            return Err(InferenceError::Denied(format!(
                "secondary inference denied: plugin '{caller}' lacks the secondary-inference capability"
            )));
        }
        if !request.generation_settings.is_object() {
            return Err(InferenceError::InvalidGenerationSettings);
        }
        let settings = self
            .config
            .resolve_provider_profile(&request.profile_name)
            .map_err(|error| InferenceError::Profile(error.to_string()))?;
        let provider_request =
            secondary_provider_request(settings, &request.prompt, &request.generation_settings)
                .map_err(|error| InferenceError::Provider(error.to_string()))?;
        let effective_settings = effective_settings(&provider_request);
        let request_hash =
            inference_request_hash(&request.profile_name, &request.prompt, &effective_settings)
                .map_err(InferenceError::Canonicalize)?;
        let result = if policy.mode == InferenceMode::DryRun && !self.stubbed {
            Ok(String::new())
        } else {
            self.transport.generate(settings, &provider_request)
        };
        let (text, status, error, response_hash) = match result {
            Ok(text) => {
                let response_hash = content_blob_hash(text.as_bytes());
                (text, InferenceStatus::Completed, None, response_hash)
            }
            Err(_) => (
                String::new(),
                InferenceStatus::TransportFailed,
                Some(TRANSPORT_ERROR.to_owned()),
                content_blob_hash(TRANSPORT_ERROR.as_bytes()),
            ),
        };
        let receipt = InferenceReceipt {
            profile_name: request.profile_name.clone(),
            prompt: request.prompt.clone(),
            effective_settings,
            request_hash,
            status,
            response_hash,
            text: text.clone(),
            error,
        };
        Ok(InferenceResponse { text, receipt })
    }
}

pub fn validate_inference_receipt(receipt: &InferenceReceipt) -> Result<(), InferenceError> {
    let request_hash = inference_request_hash(
        &receipt.profile_name,
        &receipt.prompt,
        &receipt.effective_settings,
    )
    .map_err(InferenceError::Canonicalize)?;
    let response_hash = match receipt.status {
        InferenceStatus::Completed if receipt.error.is_none() => {
            content_blob_hash(receipt.text.as_bytes())
        }
        InferenceStatus::TransportFailed
            if receipt.text.is_empty() && receipt.error.as_deref() == Some(TRANSPORT_ERROR) =>
        {
            content_blob_hash(TRANSPORT_ERROR.as_bytes())
        }
        _ => return Err(InferenceError::InvalidReceipt),
    };
    if receipt.request_hash != request_hash || receipt.response_hash != response_hash {
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
    prompt: &str,
    effective_settings: &Value,
) -> Result<ContentHash, serde_json::Error> {
    canonical_json_hash(
        INFERENCE_REQUEST_DOMAIN,
        &json!({
            "profile_name": profile_name,
            "prompt": prompt,
            "effective_settings": effective_settings,
        }),
    )
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
        let response = broker
            .infer(
                "fixture",
                &InferencePolicy {
                    capability_granted: true,
                    mode: InferenceMode::DryRun,
                },
                &InferenceRequest {
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
        assert!(matches!(
            broker.infer(
                "fixture",
                &InferencePolicy {
                    capability_granted: false,
                    mode: InferenceMode::Live
                },
                &InferenceRequest {
                    prompt: "x".to_owned(),
                    profile_name: "summary".to_owned(),
                    generation_settings: json!({}),
                }
            ),
            Err(InferenceError::Denied(_))
        ));
    }
}
