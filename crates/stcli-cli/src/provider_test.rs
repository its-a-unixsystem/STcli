use std::{
    collections::{BTreeMap, VecDeque},
    convert::Infallible,
    fs,
    net::SocketAddr,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response, Sse, sse::Event},
    routing::{get, post},
};
use axum_server::tls_rustls::RustlsConfig;
use rcgen::{CertifiedKey, generate_simple_self_signed};
use serde_json::{Value, json};
use stcli_core::{
    ExternalFixtureSource, FixtureCase, FixtureHistoryTurn, FixtureReport, FixtureSuite,
    ProviderRequestParityCase, ProviderSettings, SessionConfiguration, Store, canonical_json_hash,
};
use tokio::sync::Mutex;

const REQUEST_HASH_DOMAIN: &str = "stcli:provider-request:v1";

pub fn app() -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/chat/completions", post(chat_completions))
}

#[derive(Clone)]
struct ParityProviderState {
    responses: Arc<Mutex<VecDeque<String>>>,
    calls: Arc<AtomicUsize>,
}

pub async fn verify_provider_request_parity(
    fixtures_path: &Path,
    mut report: FixtureReport,
) -> Result<FixtureReport> {
    for (manifest_path, suite) in load_fixture_suites(fixtures_path)? {
        let sources = suite
            .external_sources
            .iter()
            .map(|source| (source.name.as_str(), source))
            .collect::<BTreeMap<_, _>>();
        for case in suite.cases {
            let FixtureCase::ProviderRequestParity(case) = case else {
                continue;
            };
            let Some(preset_path) = external_path(&sources, &case.preset_source, &manifest_path)
            else {
                continue;
            };
            let Some(oracle_path) = external_path(&sources, &case.oracle_source, &manifest_path)
            else {
                continue;
            };
            run_provider_request_parity_case(&case, &preset_path, &oracle_path).await?;
            let file = manifest_path.display().to_string();
            let case_report = report
                .cases
                .iter_mut()
                .find(|entry| entry.file == file && entry.name == case.name)
                .with_context(|| format!("parity case '{}' is absent from report", case.name))?;
            case_report.passed = true;
            case_report.not_run = false;
            case_report.message = "complete Dry Run provider request matches oracle".to_owned();
            report.passed += 1;
            report.not_run = report.not_run.saturating_sub(1);
        }
    }
    Ok(report)
}

fn load_fixture_suites(path: &Path) -> Result<Vec<(std::path::PathBuf, FixtureSuite)>> {
    let mut files = if path.is_file() {
        vec![path.to_owned()]
    } else {
        fs::read_dir(path)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>()
    };
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let suite = serde_json::from_slice(&fs::read(&path)?)?;
            Ok((path, suite))
        })
        .collect()
}

fn external_path(
    sources: &BTreeMap<&str, &ExternalFixtureSource>,
    name: &str,
    manifest_path: &Path,
) -> Option<std::path::PathBuf> {
    sources.get(name)?.resolve_path(manifest_path)
}

async fn run_provider_request_parity_case(
    case: &ProviderRequestParityCase,
    preset_path: &Path,
    oracle_path: &Path,
) -> Result<()> {
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
    let calls = Arc::new(AtomicUsize::new(0));
    let state = ParityProviderState {
        responses: Arc::new(Mutex::new(
            case.history
                .iter()
                .map(|turn| turn.assistant.clone())
                .collect(),
        )),
        calls: Arc::clone(&calls),
    };
    let router = Router::new()
        .route("/v1/chat/completions", post(parity_chat_completions))
        .with_state(state);
    let server = tokio::spawn(async move {
        axum_server::from_tcp_rustls(listener, tls)
            .expect("fixture listener is valid")
            .serve(router.into_make_service())
            .await
    });
    let directory =
        std::env::temp_dir().join(format!("stcli-parity-{}", stcli_core::EntityId::new()));
    fs::create_dir_all(&directory)?;
    let result = run_parity_dry_run(
        case,
        preset_path,
        oracle_path,
        address,
        certificate_pem,
        &calls,
        &directory,
    )
    .await;
    server.abort();
    let _ = fs::remove_dir_all(directory);
    result
}

async fn run_parity_dry_run(
    case: &ProviderRequestParityCase,
    preset_path: &Path,
    oracle_path: &Path,
    address: SocketAddr,
    certificate_pem: String,
    calls: &AtomicUsize,
    directory: &Path,
) -> Result<()> {
    let mut store = Store::open(directory.join("stcli.sqlite3"))?;
    let character = store.import_artifact(case.character.as_bytes())?;
    let preset = store.import_artifact(&fs::read(preset_path)?)?;
    let mut configuration = SessionConfiguration {
        compatibility_profile: "sillytavern-1.18-core".to_owned(),
        character_revision: character.revision_hash,
        persona_name: case.persona_name.clone(),
        lorebook_revisions: vec![],
        prompt_preset_revision: Some(preset.revision_hash),
        provider: ProviderSettings {
            id: "compat-parity".to_owned(),
            base_url: format!("https://{address}"),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 30,
            ca_certificate_pem: Some(certificate_pem),
            model: case.provider_model.clone(),
            stream: false,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        },
        tokenizer: case.tokenizer.to_string(),
        generation_settings: json!({}),
        plugins: vec![],
        script_grants: vec![],
    };
    let created = store.create_session(configuration.clone(), 0)?;
    let mut last_turn = None;
    for FixtureHistoryTurn { user, .. } in &case.history {
        last_turn = Some(
            store
                .send_message(
                    created.session.session_id,
                    created.branch.branch_id,
                    user.clone(),
                    |_| {},
                )
                .await?
                .turn,
        );
    }
    configuration.provider.stream = case.provider_stream;
    store.update_session_configuration(created.session.session_id, configuration)?;
    let calls_before = calls.load(Ordering::SeqCst);
    let events_before = store.trace_events(None)?.len();
    let turns_before = store.turns_for_branch(created.branch.branch_id)?.len();
    let last_turn = last_turn.context("parity history must contain a Turn")?;
    let dry_run = match case.generation_type.as_str() {
        "normal" => store.dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            &case.user_content,
        )?,
        "continue" => store.dry_run_continue(last_turn.turn_id)?,
        "regenerate" => store.dry_run_regenerate(last_turn.turn_id)?,
        "swipe" => store.dry_run_swipe(last_turn.turn_id)?,
        other => anyhow::bail!("unsupported parity generation type '{other}'"),
    };
    let oracle = serde_json::from_slice::<Value>(&fs::read(oracle_path)?)?;
    let expected_messages = oracle[&case.oracle_key]
        .as_array()
        .context("oracle message key must be an array")?
        .iter()
        .map(|message| {
            json!({
                "role": message["role"],
                "content": message["content"],
            })
        })
        .collect::<Vec<_>>();
    anyhow::ensure!(
        expected_messages.len() == case.expected_message_count,
        "expected {} oracle messages, found {}",
        case.expected_message_count,
        expected_messages.len()
    );
    let mut expected_request = case
        .expected_settings
        .as_object()
        .cloned()
        .context("expected settings must be an object")?;
    expected_request.insert("model".to_owned(), json!(case.provider_model));
    expected_request.insert("stream".to_owned(), json!(case.provider_stream));
    expected_request.insert("messages".to_owned(), json!(expected_messages));
    let expected_request = Value::Object(expected_request);
    anyhow::ensure!(
        dry_run.provider_request == expected_request,
        "{} request mismatch: expected {}, actual {}; {}",
        case.generation_type,
        canonical_json_hash(REQUEST_HASH_DOMAIN, &expected_request)?,
        canonical_json_hash(REQUEST_HASH_DOMAIN, &dry_run.provider_request)?,
        first_request_difference(&expected_request, &dry_run.provider_request)
    );
    let actual_effective_settings_hash = canonical_json_hash(
        "stcli:fixture-effective-generation-settings:v1",
        &serde_json::to_value(&dry_run.effective_generation_settings)?,
    )?;
    anyhow::ensure!(
        actual_effective_settings_hash == case.expected_effective_settings_hash,
        "Effective Generation Settings or provenance mismatch: expected {}, actual {}",
        case.expected_effective_settings_hash,
        actual_effective_settings_hash
    );
    let actual_warnings_hash = canonical_json_hash(
        "stcli:fixture-compatibility-warnings:v1",
        &serde_json::to_value(&dry_run.compatibility_warnings)?,
    )?;
    anyhow::ensure!(
        actual_warnings_hash == case.expected_warnings_hash,
        "Compatibility Warnings mismatch: expected {}, actual {}",
        case.expected_warnings_hash,
        actual_warnings_hash
    );
    anyhow::ensure!(
        dry_run.prompt_plan.pruning == case.expected_pruning,
        "pruning metadata mismatch: expected {:?}, actual {:?}",
        case.expected_pruning,
        dry_run.prompt_plan.pruning
    );
    for code in &case.expected_warning_codes {
        anyhow::ensure!(
            dry_run
                .compatibility_warnings
                .iter()
                .any(|warning| &warning.code == code),
            "missing compatibility warning '{code}'"
        );
    }
    anyhow::ensure!(
        dry_run
            .prompt_plan
            .macro_evaluations
            .iter()
            .filter(|evaluation| evaluation.name.eq_ignore_ascii_case("setvar"))
            .count()
            == case.expected_setvar_evaluations,
        "setvar evaluation count differs"
    );
    anyhow::ensure!(
        dry_run.prompt_plan.state_mutations.len() == case.expected_state_mutations,
        "state mutation count differs"
    );
    let max_context = dry_run.effective_generation_settings.values["max_context"]
        .as_u64()
        .context("max_context must resolve")? as usize;
    let max_tokens = dry_run.effective_generation_settings.values["max_tokens"]
        .as_u64()
        .context("max_tokens must resolve")? as usize;
    anyhow::ensure!(
        dry_run.prompt_plan.pruning.prompt_limit == max_context.saturating_sub(max_tokens),
        "prompt budget differs from Effective Generation Settings"
    );
    anyhow::ensure!(
        calls.load(Ordering::SeqCst) == calls_before,
        "Dry Run called provider"
    );
    anyhow::ensure!(
        store.trace_events(None)?.len() == events_before,
        "Dry Run changed Turn Trace"
    );
    anyhow::ensure!(
        store.turns_for_branch(created.branch.branch_id)?.len() == turns_before,
        "Dry Run created a Turn"
    );
    Ok(())
}

async fn parity_chat_completions(
    State(state): State<ParityProviderState>,
    Json(request): Json<Value>,
) -> Json<Value> {
    state.calls.fetch_add(1, Ordering::SeqCst);
    let content = state.responses.lock().await.pop_front().unwrap_or_default();
    Json(json!({
        "id": "chatcmpl-parity-history",
        "object": "chat.completion",
        "created": 0,
        "model": request.get("model").and_then(Value::as_str).unwrap_or("fixture-model"),
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": content},
            "finish_reason": "stop"
        }]
    }))
}

fn first_request_difference(expected: &Value, actual: &Value) -> String {
    let (Some(expected), Some(actual)) = (expected.as_object(), actual.as_object()) else {
        return "request is not an object".to_owned();
    };
    for field in expected.keys().chain(actual.keys()) {
        if field != "messages" && expected.get(field) != actual.get(field) {
            return format!(
                "field {field}: expected {:?}, actual {:?}",
                expected.get(field),
                actual.get(field)
            );
        }
    }
    let expected_messages = expected["messages"].as_array();
    let actual_messages = actual["messages"].as_array();
    let length = expected_messages
        .map(Vec::len)
        .unwrap_or_default()
        .max(actual_messages.map(Vec::len).unwrap_or_default());
    for index in 0..length {
        let expected = expected_messages.and_then(|messages| messages.get(index));
        let actual = actual_messages.and_then(|messages| messages.get(index));
        if expected != actual {
            return format!("message {index}: expected {expected:?}, actual {actual:?}");
        }
    }
    "request structures differ".to_owned()
}

pub async fn serve(bind: SocketAddr, certificate_output: Option<&Path>) -> Result<()> {
    let CertifiedKey { cert, signing_key } =
        generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
            .context("failed to generate test TLS certificate")?;
    let certificate_pem = cert.pem();
    if let Some(path) = certificate_output {
        fs::write(path, &certificate_pem)
            .with_context(|| format!("failed to write test certificate '{}'", path.display()))?;
    }
    let tls = RustlsConfig::from_pem(
        certificate_pem.into_bytes(),
        signing_key.serialize_pem().into_bytes(),
    )
    .await
    .context("failed to configure test TLS server")?;

    println!("ready https://{bind}/v1/chat/completions");
    axum_server::bind_rustls(bind, tls)
        .serve(app().into_make_service())
        .await
        .context("test provider server failed")
}

async fn health() -> Json<Value> {
    Json(json!({"status": "ok", "schema": "stcli.provider-test/v1"}))
}

async fn chat_completions(headers: HeaderMap, Json(request): Json<Value>) -> Response {
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("fixture-model")
        .to_owned();
    let stream_requested = request
        .get("stream")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Some(header_name) = request.get("fixture_echo_header").and_then(Value::as_str) {
        let echoed = headers
            .get(header_name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": format!("echo:{echoed}")}})),
        )
            .into_response();
    }
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
    let request_hash = canonical_json_hash(REQUEST_HASH_DOMAIN, &request)
        .expect("JSON request canonicalization cannot fail")
        .to_string();
    let content = format!("fixture-response:{request_hash}");
    let reasoning = request
        .get("fixture_reasoning")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reasoning_content = request
        .get("fixture_reasoning_content")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let reasoning_only = request
        .get("fixture_reasoning_only")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    if stream_requested {
        let delay_ms = request
            .get("fixture_chunk_delay_ms")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        streaming_response(
            model,
            content,
            reasoning,
            reasoning_content,
            reasoning_only,
            &request,
            delay_ms,
        )
    } else {
        let mut message = json!({"role": "assistant", "content": content});
        if reasoning_only {
            message
                .as_object_mut()
                .expect("fixture message is an object")
                .remove("content");
        }
        if let Some(reasoning) = reasoning {
            message["reasoning"] = Value::String(reasoning);
        }
        if let Some(reasoning_content) = reasoning_content {
            message["reasoning_content"] = Value::String(reasoning_content);
        }
        Json(json!({
            "id": "chatcmpl-stcli-fixture",
            "object": "chat.completion",
            "created": 0,
            "model": model,
            "choices": [{
                "index": 0,
                "message": message,
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
        }))
        .into_response()
    }
}

fn streaming_response(
    model: String,
    content: String,
    reasoning: Option<String>,
    reasoning_content: Option<String>,
    reasoning_only: bool,
    request: &Value,
    delay_ms: u64,
) -> Response {
    let split = content.len() / 2;
    let first = content[..split].to_owned();
    let second = content[split..].to_owned();
    let disconnect_after = request
        .get("fixture_disconnect_after_chunks")
        .and_then(Value::as_u64);
    let malformed = request
        .get("fixture_sse_malformed")
        .and_then(Value::as_str)
        .map(|value| value.to_owned());
    let events = async_stream::stream! {
        let mut emitted = 0u64;
        if malformed.as_deref() == Some("bad-json") {
            yield Ok::<_, Infallible>(Event::default().data("this is not json"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
        }
        yield Ok(Event::default().json_data(json!({
            "id": "chatcmpl-stcli-fixture",
            "object": "chat.completion.chunk",
            "created": 0,
            "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        })).expect("fixture event is JSON"));
        emitted += 1;
        if disconnect_after == Some(emitted) {
            return;
        }
        if let Some(reasoning) = reasoning {
            yield Ok(Event::default().json_data(json!({
                "id": "chatcmpl-stcli-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {"reasoning": reasoning}, "finish_reason": null}]
            })).expect("fixture event is JSON"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
        }
        if let Some(reasoning_content) = reasoning_content {
            yield Ok(Event::default().json_data(json!({
                "id": "chatcmpl-stcli-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {"reasoning_content": reasoning_content}, "finish_reason": null}]
            })).expect("fixture event is JSON"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
        }
        if !reasoning_only {
            yield Ok(Event::default().json_data(json!({
                "id": "chatcmpl-stcli-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {"content": first}, "finish_reason": null}]
            })).expect("fixture event is JSON"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
            if delay_ms > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
            }
            yield Ok(Event::default().json_data(json!({
                "id": "chatcmpl-stcli-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {"content": second}, "finish_reason": null}]
            })).expect("fixture event is JSON"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
        }
        if malformed.as_deref() == Some("truncate-mid-event") {
            yield Ok(Event::default().data("data: {\"id\": \"chatcmpl-stcli-fixture\", \"object\": \"chat.completion.chunk\", \"created\": 0"));
            return;
        }
        if malformed.as_deref() != Some("missing-done") {
            yield Ok(Event::default().json_data(json!({
                "id": "chatcmpl-stcli-fixture",
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                "usage": {"prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10}
            })).expect("fixture event is JSON"));
            emitted += 1;
            if disconnect_after == Some(emitted) {
                return;
            }
            yield Ok(Event::default().data("[DONE]"));
        }
    };
    Sse::new(events).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    };

    use stcli_core::{
        AttemptStatus, CandidateOrigin, CliEnvelope, CliError, GenerationType, HeaderSetting,
        OpenAiProvider, ProviderEvent, ProviderSettings, SessionConfiguration, Store, TurnError,
    };
    use stcli_testkit::EnvironmentGuard;
    use tempfile::tempdir;

    #[test]
    fn request_hash_changes_fixture_response() {
        let first = canonical_json_hash(REQUEST_HASH_DOMAIN, &json!({"model": "a"})).unwrap();
        let second = canonical_json_hash(REQUEST_HASH_DOMAIN, &json!({"model": "b"})).unwrap();
        assert_ne!(first, second);
    }

    #[tokio::test]
    async fn reasoning_events_cover_stream_aliases_non_streaming_and_reasoning_only_completion() {
        // Regression test for issue #60: reasoning-only streams must emit live events and complete.
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .unwrap();
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app().into_make_service())
                .await
        });
        let provider_settings = ProviderSettings {
            id: "reasoning-events".to_owned(),
            base_url: format!("https://{address}"),
            chat_completions_path: "/v1/chat/completions".to_owned(),
            api_key_env: None,
            static_headers: BTreeMap::new(),
            timeout_seconds: 5,
            ca_certificate_pem: Some(certificate_pem),
            model: "fixture-model".to_owned(),
            stream: true,
            format_mode: Default::default(),
            completions_path: None,
            instruct_template: None,
            context_formatting: None,
        };

        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let card = store
            .import_artifact(stcli_testkit::fixtures::minimal_card().as_bytes())
            .unwrap();
        let created = store
            .create_session(
                SessionConfiguration {
                    compatibility_profile: "sillytavern-1.18-core".to_owned(),
                    character_revision: card.revision_hash,
                    persona_name: "User".to_owned(),
                    lorebook_revisions: vec![],
                    prompt_preset_revision: None,
                    provider: provider_settings.clone(),
                    tokenizer: "tiktoken:o200k_base".to_owned(),
                    generation_settings: json!({
                        "fixture_reasoning": "Plan ",
                        "fixture_reasoning_content": "step",
                        "fixture_reasoning_only": true
                    }),
                    plugins: vec![],
                    script_grants: vec![],
                },
                0,
            )
            .unwrap();
        let mut stream_events = Vec::new();
        let completed = store
            .send_message(
                created.session.session_id,
                created.branch.branch_id,
                "Hello".to_owned(),
                |event| stream_events.push(event.clone()),
            )
            .await
            .unwrap();

        assert_eq!(completed.attempt.status, AttemptStatus::Completed);
        assert_eq!(completed.candidate.content, "");
        assert_eq!(
            stream_events
                .iter()
                .filter_map(|event| match event {
                    ProviderEvent::ReasoningDelta { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            ["Plan ", "step"]
        );
        assert_eq!(
            serde_json::to_value(&stream_events[1]).unwrap(),
            json!({"event_type": "reasoning-delta", "text": "Plan "})
        );

        let mut non_streaming_settings = provider_settings;
        non_streaming_settings.stream = false;
        let provider = OpenAiProvider::new(non_streaming_settings).unwrap();
        let mut non_streaming_events = Vec::new();
        let result = provider
            .generate_request(
                &json!({
                    "model": "fixture-model",
                    "stream": false,
                    "fixture_reasoning_content": "Considered alternatives"
                }),
                |event| non_streaming_events.push(event.clone()),
            )
            .await
            .unwrap();

        assert!(!result.text.is_empty());
        assert!(non_streaming_events.iter().any(|event| {
            matches!(
                event,
                ProviderEvent::ReasoningDelta { text } if text == "Considered alternatives"
            )
        }));
        server.abort();
    }
    #[tokio::test]
    async fn split_sse_cancellation_records_partial_text_and_stops_request() {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .unwrap();
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app().into_make_service())
                .await
        });

        let directory = tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let card = store
            .import_artifact(
                br#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"Alice","description":"A librarian.","personality":"Curious","scenario":"A library","first_mes":"Welcome.","mes_example":"","alternate_greetings":[],"extensions":{}}}"#,
            )
            .unwrap();
        let created = store
            .create_session(
                SessionConfiguration {
                    compatibility_profile: "sillytavern-1.18-core".to_owned(),
                    character_revision: card.revision_hash,
                    persona_name: "User".to_owned(),
                    lorebook_revisions: vec![],
                    prompt_preset_revision: None,
                    provider: ProviderSettings {
                        id: "split-sse".to_owned(),
                        base_url: format!("https://{address}"),
                        chat_completions_path: "/v1/chat/completions".to_owned(),
                        api_key_env: None,
                        static_headers: BTreeMap::new(),
                        timeout_seconds: 5,
                        ca_certificate_pem: Some(certificate_pem),
                        model: "fixture-model".to_owned(),
                        stream: true,
                        format_mode: Default::default(),
                        completions_path: None,
                        instruct_template: None,
                        context_formatting: None,
                    },
                    tokenizer: "tiktoken:o200k_base".to_owned(),
                    generation_settings: json!({"fixture_chunk_delay_ms": 2_000}),
                    plugins: vec![],
                    script_grants: vec![],
                },
                0,
            )
            .unwrap();
        let saw_delta = Arc::new(AtomicBool::new(false));
        let send_delta = Arc::clone(&saw_delta);
        let session_id = created.session.session_id;
        let branch_id = created.branch.branch_id;
        let send = tokio::spawn(async move {
            store
                .send_message(
                    session_id,
                    branch_id,
                    "{{setvar::cancelled::no}}Hello".to_owned(),
                    |event| {
                        if matches!(event, ProviderEvent::TextDelta { .. }) {
                            send_delta.store(true, Ordering::SeqCst);
                        }
                    },
                )
                .await
        });

        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !saw_delta.load(Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let mut cancelling_store = Store::open(&database).unwrap();
        let turn = cancelling_store
            .turns_for_branch(branch_id)
            .unwrap()
            .remove(0);
        let attempt = cancelling_store
            .attempts_for_turn(turn.turn_id)
            .unwrap()
            .remove(0);
        cancelling_store.cancel_attempt(attempt.attempt_id).unwrap();

        let result = tokio::time::timeout(std::time::Duration::from_secs(1), send)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(matches!(
            result,
            TurnError::AttemptNotRunning {
                status: AttemptStatus::Cancelled,
                ..
            }
        ));
        let cancelled = cancelling_store
            .attempt(attempt.attempt_id)
            .unwrap()
            .unwrap();
        let partial = cancelled
            .provider_receipt
            .as_ref()
            .and_then(|receipt| receipt.get("partial_text"))
            .and_then(Value::as_str)
            .unwrap();
        assert!(!partial.is_empty());
        let candidates = cancelling_store.candidates_for_turn(turn.turn_id).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].origin, CandidateOrigin::AcceptedPartial);
        assert_eq!(candidates[0].content, partial);
        assert_eq!(
            cancelling_store
                .turn(turn.turn_id)
                .unwrap()
                .unwrap()
                .selected_candidate_id,
            Some(candidates[0].candidate_id)
        );
        assert!(
            cancelling_store
                .trace_events(Some(session_id))
                .unwrap()
                .iter()
                .any(|event| event.event_type == "attempt.cancellation-receipt"
                    && event.payload["partial_text"]
                        .as_str()
                        .is_some_and(|text| !text.is_empty()))
        );
        assert!(
            cancelling_store
                .state_transaction(session_id)
                .unwrap()
                .get(stcli_core::VariableScope::Local, "cancelled")
                .is_none()
        );
        let candidate_id = candidates[0].candidate_id;
        cancelling_store.rebuild_session_projections().unwrap();
        let rebuilt_candidates = cancelling_store.candidates_for_turn(turn.turn_id).unwrap();
        assert_eq!(rebuilt_candidates.len(), 1);
        assert_eq!(rebuilt_candidates[0].candidate_id, candidate_id);
        assert_eq!(
            rebuilt_candidates[0].origin,
            CandidateOrigin::AcceptedPartial
        );
        assert_eq!(
            cancelling_store
                .turn(turn.turn_id)
                .unwrap()
                .unwrap()
                .selected_candidate_id,
            Some(candidate_id)
        );
        server.abort();
    }

    #[tokio::test]
    async fn alternatives_continue_swipe_and_edits_preserve_candidates_and_branches() {
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .unwrap();
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app().into_make_service())
                .await
        });

        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let card = store
            .import_artifact(
                br#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"Alice","description":"","personality":"","scenario":"","first_mes":"Welcome.","mes_example":"","alternate_greetings":[],"extensions":{}}}"#,
            )
            .unwrap();
        let created = store
            .create_session(
                SessionConfiguration {
                    compatibility_profile: "sillytavern-1.18-core".to_owned(),
                    character_revision: card.revision_hash,
                    persona_name: "User".to_owned(),
                    lorebook_revisions: vec![],
                    prompt_preset_revision: None,
                    provider: ProviderSettings {
                        id: "turn-operations".to_owned(),
                        base_url: format!("https://{address}"),
                        chat_completions_path: "/v1/chat/completions".to_owned(),
                        api_key_env: None,
                        static_headers: BTreeMap::new(),
                        timeout_seconds: 5,
                        ca_certificate_pem: Some(certificate_pem),
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
                },
                0,
            )
            .unwrap();
        let first = store
            .send_message(
                created.session.session_id,
                created.branch.branch_id,
                "Hello".to_owned(),
                |_| {},
            )
            .await
            .unwrap();
        let regenerated = store
            .regenerate_turn(first.turn.turn_id, |_| {})
            .await
            .unwrap();
        assert_eq!(
            regenerated.attempt.prompt_plan.generation_type,
            GenerationType::Regenerate
        );
        assert_eq!(regenerated.candidate.origin, CandidateOrigin::Generated);
        assert_eq!(
            store.candidates_for_turn(first.turn.turn_id).unwrap().len(),
            2
        );

        let continued = store
            .continue_turn(first.turn.turn_id, |_| {})
            .await
            .unwrap();
        assert_eq!(continued.candidate.origin, CandidateOrigin::Continued);
        assert_eq!(
            continued.candidate.parent_candidate_id,
            Some(regenerated.candidate.candidate_id)
        );
        assert!(
            continued
                .candidate
                .content
                .starts_with(&regenerated.candidate.content)
        );

        let selected = store
            .select_swipe(first.turn.turn_id, first.candidate.candidate_id)
            .unwrap();
        assert_eq!(
            selected.selected_candidate_id,
            Some(first.candidate.candidate_id)
        );
        let edited = store
            .edit_candidate(first.candidate.candidate_id, "Manual answer".to_owned())
            .unwrap();
        assert_eq!(edited.candidate.origin, CandidateOrigin::Manual);
        assert_eq!(
            edited.candidate.parent_candidate_id,
            Some(first.candidate.candidate_id)
        );
        assert_eq!(
            store
                .turns_for_branch(edited.branch.branch_id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(edited.candidate.attempt_id, None);

        let edited_user = store
            .edit_user_turn(first.turn.turn_id, "Edited hello".to_owned(), |_| {})
            .await
            .unwrap();
        assert_ne!(edited_user.turn.branch_id, first.turn.branch_id);
        assert_eq!(edited_user.turn.user_content, "Edited hello");

        store.rebuild_session_projections().unwrap();
        assert_eq!(
            store.candidates_for_turn(first.turn.turn_id).unwrap().len(),
            3
        );
        assert_eq!(
            store
                .candidate(edited.candidate.candidate_id)
                .unwrap()
                .unwrap()
                .origin,
            CandidateOrigin::Manual
        );
        server.abort();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn echoed_provider_secret_is_redacted_from_error_trace_sqlite_and_cli() {
        let mut environment = EnvironmentGuard::new();
        let secret = "echoed-provider-secret";
        environment.set("STCLI_ECHO_HEADER", secret);
        let CertifiedKey { cert, signing_key } =
            generate_simple_self_signed(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])
                .unwrap();
        let certificate_pem = cert.pem();
        let tls = RustlsConfig::from_pem(
            certificate_pem.clone().into_bytes(),
            signing_key.serialize_pem().into_bytes(),
        )
        .await
        .unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum_server::from_tcp_rustls(listener, tls)
                .unwrap()
                .serve(app().into_make_service())
                .await
        });

        let directory = tempdir().unwrap();
        let database = directory.path().join("stcli.sqlite3");
        let mut store = Store::open(&database).unwrap();
        let card = store
            .import_artifact(
                br#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"Alice","description":"","personality":"","scenario":"","first_mes":"Welcome.","mes_example":"","alternate_greetings":[],"extensions":{}}}"#,
            )
            .unwrap();
        let mut headers = BTreeMap::new();
        headers.insert(
            "x-api-key".to_owned(),
            HeaderSetting::Environment("STCLI_ECHO_HEADER".to_owned()),
        );
        let created = store
            .create_session(
                SessionConfiguration {
                    compatibility_profile: "sillytavern-1.18-core".to_owned(),
                    character_revision: card.revision_hash,
                    persona_name: "User".to_owned(),
                    lorebook_revisions: vec![],
                    prompt_preset_revision: None,
                    provider: ProviderSettings {
                        id: "echo-secret".to_owned(),
                        base_url: format!("https://{address}"),
                        chat_completions_path: "/v1/chat/completions".to_owned(),
                        api_key_env: None,
                        static_headers: headers,
                        timeout_seconds: 5,
                        ca_certificate_pem: Some(certificate_pem),
                        model: "fixture-model".to_owned(),
                        stream: false,
                        format_mode: Default::default(),
                        completions_path: None,
                        instruct_template: None,
                        context_formatting: None,
                    },
                    tokenizer: "tiktoken:o200k_base".to_owned(),
                    generation_settings: json!({"fixture_echo_header": "x-api-key"}),
                    plugins: vec![],
                    script_grants: vec![],
                },
                0,
            )
            .unwrap();
        let error = store
            .send_message(
                created.session.session_id,
                created.branch.branch_id,
                "Hello".to_owned(),
                |_| {},
            )
            .await
            .unwrap_err();
        let error_text = error.to_string();
        assert!(!error_text.contains(secret));
        assert!(error_text.contains("[REDACTED]"));
        let cli = serde_json::to_vec(&CliEnvelope::<Value>::failure(
            "message.send",
            CliError {
                code: "command_failed".to_owned(),
                message: error_text,
                details: None,
            },
        ))
        .unwrap();
        let trace = serde_json::to_vec(
            &store
                .trace_events(Some(created.session.session_id))
                .unwrap(),
        )
        .unwrap();
        drop(store);
        let sqlite = fs::read(&database).unwrap();
        assert!(!String::from_utf8_lossy(&cli).contains(secret));
        assert!(!String::from_utf8_lossy(&trace).contains(secret));
        assert!(!String::from_utf8_lossy(&sqlite).contains(secret));

        server.abort();
    }

    #[tokio::test]
    async fn checked_in_oracle_matches_all_dry_run_generation_types() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let report = stcli_core::verify_fixture_suite(
            root.join("compat/profiles/sillytavern-1.18-core.json"),
            root.join("compat/fixtures"),
        )
        .unwrap();
        let report = verify_provider_request_parity(&root.join("compat/fixtures"), report)
            .await
            .unwrap();

        assert_eq!(report.not_run, 0);
        assert_eq!(
            report
                .cases
                .iter()
                .filter(|case| case.name.ends_with("provider request matches oracle"))
                .filter(|case| case.passed)
                .count(),
            4
        );
    }
}
