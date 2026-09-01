use std::{env, time::Duration};

use eventsource_stream::Eventsource;
use futures::StreamExt;
use reqwest::{
    Certificate, Client,
    header::{HeaderMap, HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;

use crate::{
    ContentHash, CredentialError, CredentialResolver, FormatMode, HeaderSetting, PromptPlan,
    ProviderSettings, SystemCredentialStore, canonical_json_hash,
};

const PROVIDER_REQUEST_DOMAIN: &str = "stcli:provider-request:v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ProviderResult {
    pub text: String,
    pub request_hash: ContentHash,
    pub receipt: Value,
    pub events: Vec<ProviderEvent>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "event_type", rename_all = "kebab-case")]
pub enum ProviderEvent {
    Started,
    TextDelta { text: String },
    ReasoningDelta { text: String },
    Usage { usage: Value },
    Completed,
}

pub struct OpenAiProvider {
    client: Client,
    settings: ProviderSettings,
    redactions: Vec<String>,
}

pub fn validate_provider_settings(settings: &ProviderSettings) -> Result<(), ProviderError> {
    let url = reqwest::Url::parse(&settings.base_url)
        .map_err(|error| ProviderError::InvalidUrl(error.to_string()))?;
    if url.scheme() != "https" {
        return Err(ProviderError::HttpsRequired(settings.base_url.clone()));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(ProviderError::UrlUserinfo);
    }
    validate_text_completion_settings(settings)?;
    for (name, setting) in &settings.static_headers {
        let header_name = HeaderName::try_from(name.as_str())
            .map_err(|error| ProviderError::InvalidHeader(error.to_string()))?;
        if matches!(setting, HeaderSetting::Literal(_)) && is_secret_header(&header_name) {
            return Err(ProviderError::SecretHeaderMustUseEnvironment(name.clone()));
        }
    }
    Ok(())
}

pub(crate) fn validate_text_completion_settings(
    settings: &ProviderSettings,
) -> Result<(), ProviderError> {
    if settings.format_mode != FormatMode::TextCompletion {
        return Ok(());
    }
    match settings.completions_path.as_deref() {
        None => return Err(ProviderError::MissingCompletionsPath),
        Some(path) if path.trim().is_empty() => return Err(ProviderError::EmptyCompletionsPath),
        Some(_) => {}
    }
    if settings.instruct_template.is_none() {
        return Err(ProviderError::MissingInstructTemplate);
    }
    if settings.context_formatting.is_none() {
        return Err(ProviderError::MissingContextFormatting);
    }
    Ok(())
}

impl OpenAiProvider {
    pub fn new(settings: ProviderSettings) -> Result<Self, ProviderError> {
        Self::new_with_credential_resolver(settings, &SystemCredentialStore)
    }

    pub fn new_with_credential_resolver(
        settings: ProviderSettings,
        credential_resolver: &impl CredentialResolver,
    ) -> Result<Self, ProviderError> {
        validate_provider_settings(&settings)?;
        let mut redactions = Vec::new();
        let mut headers = HeaderMap::new();
        for (name, setting) in &settings.static_headers {
            let header_name = HeaderName::try_from(name.as_str())
                .map_err(|error| ProviderError::InvalidHeader(error.to_string()))?;
            let value = match setting {
                HeaderSetting::Literal(value) => value.clone(),
                HeaderSetting::Environment(environment_name) => {
                    let value = env::var(environment_name)
                        .map_err(|_| ProviderError::MissingHeaderValue(environment_name.clone()))?;
                    redactions.push(value.clone());
                    value
                }
            };
            let header_value = HeaderValue::try_from(value)
                .map_err(|error| ProviderError::InvalidHeader(error.to_string()))?;
            headers.insert(header_name, header_value);
        }
        let configured_environment = settings
            .api_key_env
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty());
        let environment_secret = configured_environment
            .and_then(|name| env::var(name).ok())
            .filter(|secret| !secret.is_empty());
        let secret = if let Some(secret) = environment_secret {
            Some(secret)
        } else if let Some(key) = settings
            .credential_key
            .as_deref()
            .map(str::trim)
            .filter(|key| !key.is_empty())
        {
            Some(credential_resolver.get(key).map_err(|error| match error {
                CredentialError::Missing => ProviderError::MissingCredential(key.to_owned()),
                CredentialError::Store(error) => ProviderError::CredentialStoreError {
                    key: key.to_owned(),
                    error,
                },
            })?)
        } else {
            if let Some(environment_name) = configured_environment {
                return Err(ProviderError::MissingApiKey(environment_name.to_owned()));
            }
            None
        };
        if let Some(secret) = secret {
            redactions.push(secret.clone());
            redactions.push(format!("Bearer {secret}"));
            let value = HeaderValue::try_from(format!("Bearer {secret}"))
                .map_err(|error| ProviderError::InvalidHeader(error.to_string()))?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
        }

        let mut builder = Client::builder()
            .timeout(Duration::from_secs(settings.timeout_seconds))
            .default_headers(headers);
        if let Some(pem) = &settings.ca_certificate_pem {
            let certificate = Certificate::from_pem(pem.as_bytes())
                .map_err(|error| ProviderError::InvalidCertificate(error.to_string()))?;
            builder = builder.add_root_certificate(certificate);
        }
        let client = builder
            .build()
            .map_err(|error| ProviderError::Client(error.to_string()))?;
        redactions.retain(|value| !value.is_empty());
        redactions.sort_by_key(|value| std::cmp::Reverse(value.len()));
        Ok(Self {
            client,
            settings,
            redactions,
        })
    }

    pub async fn generate_request(
        &self,
        request: &Value,
        mut on_event: impl FnMut(&ProviderEvent),
    ) -> Result<ProviderResult, ProviderError> {
        let request_hash = provider_request_hash(request)?;
        let path = match self.settings.format_mode {
            FormatMode::ChatCompletion => &self.settings.chat_completions_path,
            FormatMode::TextCompletion => self
                .settings
                .completions_path
                .as_deref()
                .expect("validated Text Completion path"),
        };
        let endpoint = format!(
            "{}{}",
            self.settings.base_url.trim_end_matches('/'),
            normalize_path(path)
        );
        let started = ProviderEvent::Started;
        on_event(&started);
        let mut events = vec![started];
        let response = self
            .client
            .post(endpoint)
            .json(&request)
            .send()
            .await
            .map_err(|error| ProviderError::Transport(self.redact(&error.to_string())))?;
        let status = response.status();
        if !status.is_success() {
            let mut body = response.text().await.unwrap_or_default();
            body.truncate(crate::limits::MAX_RESPONSE_BODY_BYTES);
            let body = self.redact(&body);
            return Err(ProviderError::Http {
                status: status.as_u16(),
                body,
            });
        }

        if self.settings.stream {
            let mut chunks = Vec::new();
            let mut text = String::new();
            let mut saw_done = false;
            let mut stream = response.bytes_stream().eventsource();
            while let Some(event) = stream.next().await {
                let event = event
                    .map_err(|error| ProviderError::Stream(self.redact(&error.to_string())))?;
                if event.data == "[DONE]" {
                    saw_done = true;
                    break;
                }
                let data = self.redact(&event.data);
                let chunk =
                    serde_json::from_str::<Value>(&data).map_err(ProviderError::ChunkDecode)?;
                for pointer in [
                    "/choices/0/delta/reasoning",
                    "/choices/0/delta/reasoning_content",
                ] {
                    if let Some(delta) = chunk.pointer(pointer).and_then(Value::as_str) {
                        let provider_event = ProviderEvent::ReasoningDelta {
                            text: delta.to_owned(),
                        };
                        on_event(&provider_event);
                        events.push(provider_event);
                    }
                }
                let text_pointer = match self.settings.format_mode {
                    FormatMode::ChatCompletion => "/choices/0/delta/content",
                    FormatMode::TextCompletion => "/choices/0/text",
                };
                if let Some(delta) = chunk.pointer(text_pointer).and_then(Value::as_str) {
                    text.push_str(delta);
                    if text.len() > crate::limits::MAX_RESPONSE_TEXT_BYTES {
                        return Err(ProviderError::ResponseTooLarge {
                            size: text.len(),
                            limit: crate::limits::MAX_RESPONSE_TEXT_BYTES,
                        });
                    }
                    let provider_event = ProviderEvent::TextDelta {
                        text: delta.to_owned(),
                    };
                    on_event(&provider_event);
                    events.push(provider_event);
                }
                if let Some(usage) = chunk.get("usage").filter(|value| !value.is_null()) {
                    let provider_event = ProviderEvent::Usage {
                        usage: usage.clone(),
                    };
                    on_event(&provider_event);
                    events.push(provider_event);
                }
                chunks.push(chunk);
            }
            if !saw_done {
                return Err(ProviderError::Stream(
                    "stream ended without [DONE]".to_owned(),
                ));
            }
            let completed = ProviderEvent::Completed;
            on_event(&completed);
            events.push(completed);
            Ok(ProviderResult {
                text,
                request_hash,
                receipt: json!({"status": status.as_u16(), "chunks": chunks}),
                events,
            })
        } else {
            let raw_body = response.text().await.map_err(ProviderError::Decode)?;
            if raw_body.len() > crate::limits::MAX_RESPONSE_BODY_BYTES {
                return Err(ProviderError::ResponseTooLarge {
                    size: raw_body.len(),
                    limit: crate::limits::MAX_RESPONSE_BODY_BYTES,
                });
            }
            let body = self.redact(&raw_body);
            let body = serde_json::from_str::<Value>(&body).map_err(ProviderError::ChunkDecode)?;
            let reasoning = [
                "/choices/0/message/reasoning",
                "/choices/0/message/reasoning_content",
            ]
            .into_iter()
            .find_map(|pointer| body.pointer(pointer).and_then(Value::as_str));
            let text_pointer = match self.settings.format_mode {
                FormatMode::ChatCompletion => "/choices/0/message/content",
                FormatMode::TextCompletion => "/choices/0/text",
            };
            let text = match body.pointer(text_pointer).and_then(Value::as_str) {
                Some(content) => content.to_owned(),
                None if reasoning.is_some() => String::new(),
                None => return Err(ProviderError::MissingContent),
            };
            if let Some(reasoning) = reasoning {
                let provider_event = ProviderEvent::ReasoningDelta {
                    text: reasoning.to_owned(),
                };
                on_event(&provider_event);
                events.push(provider_event);
            }
            if let Some(usage) = body.get("usage").filter(|value| !value.is_null()) {
                let provider_event = ProviderEvent::Usage {
                    usage: usage.clone(),
                };
                on_event(&provider_event);
                events.push(provider_event);
            }
            let completed = ProviderEvent::Completed;
            on_event(&completed);
            events.push(completed);
            Ok(ProviderResult {
                text,
                request_hash,
                receipt: json!({"status": status.as_u16(), "body": body}),
                events,
            })
        }
    }

    fn redact(&self, input: &str) -> String {
        self.redactions
            .iter()
            .fold(input.to_owned(), |text, secret| {
                text.replace(secret, "[REDACTED]")
            })
    }
}

pub fn provider_request(
    settings: &ProviderSettings,
    prompt_plan: &PromptPlan,
    generation_settings: &Value,
) -> Result<Value, ProviderError> {
    let mut request = match generation_settings {
        Value::Object(object) => object.clone(),
        _ => return Err(ProviderError::InvalidGenerationSettings),
    };
    request.insert("model".to_owned(), Value::String(settings.model.clone()));
    match settings.format_mode {
        FormatMode::ChatCompletion => {
            request.insert(
                "messages".to_owned(),
                serde_json::to_value(&prompt_plan.messages).map_err(ProviderError::Encode)?,
            );
        }
        FormatMode::TextCompletion => {
            request.insert(
                "prompt".to_owned(),
                Value::String(
                    prompt_plan
                        .text_prompt
                        .clone()
                        .ok_or(ProviderError::MissingTextPrompt)?,
                ),
            );
            let mut stops = match request.remove("stop") {
                None => Vec::new(),
                Some(Value::String(stop)) => vec![stop],
                Some(Value::Array(stops)) => stops
                    .into_iter()
                    .map(|stop| match stop {
                        Value::String(stop) => Ok(stop),
                        _ => Err(ProviderError::InvalidStopSequences),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                Some(_) => return Err(ProviderError::InvalidStopSequences),
            };
            for stop in &prompt_plan.stop_sequences {
                if !stop.is_empty() && !stops.contains(stop) {
                    stops.push(stop.clone());
                }
            }
            if !stops.is_empty() {
                request.insert(
                    "stop".to_owned(),
                    Value::Array(stops.into_iter().map(Value::String).collect()),
                );
            }
        }
    }
    request.insert("stream".to_owned(), Value::Bool(settings.stream));
    Ok(Value::Object(request))
}

pub fn provider_request_hash(request: &Value) -> Result<ContentHash, ProviderError> {
    canonical_json_hash(PROVIDER_REQUEST_DOMAIN, request).map_err(ProviderError::Canonicalize)
}

fn is_secret_header(name: &HeaderName) -> bool {
    matches!(
        name.as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-auth-token"
    )
}

fn normalize_path(path: &str) -> String {
    if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    }
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("provider URL is invalid: {0}")]
    InvalidUrl(String),
    #[error("provider URL must not contain username or password userinfo")]
    UrlUserinfo,
    #[error("provider URL must use HTTPS, found '{0}'")]
    HttpsRequired(String),
    #[error("provider API key environment variable '{0}' is not set")]
    MissingApiKey(String),
    #[error(
        "Credential Store entry '{0}' is not configured; run `stcli credentials set {0}` or specify `api_key_env`"
    )]
    MissingCredential(String),
    #[error(
        "Credential Store entry '{key}' could not be accessed: {error}. Ensure your system keyring/Secret Service is unlocked, or specify `api_key_env` to use an environment variable instead."
    )]
    CredentialStoreError { key: String, error: String },
    #[error("provider header environment variable '{0}' is not set")]
    MissingHeaderValue(String),
    #[error("secret-valued provider header '{0}' must use an environment reference")]
    SecretHeaderMustUseEnvironment(String),
    #[error("provider header is invalid: {0}")]
    InvalidHeader(String),
    #[error("provider CA certificate is invalid: {0}")]
    InvalidCertificate(String),
    #[error("failed to create provider client: {0}")]
    Client(String),
    #[error("provider transport failed: {0}")]
    Transport(String),
    #[error("provider returned HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("provider stream failed: {0}")]
    Stream(String),
    #[error("provider JSON decode failed: {0}")]
    Decode(reqwest::Error),
    #[error("provider SSE chunk JSON decode failed: {0}")]
    ChunkDecode(serde_json::Error),
    #[error("provider JSON encode failed: {0}")]
    Encode(serde_json::Error),
    #[error("provider request canonicalization failed: {0}")]
    Canonicalize(serde_json::Error),
    #[error("Text Completion provider is missing completions_path")]
    MissingCompletionsPath,
    #[error("Text Completion provider completions_path must not be empty")]
    EmptyCompletionsPath,
    #[error("Text Completion provider is missing instruct_template")]
    MissingInstructTemplate,
    #[error("Text Completion provider is missing context_formatting")]
    MissingContextFormatting,
    #[error("generation settings must be a JSON object")]
    InvalidGenerationSettings,
    #[error("provider response did not contain completion text")]
    MissingContent,
    #[error("Text Completion Prompt Plan is missing text_prompt")]
    MissingTextPrompt,
    #[error("Text Completion stop must be a string or an array of strings")]
    InvalidStopSequences,
    #[error("provider response exceeds {limit} byte limit ({size} bytes)")]
    ResponseTooLarge { size: usize, limit: usize },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credential::{CredentialError, CredentialResolver};
    use stcli_testkit::EnvironmentGuard;
    use std::collections::BTreeMap;

    struct FakeCredentialResolver {
        result: Result<String, CredentialError>,
    }

    impl CredentialResolver for FakeCredentialResolver {
        fn get(&self, _key: &str) -> Result<String, CredentialError> {
            self.result.clone()
        }
    }

    fn settings() -> ProviderSettings {
        ProviderSettings {
            id: "fixture".to_owned(),
            base_url: "https://example.invalid".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            credential_key: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 1,
            ca_certificate_pem: None,
            model: "fixture-model".to_owned(),
            stream: true,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        }
    }

    #[test]
    fn http_provider_is_rejected() {
        let mut settings = settings();
        settings.base_url = "http://localhost".to_owned();
        assert!(matches!(
            OpenAiProvider::new(settings),
            Err(ProviderError::HttpsRequired(_))
        ));
    }

    #[test]
    fn literal_authorization_header_is_rejected() {
        let mut settings = settings();
        settings.static_headers.insert(
            "authorization".to_owned(),
            HeaderSetting::Literal("Bearer secret".to_owned()),
        );
        assert!(matches!(
            OpenAiProvider::new(settings),
            Err(ProviderError::SecretHeaderMustUseEnvironment(_))
        ));
    }

    #[test]
    fn empty_environment_api_key_falls_back_to_credential_store() {
        let mut environment = EnvironmentGuard::new();
        environment.set("STCLI_PROVIDER_EMPTY_KEY", "");
        let mut settings = settings();
        settings.api_key_env = Some("STCLI_PROVIDER_EMPTY_KEY".to_owned());
        settings.credential_key = Some("openrouter".to_owned());
        let resolver = FakeCredentialResolver {
            result: Ok("stored-secret".to_owned()),
        };

        let provider = OpenAiProvider::new_with_credential_resolver(settings, &resolver).unwrap();

        assert!(provider.redactions.contains(&"stored-secret".to_owned()));
    }

    #[test]
    fn credential_store_secret_is_applied_and_redacted() {
        let mut settings = settings();
        settings.credential_key = Some("openrouter".to_owned());
        let resolver = FakeCredentialResolver {
            result: Ok("stored-secret".to_owned()),
        };

        let provider = OpenAiProvider::new_with_credential_resolver(settings, &resolver).unwrap();

        assert!(provider.redactions.contains(&"stored-secret".to_owned()));
        assert!(
            provider
                .redactions
                .contains(&"Bearer stored-secret".to_owned())
        );
    }

    #[test]
    fn environment_api_key_takes_precedence_over_credential_store() {
        let mut environment = EnvironmentGuard::new();
        environment.set("STCLI_PROVIDER_TEST_KEY", "environment-secret");
        let mut settings = settings();
        settings.api_key_env = Some("STCLI_PROVIDER_TEST_KEY".to_owned());
        settings.credential_key = Some("openrouter".to_owned());
        let resolver = FakeCredentialResolver {
            result: Err(CredentialError::Store("must not be read".to_owned())),
        };

        let provider = OpenAiProvider::new_with_credential_resolver(settings, &resolver).unwrap();

        assert!(
            provider
                .redactions
                .contains(&"environment-secret".to_owned())
        );
        assert!(
            !provider
                .redactions
                .iter()
                .any(|value| value == "stored-secret")
        );
    }

    #[test]
    fn credential_failures_are_classified_for_provider_diagnostics() {
        let mut settings = settings();
        settings.credential_key = Some("missing".to_owned());
        let missing = FakeCredentialResolver {
            result: Err(CredentialError::Missing),
        };
        assert!(matches!(
            OpenAiProvider::new_with_credential_resolver(settings.clone(), &missing),
            Err(ProviderError::MissingCredential(key)) if key == "missing"
        ));

        let unavailable = FakeCredentialResolver {
            result: Err(CredentialError::Store("locked".to_owned())),
        };
        assert!(matches!(
            OpenAiProvider::new_with_credential_resolver(settings, &unavailable),
            Err(ProviderError::CredentialStoreError { key, error })
                if key == "missing" && error == "locked"
        ));
    }
}
