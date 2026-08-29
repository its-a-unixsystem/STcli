//! L3 binary provider failure mode coverage.

use std::{ffi::OsStr, process::Output};

use serde_json::Value;
use stcli_testkit::{MockProviderProcess, TestHome, fixtures, stcli_cmd};

fn run(home: &TestHome, args: &[&dyn AsRef<OsStr>]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
    stcli_cmd(home)
        .args([OsStr::new("--output"), OsStr::new("json")])
        .args(&args)
        .output()
        .unwrap()
}

fn json_lines(bytes: &[u8]) -> Vec<Value> {
    String::from_utf8(bytes.to_vec())
        .unwrap()
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn last_envelope(bytes: &[u8]) -> Value {
    let mut lines = json_lines(bytes);
    assert!(!lines.is_empty());
    lines.pop().unwrap()
}

fn import_artifact(home: &TestHome, bytes: &[u8]) -> Value {
    let path = home.root().join("artifact.json");
    std::fs::write(&path, bytes).unwrap();
    let output = run(
        home,
        &[&"artifact", &"import", &path.to_string_lossy().to_string()],
    );
    let envelope = last_envelope(&output.stdout);
    assert_eq!(envelope["ok"], true);
    envelope["data"].clone()
}

fn create_session(
    home: &TestHome,
    provider: &MockProviderProcess,
    generation_settings: &str,
    provider_timeout: u64,
) -> (String, String) {
    let cert_path = home.root().join("provider-test-ca.pem");
    let cert_path_str = cert_path.to_str().unwrap();
    let base_url = provider.provider_settings().base_url;
    let timeout = provider_timeout.to_string();

    let character = import_artifact(home, fixtures::character().as_bytes());
    let character_hash = character["revision_hash"].as_str().unwrap();
    let lorebook = import_artifact(home, fixtures::lorebook().as_bytes());
    let lorebook_hash = lorebook["revision_hash"].as_str().unwrap();
    let preset = import_artifact(home, fixtures::preset().as_bytes());
    let preset_hash = preset["revision_hash"].as_str().unwrap();

    let create = run(
        home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--lorebook",
            &lorebook_hash,
            &"--preset",
            &preset_hash,
            &"--compatibility-profile",
            &"sillytavern-1.18-core",
            &"--provider-base-url",
            &base_url,
            &"--provider-ca-certificate",
            &cert_path_str,
            &"--model",
            &"fixture-model",
            &"--provider-timeout",
            &timeout,
            &"--generation-settings",
            &generation_settings,
        ],
    );
    let create_data = last_envelope(&create.stdout);
    assert_eq!(create_data["ok"], true);
    let session_id = create_data["data"]["session"]["session_id"]
        .as_str()
        .unwrap();
    let branch_id = create_data["data"]["session"]["root_branch_id"]
        .as_str()
        .unwrap();
    (session_id.to_owned(), branch_id.to_owned())
}

#[tokio::test]
async fn cli_provider_http_429_returns_schema_conformant_error() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();

    let (session_id, _branch_id) =
        create_session(&home, &provider, r#"{"fixture_status": 429}"#, 5);
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    let envelope = last_envelope(&send.stderr);
    assert_eq!(envelope["ok"], false);
    assert!(envelope["error"].is_object());
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("429"),
        "error should mention 429: {message}"
    );
}

#[tokio::test]
async fn cli_provider_stream_missing_done_returns_schema_conformant_error() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();

    let (session_id, _branch_id) = create_session(
        &home,
        &provider,
        r#"{"fixture_sse_malformed": "missing-done"}"#,
        5,
    );
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    let envelope = last_envelope(&send.stderr);
    assert_eq!(envelope["ok"], false);
    let message = envelope["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("[DONE]") || message.contains("stream"),
        "error should mention stream/DONE: {message}"
    );
}

#[tokio::test]
async fn cli_provider_stream_bad_json_returns_schema_conformant_error() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();

    let (session_id, _branch_id) = create_session(
        &home,
        &provider,
        r#"{"fixture_sse_malformed": "bad-json"}"#,
        5,
    );
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    let envelope = last_envelope(&send.stderr);
    assert_eq!(envelope["ok"], false);
}

#[tokio::test]
async fn cli_provider_disconnect_after_chunks_returns_schema_conformant_error() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();

    let (session_id, _branch_id) = create_session(
        &home,
        &provider,
        r#"{"fixture_disconnect_after_chunks": 3}"#,
        5,
    );
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    let envelope = last_envelope(&send.stderr);
    assert_eq!(envelope["ok"], false);
}
