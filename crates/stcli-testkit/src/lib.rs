use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::{IntoResponse, Response},
    routing,
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use serde_json::{Value, json};
use stcli_core::{AppPaths, ContentHash, ProviderSettings, SessionConfiguration};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{OsStr, OsString},
    io::{BufRead, BufReader},
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{sync::Mutex as AsyncMutex, task::JoinHandle};

pub mod fixtures {
    pub const MINIMAL_CARD: &str = r#"{
  "spec":"chara_card_v2",
  "spec_version":"2.0",
  "data":{
    "name":"Alice",
    "description":"A librarian.",
    "personality":"Curious",
    "scenario":"An old library",
    "first_mes":"Welcome.",
    "mes_example":"",
    "alternate_greetings":["Hello again."],
    "extensions":{}
  }
}"#;

    pub fn minimal_card() -> &'static str {
        MINIMAL_CARD
    }

    pub fn character() -> &'static str {
        include_str!("../../../examples/character.json")
    }

    pub fn lorebook() -> &'static str {
        include_str!("../../../examples/lorebook.json")
    }

    pub fn preset() -> &'static str {
        include_str!("../../../examples/preset.json")
    }
}

pub fn configuration(character_revision: ContentHash) -> SessionConfiguration {
    SessionConfiguration {
        compatibility_profile: "sillytavern-1.18-core".to_owned(),
        character_revision,
        persona_name: "User".to_owned(),
        persona_description: None,
        lorebook_revisions: vec![],
        prompt_preset_revision: None,
        prompt_order_overrides: BTreeMap::new(),
        provider: ProviderSettings {
            id: "invalid-http".to_owned(),
            base_url: "http://127.0.0.1:1".to_owned(),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            credential_key: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 1,
            ca_certificate_pem: None,
            model: "fixture-model".to_owned(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        },
        tokenizer: "tiktoken:o200k_base".to_owned(),
        generation_settings: json!({}),
        plugins: vec![],
        script_grants: vec![],
    }
}

pub struct TestHome {
    root: TempDir,
    binary: PathBuf,
}

impl TestHome {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            root: tempfile::tempdir()?,
            binary: stcli_binary(),
        })
    }

    pub fn root(&self) -> &Path {
        self.root.path()
    }

    pub fn paths(&self) -> AppPaths {
        AppPaths {
            config: self.root().join("config"),
            data: self.root().join("data"),
            cache: self.root().join("cache"),
        }
    }

    pub fn stcli_binary(&self) -> &Path {
        &self.binary
    }
}

pub fn stcli_cmd(home: &TestHome) -> Command {
    let mut command = Command::new(home.stcli_binary());
    command
        .env("STCLI_HOME", home.root())
        .env("STCLI_REGEX_WORKER", home.stcli_binary());
    command
}

fn stcli_binary() -> PathBuf {
    std::env::var_os("CARGO_BIN_EXE_stcli")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let executable = std::env::current_exe().expect("test executable path is available");
            let profile = executable
                .parent()
                .and_then(Path::parent)
                .expect("test executable is under target profile directory");
            profile.join(format!("stcli{}", std::env::consts::EXE_SUFFIX))
        })
}

static ENVIRONMENT_LOCK: Mutex<()> = Mutex::new(());

pub struct EnvironmentGuard {
    _lock: MutexGuard<'static, ()>,
    original: BTreeMap<OsString, Option<OsString>>,
}

impl EnvironmentGuard {
    pub fn new() -> Self {
        Self {
            _lock: ENVIRONMENT_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
            original: BTreeMap::new(),
        }
    }

    pub fn set(&mut self, name: impl AsRef<OsStr>, value: impl AsRef<OsStr>) {
        let name = name.as_ref();
        self.original
            .entry(name.to_owned())
            .or_insert_with(|| std::env::var_os(name));
        unsafe { std::env::set_var(name, value) };
    }

    pub fn remove(&mut self, name: impl AsRef<OsStr>) {
        let name = name.as_ref();
        self.original
            .entry(name.to_owned())
            .or_insert_with(|| std::env::var_os(name));
        unsafe { std::env::remove_var(name) };
    }
}

impl Default for EnvironmentGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for EnvironmentGuard {
    fn drop(&mut self) {
        for (name, value) in &self.original {
            unsafe {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }
}

#[derive(Clone)]
struct MockState {
    responses: Arc<AsyncMutex<VecDeque<String>>>,
}

pub struct MockProvider {
    address: SocketAddr,
    certificate_pem: String,
    server: JoinHandle<std::io::Result<()>>,
}

impl MockProvider {
    pub async fn spawn(
        responses: impl IntoIterator<Item = impl Into<String>>,
    ) -> anyhow::Result<Self> {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])?;
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let state = MockState {
            responses: Arc::new(AsyncMutex::new(
                responses.into_iter().map(Into::into).collect(),
            )),
        };
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/v1/chat/completions", post(mock_completion))
            .route("/v1/completions", post(mock_text_completion))
            .with_state(state);
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .expect("mock provider listener is valid")
                .serve(app.into_make_service())
                .await
        });
        let provider = Self {
            address,
            certificate_pem,
            server,
        };
        let client = reqwest::Client::builder()
            .danger_accept_invalid_certs(true)
            .build()?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if client.get(provider.health_url()).send().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(provider)
    }

    pub fn health_url(&self) -> String {
        format!("https://{}/health", self.address)
    }

    pub fn provider_settings(&self) -> ProviderSettings {
        ProviderSettings {
            id: "mock-provider".to_owned(),
            base_url: format!("https://{}", self.address),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            credential_key: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 2,
            ca_certificate_pem: Some(self.certificate_pem.clone()),
            model: "fixture-model".to_owned(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        }
    }

    pub async fn shutdown(self) {
        self.server.abort();
        let _ = self.server.await;
    }
}
async fn mock_completion(State(state): State<MockState>, Json(request): Json<Value>) -> Response {
    if let Some(status) = request.get("fixture_status").and_then(Value::as_u64) {
        let status = u16::try_from(status).unwrap_or(200);
        let body = if request
            .get("fixture_non_json")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            ("this is not json").into_response()
        } else {
            Json(json!({"error": {"message": format!("fixture status {status}")}})).into_response()
        };
        return (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body).into_response();
    }
    if let Some(true) = request.get("fixture_non_json").and_then(Value::as_bool) {
        return (StatusCode::OK, "this is not json").into_response();
    }
    if let Some(delay_ms) = request.get("fixture_delay_ms").and_then(Value::as_u64) {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
    }
    let content = state.responses.lock().await.pop_front().unwrap_or_default();
    Json(json!({
        "id": "mock-completion",
        "object": "chat.completion",
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    }))
    .into_response()
}

async fn mock_text_completion(
    State(state): State<MockState>,
    Json(request): Json<Value>,
) -> Response {
    let valid_prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .is_some_and(|prompt| !prompt.is_empty());
    let valid_stops = request
        .get("stop")
        .and_then(Value::as_array)
        .is_some_and(|stops| {
            stops.first().and_then(Value::as_str) == Some("configured-stop")
                && stops.iter().any(|stop| stop.as_str() == Some("<|im_end|>"))
        });
    if !valid_prompt || !valid_stops {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"message": "expected flat prompt and stop array"}})),
        )
            .into_response();
    }
    let content = state.responses.lock().await.pop_front().unwrap_or_default();
    Json(json!({
        "id": "mock-text-completion",
        "object": "text_completion",
        "choices": [{
            "index": 0,
            "text": content,
            "finish_reason": "stop"
        }]
    }))
    .into_response()
}

pub struct MockProviderProcess {
    child: Child,
    address: SocketAddr,
    certificate_pem: String,
}

impl MockProviderProcess {
    pub async fn spawn(home: &TestHome) -> anyhow::Result<Self> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        drop(listener);
        let certificate = home.root().join("provider-test-ca.pem");
        let mut command = stcli_cmd(home);
        let mut child = command
            .args([
                "provider-test",
                "serve",
                "--bind",
                &address.to_string(),
                "--certificate-output",
                certificate.to_str().expect("test path is UTF-8"),
            ])
            .stdout(Stdio::piped())
            .spawn()?;
        let stdout = child.stdout.take().expect("provider stdout is piped");
        let mut ready = String::new();
        BufReader::new(stdout).read_line(&mut ready)?;
        if !ready.starts_with("ready https://") {
            anyhow::bail!("provider-test exited before readiness: {ready}");
        }
        let certificate_pem = std::fs::read_to_string(certificate)?;
        let provider = Self {
            child,
            address,
            certificate_pem,
        };
        let client = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_pem(
                provider.certificate_pem.as_bytes(),
            )?)
            .build()?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if client.get(provider.health_url()).send().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(provider)
    }

    pub fn health_url(&self) -> String {
        format!("https://{}/health", self.address)
    }

    pub fn provider_settings(&self) -> ProviderSettings {
        ProviderSettings {
            id: "provider-test-process".to_owned(),
            base_url: format!("https://{}", self.address),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            credential_key: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 2,
            ca_certificate_pem: Some(self.certificate_pem.clone()),
            model: "fixture-model".to_owned(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        }
    }

    pub fn shutdown(self) {
        drop(self);
    }

    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for MockProviderProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedRequest {
    pub method: String,
    pub path: String,
    pub query: BTreeMap<String, String>,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct QueuedResponse {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

struct BrokerStateData {
    requests: Vec<CapturedRequest>,
    responses: VecDeque<QueuedResponse>,
}

#[derive(Clone)]
struct BrokerState {
    data: Arc<AsyncMutex<BrokerStateData>>,
}

async fn broker_capture(
    State(state): State<BrokerState>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: String,
) -> Response {
    let query = reqwest::Url::parse(&format!("https://fixture.invalid{uri}"))
        .expect("captured request URI is valid")
        .query_pairs()
        .map(|(name, value)| (name.into_owned(), value.into_owned()))
        .collect();
    let mut captured_headers = BTreeMap::new();
    for (name, value) in &headers {
        let value = String::from_utf8_lossy(value.as_bytes());
        let entry: &mut String = captured_headers
            .entry(name.as_str().to_owned())
            .or_default();
        if entry.is_empty() {
            entry.push_str(&value);
        } else {
            entry.push_str(", ");
            entry.push_str(&value);
        }
    }
    let mut state = state.data.lock().await;
    state.requests.push(CapturedRequest {
        method: method.to_string(),
        path: uri.path().to_owned(),
        query,
        headers: captured_headers,
        body,
    });
    let queued = state
        .responses
        .pop_front()
        .expect("broker test server response queue is exhausted");
    drop(state);
    let mut response = Response::builder()
        .status(
            StatusCode::from_u16(queued.status)
                .expect("broker test server response status is valid"),
        )
        .body(axum::body::Body::from(queued.body))
        .expect("broker test server response is constructible");
    for (name, value) in queued.headers {
        let name = axum::http::HeaderName::from_bytes(name.as_bytes())
            .expect("broker test server response header name is valid");
        let value = axum::http::HeaderValue::from_str(&value)
            .expect("broker test server response header value is valid");
        response.headers_mut().insert(name, value);
    }
    response
}

pub struct BrokerTestServer {
    address: SocketAddr,
    hostname: String,
    certificate_pem: String,
    server: JoinHandle<std::io::Result<()>>,
    state: Arc<AsyncMutex<BrokerStateData>>,
}

impl BrokerTestServer {
    pub async fn spawn(
        responses: impl IntoIterator<Item = QueuedResponse>,
    ) -> anyhow::Result<Self> {
        let hostname = "127.0.0.1".to_owned();
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), hostname.clone()])?;
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let state = Arc::new(AsyncMutex::new(BrokerStateData {
            requests: Vec::new(),
            responses: responses.into_iter().collect(),
        }));
        let router_state = BrokerState {
            data: state.clone(),
        };
        let app = Router::new()
            .route("/health", get(|| async { "ok" }))
            .route("/{*rest}", routing::any(broker_capture))
            .with_state(router_state);
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .expect("broker test server listener is valid")
                .serve(app.into_make_service())
                .await
        });
        let server = Self {
            address,
            hostname,
            certificate_pem,
            server,
            state,
        };
        let certificate = reqwest::Certificate::from_pem(server.certificate_pem.as_bytes())?;
        let client = reqwest::Client::builder()
            .add_root_certificate(certificate)
            .build()?;
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if client.get(server.health_url()).send().await.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await?;
        Ok(server)
    }

    pub fn health_url(&self) -> String {
        format!("https://{}/health", self.address)
    }

    pub fn certificate_pem(&self) -> &str {
        &self.certificate_pem
    }

    pub fn hostname(&self) -> &str {
        &self.hostname
    }

    pub fn base_url(&self) -> String {
        format!("https://{}", self.address)
    }

    pub async fn https_client(&self) -> anyhow::Result<reqwest::blocking::Client> {
        let certificate_pem = self.certificate_pem.clone();
        tokio::task::spawn_blocking(move || {
            let certificate = reqwest::Certificate::from_pem(certificate_pem.as_bytes())?;
            Ok(reqwest::blocking::Client::builder()
                .timeout(Duration::from_secs(5))
                .add_root_certificate(certificate)
                .build()?)
        })
        .await?
    }

    pub async fn captured_requests(&self) -> Vec<CapturedRequest> {
        self.state.lock().await.requests.clone()
    }

    pub async fn request_count(&self) -> usize {
        self.state.lock().await.requests.len()
    }
}

impl Drop for BrokerTestServer {
    fn drop(&mut self) {
        self.server.abort();
    }
}
