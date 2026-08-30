use std::{
    io::{BufRead, BufReader},
    path::PathBuf,
    process::{Output, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde_json::Value;
use stcli_core::{EntityId, Store};
use stcli_testkit::{MockProviderProcess, TestHome, configuration, fixtures, stcli_cmd};

const CLI_ENVELOPE_SCHEMA: &str = include_str!("../../../schemas/cli-envelope.schema.json");
const CLI_EVENT_SCHEMA: &str = include_str!("../../../schemas/cli-event.schema.json");
const TURN_CAPSULE_SCHEMA: &str = include_str!("../../../schemas/turn-capsule.schema.json");
const PLUGIN_MANIFEST_SCHEMA: &str = include_str!("../../../schemas/plugin-manifest.schema.json");
const COMPAT_PROFILE_SCHEMA: &str = include_str!("../../../schemas/compat-profile.schema.json");
const FIXTURE_SUITE_SCHEMA: &str = include_str!("../../../schemas/fixture-suite.schema.json");

fn parse_schema(source: &str) -> Value {
    serde_json::from_str(source).unwrap()
}

fn assert_valid(schema: &Value, instance: &Value) {
    jsonschema::validator_for(schema)
        .unwrap()
        .validate(instance)
        .unwrap();
}

fn json_lines(output: &[u8]) -> Vec<Value> {
    String::from_utf8(output.to_vec())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn proof_manifest() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/proof/manifest.json")
}

#[tokio::test(flavor = "multi_thread")]
async fn reasoning_json_event_is_flushed_before_generation_completes() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let paths = home.paths();
    paths.ensure_exists().unwrap();
    let mut store = Store::open(paths.database()).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut session_configuration = configuration(character.revision_hash);
    session_configuration.provider = provider.provider_settings();
    session_configuration.provider.stream = true;
    session_configuration.provider.timeout_seconds = 15;
    session_configuration.generation_settings = serde_json::json!({
        "fixture_reasoning": "Planning response",
        "fixture_chunk_delay_ms": 5_000
    });
    let created = store.create_session(session_configuration, 0).unwrap();
    drop(store);

    let mut child = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "message",
            "send",
            "--session",
            &created.session.session_id.to_string(),
            "Hello",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (sender, receiver) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut first_events = Vec::new();
        for line in BufReader::new(stdout).lines() {
            let event = serde_json::from_str::<Value>(&line.unwrap()).unwrap();
            if first_events.len() < 2 {
                first_events.push(event);
                if first_events.len() == 2 {
                    sender.send(first_events.clone()).unwrap();
                }
            }
        }
    });

    // Regression test for coverage-instrumented startup: prove early flushing from process state,
    // not a one-second wall-clock deadline.
    let first_events = receiver
        .recv_timeout(Duration::from_secs(15))
        .expect("reasoning event should be emitted while generation is still running");
    assert_eq!(first_events[0]["event_type"], "provider.started");
    assert_eq!(first_events[1]["event_type"], "provider.reasoning-delta");
    assert!(
        child.try_wait().unwrap().is_none(),
        "generation completed before the reasoning event was observed"
    );
    child.kill().unwrap();
    child.wait().unwrap();
    reader.join().unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn real_cli_workflows_conform_to_public_protocol_schemas() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let paths = home.paths();
    paths.ensure_exists().unwrap();
    let mut store = Store::open(paths.database()).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut session_configuration = configuration(character.revision_hash);
    session_configuration.provider = provider.provider_settings();
    session_configuration.provider.stream = true;
    session_configuration.generation_settings =
        serde_json::json!({"fixture_reasoning": "Planning response"});
    let created = store.create_session(session_configuration, 0).unwrap();
    drop(store);

    let session_id = created.session.session_id.to_string();
    let output = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "message",
            "send",
            "--session",
            &session_id,
            "Hello",
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut lines = json_lines(&output.stdout);
    let success_envelope = lines.pop().unwrap();
    let envelope_schema = parse_schema(CLI_ENVELOPE_SCHEMA);
    assert_valid(&envelope_schema, &success_envelope);
    assert_eq!(success_envelope["ok"], true);

    let event_schema = parse_schema(CLI_EVENT_SCHEMA);
    for event in &lines {
        assert_valid(&event_schema, event);
    }
    let event_types = lines
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        event_types,
        [
            "provider.started",
            "provider.reasoning-delta",
            "provider.text-delta",
            "provider.text-delta",
            "provider.usage",
            "provider.completed",
        ]
    );
    assert_eq!(
        event_types
            .iter()
            .filter(|event_type| **event_type == "provider.completed")
            .count(),
        1
    );
    let reasoning_event = lines
        .iter()
        .find(|event| event["event_type"] == "provider.reasoning-delta")
        .unwrap();
    assert_eq!(
        reasoning_event["data"],
        serde_json::json!({
            "event_type": "reasoning-delta",
            "text": "Planning response"
        })
    );

    let attempt_id = success_envelope["data"]["attempt"]["attempt_id"]
        .as_str()
        .unwrap();
    let capsule_path = home.root().join("turn-capsule.json");
    let export = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "turn",
            "export",
            "--session",
            &session_id,
            attempt_id,
            "--file",
            capsule_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    assert_valid(&envelope_schema, &json_lines(&export.stdout)[0]);
    let capsule = serde_json::from_slice::<Value>(&std::fs::read(capsule_path).unwrap()).unwrap();
    assert_valid(&parse_schema(TURN_CAPSULE_SCHEMA), &capsule);

    let failure = stcli_cmd(&home)
        .args([
            "--output",
            "json",
            "session",
            "show",
            &EntityId::new().to_string(),
        ])
        .output()
        .unwrap();
    assert!(!failure.status.success());
    assert!(failure.stdout.is_empty());
    let failure_envelope = single_error_envelope(&failure);
    assert_valid(&envelope_schema, &failure_envelope);
    assert_eq!(failure_envelope["ok"], false);

    let manifest =
        serde_json::from_slice::<Value>(&std::fs::read(proof_manifest()).unwrap()).unwrap();
    assert_valid(&parse_schema(PLUGIN_MANIFEST_SCHEMA), &manifest);

    provider.shutdown();
}

fn single_error_envelope(output: &Output) -> Value {
    let lines = json_lines(&output.stderr);
    assert_eq!(lines.len(), 1);
    lines.into_iter().next().unwrap()
}

#[test]
fn public_schema_identifiers_are_stable() {
    let schemas = [
        (
            COMPAT_PROFILE_SCHEMA,
            "https://stcli.invalid/schemas/compat-profile.schema.json",
        ),
        (
            FIXTURE_SUITE_SCHEMA,
            "https://stcli.invalid/schemas/fixture-suite.schema.json",
        ),
        (
            CLI_ENVELOPE_SCHEMA,
            "https://stcli.invalid/schemas/cli-envelope.schema.json",
        ),
        (
            CLI_EVENT_SCHEMA,
            "https://stcli.invalid/schemas/cli-event.schema.json",
        ),
        (
            TURN_CAPSULE_SCHEMA,
            "https://stcli.invalid/schemas/turn-capsule.schema.json",
        ),
        (
            PLUGIN_MANIFEST_SCHEMA,
            "https://stcli.invalid/schemas/plugin-manifest.schema.json",
        ),
    ];

    for (source, expected_id) in schemas {
        assert_eq!(parse_schema(source)["$id"], expected_id);
    }
}
