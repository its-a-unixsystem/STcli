//! Brokered HTTPS egress for Plugins.
//!
//! ## Entry points
//!
//! The [`EgressBroker`] is the single host-controlled boundary for outbound
//! HTTPS from Plugin runtimes. Callers never open sockets: `st-bridge`
//! Extensions call `fetch`/`$.ajax`, which route through the broker's
//! [`EgressBroker::fetch`]. Native Plugin hosts reuse the same trait pair
//! ([`EgressTransport`] for the wire, [`crate::CredentialResolver`] for
//! secrets).
//!
//! ## Deny by default
//!
//! Egress requires the `brokered-egress` capability, an `https` URL, and a
//! host that exactly matches an allowance in the caller's egress allow-list.
//! Every denial resolves non-fatally (`ok: false`, `status: 0`) and records
//! one `warn` script log.
//!
//! ## Secrets
//!
//! An [`EgressAllowance`] may carry an [`EgressSecretInjection`]: the broker
//! resolves the Credential Reference host-side and injects the header after
//! the Plugin runtime has handed over the request, so secret values never
//! enter Plugin memory, receipts, or hashes. [`EgressReceipt::request_hash`]
//! covers method, URL, body, and injected header *names* only.
//!
//! ## Receipts
//!
//! Every exchange that reaches the transport records a content-addressed
//! [`EgressReceipt`] (request hash from `stcli:egress-request:v1`, response
//! hash from the content-blob domain). Receipts ride the Plugin receipt into
//! the Turn Trace; Replay re-applies the recorded result offline without
//! re-executing Plugin JavaScript.
//!
//! ## Dry Runs
//!
//! In [`EgressMode::DryRun`] a non-stubbed broker answers a canned empty `200`
//! without touching the transport; a stubbed broker forwards to the stub so
//! Dry Runs can exercise egress offline.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, OnceLock},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

use crate::{
    ContentHash, CredentialResolver, ScriptLog, SystemCredentialStore, canonical_json_hash,
    content_blob_hash,
};

pub const EGRESS_REQUEST_DOMAIN: &str = "stcli:egress-request:v1";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const DENIED_STATUS_TEXT: &str = "egress denied";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EgressSecretInjection {
    pub credential_key: String,
    pub header: String,
    pub value_template: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EgressAllowance {
    pub domain: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<EgressSecretInjection>,
}

#[derive(Clone, Debug, Default)]
pub struct EgressPolicy {
    pub capability_granted: bool,
    pub allowances: Vec<EgressAllowance>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EgressMode {
    Live,
    DryRun,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EgressRequest {
    pub url: String,
    pub method: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct EgressResponse {
    pub status: u16,
    pub status_text: String,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct EgressReceipt {
    pub url: String,
    pub method: String,
    pub request_hash: ContentHash,
    pub status: u16,
    pub response_hash: ContentHash,
    pub body: String,
}

#[derive(Debug, Error)]
#[error("egress transport failed: {0}")]
pub struct EgressTransportError(pub String);

pub trait EgressTransport: Send + Sync {
    fn roundtrip(&self, request: &EgressRequest) -> Result<EgressResponse, EgressTransportError>;
}

pub struct ReqwestTransport {
    client: OnceLock<reqwest::blocking::Client>,
}

impl ReqwestTransport {
    pub fn new() -> Self {
        Self {
            client: OnceLock::new(),
        }
    }
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressTransport for ReqwestTransport {
    fn roundtrip(&self, request: &EgressRequest) -> Result<EgressResponse, EgressTransportError> {
        let client = if let Some(client) = self.client.get() {
            client
        } else {
            let built = reqwest::blocking::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .map_err(|error| EgressTransportError(error.to_string()))?;
            self.client.get_or_init(move || built)
        };
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| EgressTransportError(error.to_string()))?;
        let mut call = client.request(method, &request.url);
        for (name, value) in &request.headers {
            call = call.header(name, value);
        }
        if let Some(body) = &request.body {
            call = call.body(body.clone());
        }
        let response = call
            .send()
            .map_err(|error| EgressTransportError(error.to_string()))?;
        let status = response.status().as_u16();
        let status_text = response
            .status()
            .canonical_reason()
            .unwrap_or_default()
            .to_owned();
        let mut headers = BTreeMap::new();
        for (name, value) in response.headers() {
            let value = value
                .to_str()
                .map_err(|error| EgressTransportError(error.to_string()))?;
            let entry: &mut String = headers.entry(name.as_str().to_owned()).or_default();
            if entry.is_empty() {
                entry.push_str(value);
            } else {
                entry.push_str(", ");
                entry.push_str(value);
            }
        }
        let body = response
            .text()
            .map_err(|error| EgressTransportError(error.to_string()))?;
        Ok(EgressResponse {
            status,
            status_text,
            headers,
            body,
        })
    }
}

#[derive(Debug, Default)]
pub struct StubTransport {
    pub responses: BTreeMap<String, EgressResponse>,
}

impl EgressTransport for StubTransport {
    fn roundtrip(&self, request: &EgressRequest) -> Result<EgressResponse, EgressTransportError> {
        Ok(self
            .responses
            .get(&request.url)
            .cloned()
            .unwrap_or(EgressResponse {
                status: 200,
                status_text: "OK".to_owned(),
                headers: BTreeMap::new(),
                body: String::new(),
            }))
    }
}

pub struct EgressOutcome {
    pub response: EgressResponse,
    pub ok: bool,
    pub receipt: Option<EgressReceipt>,
}

#[derive(Clone)]
pub struct EgressBroker {
    transport: Arc<dyn EgressTransport>,
    stubbed: bool,
    credentials: Arc<dyn CredentialResolver>,
}

impl fmt::Debug for EgressBroker {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EgressBroker")
            .field("stubbed", &self.stubbed)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct EgressInvocation {
    pub broker: EgressBroker,
    pub policy: EgressPolicy,
    pub mode: EgressMode,
}

impl EgressBroker {
    pub fn live() -> Self {
        Self {
            transport: Arc::new(ReqwestTransport::new()),
            stubbed: false,
            credentials: Arc::new(SystemCredentialStore),
        }
    }

    pub fn stub(
        transport: Arc<dyn EgressTransport>,
        credentials: Arc<dyn CredentialResolver>,
    ) -> Self {
        Self {
            transport,
            stubbed: true,
            credentials,
        }
    }

    pub fn fetch(
        &self,
        caller: &str,
        policy: &EgressPolicy,
        mode: EgressMode,
        request: &EgressRequest,
        logs: &mut Vec<ScriptLog>,
    ) -> EgressOutcome {
        if !policy.capability_granted {
            return denied(
                logs,
                format!("egress denied: plugin '{caller}' lacks the brokered-egress capability"),
            );
        }
        let url = match reqwest::Url::parse(&request.url) {
            Ok(url) => url,
            Err(_) => {
                return denied(
                    logs,
                    format!("egress denied: '{}' is not a valid URL", request.url),
                );
            }
        };
        if url.scheme() != "https" {
            return denied(
                logs,
                format!(
                    "egress denied: only https URLs are brokered ({})",
                    request.url
                ),
            );
        }
        let host = url.host_str().unwrap_or_default();
        let Some(allowance) = policy
            .allowances
            .iter()
            .find(|allowance| allowance.domain.eq_ignore_ascii_case(host))
        else {
            return denied(
                logs,
                format!("egress denied: '{host}' is not in the egress allow-list for '{caller}'"),
            );
        };
        let mut headers = request.headers.clone();
        let mut injected_headers = Vec::new();
        if let Some(secret) = &allowance.secret {
            let secret_value = match self.credentials.get(&secret.credential_key) {
                Ok(value) => value,
                Err(_) => {
                    return denied(
                        logs,
                        format!(
                            "egress denied: credential '{}' is unavailable for '{host}'",
                            secret.credential_key
                        ),
                    );
                }
            };
            headers.retain(|name, _| !name.eq_ignore_ascii_case(&secret.header));
            headers.insert(
                secret.header.clone(),
                secret.value_template.replace("{secret}", &secret_value),
            );
            injected_headers.push(secret.header.clone());
        }
        let method = request.method.to_uppercase();
        let request_hash = canonical_json_hash(
            EGRESS_REQUEST_DOMAIN,
            &json!({
                "method": method.clone(),
                "url": request.url,
                "body": request.body,
                "injected_headers": injected_headers,
            }),
        )
        .expect("egress request hash inputs are serializable");
        let exchange = |transport_request: &EgressRequest| {
            if mode == EgressMode::DryRun && !self.stubbed {
                return Ok(EgressResponse {
                    status: 200,
                    status_text: "OK".to_owned(),
                    headers: BTreeMap::new(),
                    body: String::new(),
                });
            }
            self.transport.roundtrip(transport_request)
        };
        match exchange(&EgressRequest {
            url: request.url.clone(),
            method: method.clone(),
            headers,
            body: request.body.clone(),
        }) {
            Ok(response) => {
                let ok = (200..300).contains(&response.status);
                EgressOutcome {
                    receipt: Some(EgressReceipt {
                        url: request.url.clone(),
                        method,
                        request_hash,
                        status: response.status,
                        response_hash: content_blob_hash(response.body.as_bytes()),
                        body: response.body.clone(),
                    }),
                    response,
                    ok,
                }
            }
            Err(error) => {
                let message = error.to_string();
                EgressOutcome {
                    response: EgressResponse {
                        status: 0,
                        status_text: "transport error".to_owned(),
                        headers: BTreeMap::new(),
                        body: message.clone(),
                    },
                    ok: false,
                    receipt: Some(EgressReceipt {
                        url: request.url.clone(),
                        method,
                        request_hash,
                        status: 0,
                        response_hash: content_blob_hash(message.as_bytes()),
                        body: message,
                    }),
                }
            }
        }
    }
}

fn denied(logs: &mut Vec<ScriptLog>, message: String) -> EgressOutcome {
    logs.push(ScriptLog {
        level: "warn".to_owned(),
        message,
    });
    EgressOutcome {
        response: EgressResponse {
            status: 0,
            status_text: DENIED_STATUS_TEXT.to_owned(),
            headers: BTreeMap::new(),
            body: String::new(),
        },
        ok: false,
        receipt: None,
    }
}
