//! Binary integration coverage for prompt diffing from issue #62.

use serde_json::Value;
use stcli_testkit::{MockProviderProcess, TestHome};
use std::{
    ffi::OsStr,
    path::PathBuf,
    process::{Command, Output},
};

const CLI_ENVELOPE_SCHEMA: &str = include_str!("../../../schemas/cli-envelope.schema.json");

fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../examples/{name}"))
        .to_string_lossy()
        .into_owned()
}

fn run(home: &TestHome, args: &[&dyn AsRef<OsStr>]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
    Command::new(home.stcli_binary())
        .env("STCLI_HOME", home.root())
        .env("STCLI_REGEX_WORKER", home.stcli_binary())
        .args([OsStr::new("--output"), OsStr::new("json")])
        .args(args)
        .output()
        .unwrap()
}

fn run_human(home: &TestHome, args: &[&dyn AsRef<OsStr>]) -> Output {
    let args: Vec<&OsStr> = args.iter().map(|arg| arg.as_ref()).collect();
    Command::new(home.stcli_binary())
        .env("STCLI_HOME", home.root())
        .env("STCLI_REGEX_WORKER", home.stcli_binary())
        .args(args)
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

fn envelope(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    json_lines(&output.stdout).pop().unwrap()
}

fn data(output: &Output) -> Value {
    envelope(output)["data"].clone()
}

fn error_envelope(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    json_lines(&output.stderr).pop().unwrap()
}

async fn session(home: &TestHome, provider: &MockProviderProcess) -> String {
    let character = data(&run(
        home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();
    let base_url = provider.provider_settings().base_url;
    let certificate = home
        .root()
        .join("provider-test-ca.pem")
        .to_string_lossy()
        .into_owned();
    let created = data(&run(
        home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--provider-base-url",
            &base_url,
            &"--provider-ca-certificate",
            &certificate,
            &"--model",
            &"fixture-model",
        ],
    ));
    created["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

fn send(home: &TestHome, session_id: &str, content: &str) -> Value {
    data(&run(
        home,
        &[&"message", &"send", &"--session", &session_id, &content],
    ))
}

#[tokio::test(flavor = "multi_thread")]
async fn arbitrary_attempts_return_a_structured_prompt_diff_envelope() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let session_id = session(&home, &provider).await;
    let first = send(&home, &session_id, "Hello");
    let second = send(&home, &session_id, "What changed?");
    let first_attempt = first["attempt"]["attempt_id"].as_str().unwrap();
    let second_attempt = second["attempt"]["attempt_id"].as_str().unwrap();

    let output = run(
        &home,
        &[&"prompt", &"diff", &first_attempt, &second_attempt],
    );
    let envelope = envelope(&output);
    let schema: Value = serde_json::from_str(CLI_ENVELOPE_SCHEMA).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&envelope)
        .unwrap();
    let diff = &envelope["data"];

    assert_eq!(diff["baseline_attempt_id"], first_attempt);
    assert_eq!(diff["target_attempt_id"], second_attempt);
    assert!(diff["segments"].as_array().unwrap().iter().any(|segment| {
        segment["changes"]
            .as_array()
            .is_some_and(|changes| changes.iter().any(|change| change == "added"))
    }));
    assert!(diff["segments"].as_array().unwrap().iter().any(|segment| {
        segment["text_diff"]["word"]
            .as_array()
            .is_some_and(|changes| changes.iter().any(|change| change["kind"] == "insert"))
    }));
    assert!(diff["token_delta"]["kept_tokens"].is_i64());
    assert!(diff["token_delta"]["pruned_tokens"].is_i64());
    assert!(diff["token_delta"]["total_tokens"].is_i64());
    let human = run_human(
        &home,
        &[&"prompt", &"diff", &first_attempt, &second_attempt],
    );
    assert!(human.status.success());
    let terminal = String::from_utf8(human.stdout).unwrap();
    assert!(terminal.contains("Prompt diff"));
    assert!(terminal.contains("tokens  kept"));
    assert!(terminal.contains("@@"));
    assert!(terminal.contains("\u{1b}[32m"));

    provider.shutdown();
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_prev_uses_the_previous_turns_selected_regeneration_attempt() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let session_id = session(&home, &provider).await;
    let first = send(&home, &session_id, "Hello");
    let first_attempt = first["attempt"]["attempt_id"].as_str().unwrap();
    let turn_id = first["turn"]["turn_id"].as_str().unwrap();

    let swipe = data(&run(&home, &[&"message", &"swipe", &turn_id]));
    let swipe_attempt = swipe["attempt"]["attempt_id"].as_str().unwrap();
    let regenerate = data(&run(&home, &[&"message", &"regenerate", &turn_id]));
    let regenerate_attempt = regenerate["attempt"]["attempt_id"].as_str().unwrap();

    let swipe_diff = data(&run(
        &home,
        &[&"prompt", &"diff", &first_attempt, &swipe_attempt],
    ));
    assert_eq!(swipe_diff["baseline_attempt_id"], first_attempt);
    assert_eq!(swipe_diff["target_attempt_id"], swipe_attempt);
    assert_eq!(swipe_diff["segments"], serde_json::json!([]));
    assert_eq!(
        swipe_diff["token_delta"],
        serde_json::json!({
            "kept_tokens": 0,
            "pruned_tokens": 0,
            "total_tokens": 0,
        })
    );

    let regeneration_diff = data(&run(
        &home,
        &[&"prompt", &"diff", &swipe_attempt, &regenerate_attempt],
    ));
    assert_eq!(regeneration_diff["baseline_attempt_id"], swipe_attempt);
    assert_eq!(regeneration_diff["target_attempt_id"], regenerate_attempt);
    assert_eq!(regeneration_diff["segments"], serde_json::json!([]));
    assert_eq!(
        regeneration_diff["token_delta"],
        serde_json::json!({
            "kept_tokens": 0,
            "pruned_tokens": 0,
            "total_tokens": 0,
        })
    );

    let second = send(&home, &session_id, "Continue");
    let second_attempt = second["attempt"]["attempt_id"].as_str().unwrap();
    let diff_prev = data(&run(
        &home,
        &[&"prompt", &"inspect", &second_attempt, &"--diff-prev"],
    ));
    assert_eq!(diff_prev["baseline_attempt_id"], regenerate_attempt);
    assert_eq!(diff_prev["target_attempt_id"], second_attempt);

    let initial_error = error_envelope(&run(
        &home,
        &[&"prompt", &"inspect", &first_attempt, &"--diff-prev"],
    ));
    assert_eq!(initial_error["ok"], false);
    assert!(
        initial_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("first Turn")
    );

    let missing_attempt = "01M0ZVXKJ3GN413FMVXVAGGT37";
    let missing_error = error_envelope(&run(
        &home,
        &[&"prompt", &"diff", &missing_attempt, &second_attempt],
    ));
    assert!(
        missing_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was not found")
    );

    provider.shutdown();
}
