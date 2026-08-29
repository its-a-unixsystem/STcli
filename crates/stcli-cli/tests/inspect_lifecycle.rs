//! L3 binary end-to-end: inspect and lifecycle workflows not covered by the
//! main session-loop test — artifact listing, archive/purge with shared
//! revisions, prompt inspection, and dry-run protocol output.
//!
//! Implements acceptance criteria of issue #43.

use serde_json::Value;
use stcli_core::{CapsuleKind, EntityId, Store};
use stcli_testkit::{MockProviderProcess, TestHome};
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const CLI_ENVELOPE_SCHEMA: &str = include_str!("../../../schemas/cli-envelope.schema.json");

fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join(format!("../../examples/{name}"))
        .to_string_lossy()
        .into_owned()
}

fn stcli_cmd(home: &TestHome) -> Command {
    let mut command = Command::new(home.stcli_binary());
    command.env("STCLI_HOME", home.root());
    command.env("STCLI_REGEX_WORKER", home.stcli_binary());
    command
}

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

fn envelope(output: &Output) -> Value {
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = json_lines(&output.stdout);
    assert!(!lines.is_empty());
    let envelope = lines.pop().unwrap();
    assert_eq!(envelope["schema"], "stcli.cli/v1");
    assert_eq!(envelope["ok"], true);
    envelope
}

fn envelope_data(output: &Output) -> Value {
    envelope(output)["data"].clone()
}

fn error_envelope(output: &Output) -> Value {
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let lines = json_lines(&output.stderr);
    assert_eq!(lines.len(), 1);
    let envelope = &lines[0];
    assert_eq!(envelope["schema"], "stcli.cli/v1");
    assert_eq!(envelope["ok"], false);
    envelope.clone()
}

fn assert_valid_envelope(value: &Value) {
    let schema: Value = serde_json::from_str(CLI_ENVELOPE_SCHEMA).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(value)
        .unwrap();
}

fn projection_hash(database: &Path, attempt_id: EntityId) -> String {
    let store = Store::open(database).unwrap();
    store
        .export_turn_capsule(attempt_id, CapsuleKind::Thin, false)
        .unwrap()
        .result
        .projection_hash
        .unwrap()
        .to_string()
}

/// Import artifacts and exercise `artifact list`, `artifact show`, and
/// `artifact export` through the real binary, validating exit status and
/// parsed envelopes.
#[tokio::test(flavor = "multi_thread")]
async fn artifact_list_show_and_export_are_exercised_through_the_real_binary() {
    let home = TestHome::new().unwrap();

    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["revision_hash"].as_str().unwrap();
    assert_eq!(character["kind"], "character-card-v2");

    let lorebook = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("lorebook.json")],
    ));
    let lorebook_hash = lorebook["revision_hash"].as_str().unwrap();
    assert_eq!(lorebook["kind"], "lorebook");

    let preset = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("preset.json")],
    ));
    let preset_hash = preset["revision_hash"].as_str().unwrap();
    assert_eq!(preset["kind"], "chat-completion-preset");

    // artifact list
    let list = run(&home, &[&"artifact", &"list"]);
    let list_data = envelope_data(&list);
    let list_array = list_data.as_array().unwrap();
    assert_eq!(list_array.len(), 3);
    let hashes: Vec<&str> = list_array
        .iter()
        .map(|record| record["revision_hash"].as_str().unwrap())
        .collect();
    assert!(hashes.contains(&character_hash));
    assert!(hashes.contains(&lorebook_hash));
    assert!(hashes.contains(&preset_hash));

    // artifact show
    let show = run(&home, &[&"artifact", &"show", &character_hash]);
    let show_data = envelope_data(&show);
    assert_eq!(show_data["revision_hash"], character_hash);
    assert_eq!(show_data["kind"], "character-card-v2");
    assert!(show_data["semantic_hash"].is_string());
    assert!(show_data["source_blob_hash"].is_string());

    // artifact export — round-trips the original bytes
    let export_path = home.root().join("exported-character.json");
    let export = run(
        &home,
        &[
            &"artifact",
            &"export",
            &character_hash,
            &export_path.to_str().unwrap(),
        ],
    );
    let export_data = envelope_data(&export);
    assert_eq!(export_data["revision_hash"], character_hash);
    let exported_bytes = std::fs::read(&export_path).unwrap();
    let original_bytes = std::fs::read(example("character.json")).unwrap();
    assert_eq!(exported_bytes, original_bytes);

    // artifact show with a bogus hash fails with a non-zero exit and a valid error envelope
    let bad = run(
        &home,
        &[
            &"artifact",
            &"show",
            &"sha256:0000000000000000000000000000000000000000000000000000000000000000",
        ],
    );
    let bad_env = error_envelope(&bad);
    assert_valid_envelope(&bad_env);
    assert!(bad_env["error"]["message"].is_string());
}

/// Archive a session, then purge it, and verify that shared Artifact Revisions
/// survive when another session still references them.
#[tokio::test(flavor = "multi_thread")]
async fn archive_and_purge_preserve_shared_artifact_revisions() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let cert_path = home.root().join("provider-test-ca.pem");
    let cert_path_str = cert_path.to_str().unwrap();

    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["revision_hash"].as_str().unwrap();

    let lorebook = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("lorebook.json")],
    ));
    let lorebook_hash = lorebook["revision_hash"].as_str().unwrap();

    let base_url = provider.provider_settings().base_url.clone();

    // Create two sessions sharing the same character and lorebook.
    let create_a = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--lorebook",
            &lorebook_hash,
            &"--provider-base-url",
            &base_url,
            &"--provider-ca-certificate",
            &cert_path_str,
            &"--model",
            &"fixture-model",
        ],
    );
    let session_a = envelope_data(&create_a)["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let create_b = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--lorebook",
            &lorebook_hash,
            &"--provider-base-url",
            &base_url,
            &"--provider-ca-certificate",
            &cert_path_str,
            &"--model",
            &"fixture-model",
        ],
    );
    let session_b = envelope_data(&create_b)["session"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Send a turn on session A so it has trace events to purge.
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_a, &"Hello"],
    );
    assert!(send.status.success());

    // Archive session A.
    let archive = run(&home, &[&"session", &"archive", &session_a]);
    let archive_data = envelope_data(&archive);
    assert_eq!(archive_data["archived"], true);
    assert_eq!(archive_data["session_id"], session_a);

    // Archiving again is idempotent.
    let archive2 = run(&home, &[&"session", &"archive", &session_a]);
    assert_eq!(envelope_data(&archive2)["archived"], true);

    // session show reflects the archived flag.
    let show = run(&home, &[&"session", &"show", &session_a]);
    let show_data = envelope_data(&show);
    assert_eq!(show_data["session"]["archived"], true);

    // Purge session A.
    let purge = run(&home, &[&"session", &"purge", &session_a]);
    let purge_data = envelope_data(&purge);
    assert!(
        purge_data["removed_trace_events"].as_u64().is_some(),
        "purge should report removed trace events"
    );

    // Session A is gone from the list.
    let sessions = run(&home, &[&"session", &"list"]);
    let session_list = envelope_data(&sessions).as_array().unwrap().clone();
    assert!(
        !session_list
            .iter()
            .any(|record| record["session_id"] == session_a)
    );
    // Session B is still present.
    assert!(
        session_list
            .iter()
            .any(|record| record["session_id"] == session_b)
    );

    // Shared artifacts survive because session B still references them.
    let show_char = run(&home, &[&"artifact", &"show", &character_hash]);
    assert_eq!(envelope_data(&show_char)["revision_hash"], character_hash);
    let show_lore = run(&home, &[&"artifact", &"show", &lorebook_hash]);
    assert_eq!(envelope_data(&show_lore)["revision_hash"], lorebook_hash);

    // Session B can still send a turn using the shared artifact.
    let send_b = run(
        &home,
        &[&"message", &"send", &"--session", &session_b, &"Hi there"],
    );
    assert!(send_b.status.success());

    provider.shutdown();
}

/// Prompt inspection and Dry Run produce the documented protocol outputs with
/// structured fields, validated against the CLI envelope schema.
#[tokio::test(flavor = "multi_thread")]
async fn prompt_inspect_and_dry_run_produce_documented_protocol_output() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let cert_path = home.root().join("provider-test-ca.pem");
    let cert_path_str = cert_path.to_str().unwrap();
    let database = home.paths().database();

    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["revision_hash"].as_str().unwrap();

    let lorebook = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("lorebook.json")],
    ));
    let lorebook_hash = lorebook["revision_hash"].as_str().unwrap();

    let preset = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("preset.json")],
    ));
    let preset_hash = preset["revision_hash"].as_str().unwrap();

    let base_url = provider.provider_settings().base_url.clone();
    let create = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--lorebook",
            &lorebook_hash,
            &"--preset",
            &preset_hash,
            &"--provider-base-url",
            &base_url,
            &"--provider-ca-certificate",
            &cert_path_str,
            &"--model",
            &"fixture-model",
        ],
    );
    let created = envelope_data(&create);
    let session_id = created["session"]["session_id"].as_str().unwrap();
    let root_branch_id = created["session"]["root_branch_id"].as_str().unwrap();

    // Send a real turn so we have an attempt to inspect.
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    assert!(send.status.success());
    let send_env = envelope(&send);
    assert_valid_envelope(&send_env);
    let attempt_id = send_env["data"]["attempt"]["attempt_id"].as_str().unwrap();
    let attempt_id_parsed: EntityId = attempt_id.parse().unwrap();

    // --- prompt inspect ---
    let prompt = run(&home, &[&"prompt", &"inspect", &attempt_id]);
    let prompt_env = envelope(&prompt);
    assert_valid_envelope(&prompt_env);
    let prompt_data = &prompt_env["data"];

    // PromptPlan fields
    assert!(prompt_data["messages"].is_array(), "messages must be array");
    assert!(prompt_data["segments"].is_array(), "segments must be array");
    assert!(
        prompt_data["total_tokens"].is_u64(),
        "total_tokens must be present"
    );
    assert!(
        prompt_data["tokenizer"].is_string(),
        "tokenizer must be present"
    );
    assert!(
        prompt_data["pruning"].is_object(),
        "pruning must be an object"
    );
    assert!(
        prompt_data["pruning"]["context_limit"].is_u64(),
        "pruning.context_limit must be present"
    );
    assert!(
        prompt_data["pruning"]["response_reserve"].is_u64(),
        "pruning.response_reserve must be present"
    );
    assert!(
        prompt_data["pruning"]["kept_tokens"].is_u64(),
        "pruning.kept_tokens must be present"
    );

    // Each message has a role and content.
    for message in prompt_data["messages"].as_array().unwrap() {
        assert!(message["role"].is_string(), "each message must have a role");
        assert!(
            message["content"].is_string(),
            "each message must have content"
        );
    }

    // Each segment has source, slot, role, content, and token_count.
    for segment in prompt_data["segments"].as_array().unwrap() {
        assert!(segment["source"].is_string(), "segment must have source");
        assert!(segment["slot"].is_string(), "segment must have slot");
        assert!(segment["role"].is_string(), "segment must have role");
        assert!(
            segment["token_count"].is_u64(),
            "segment must have token_count"
        );
        assert!(
            segment["in_chat_order"].is_u64(),
            "segment must have in_chat_order"
        );
    }

    // Regression test for issue #61: prompt segments are inspectable by slot and index.
    let by_slot = run(
        &home,
        &[&"prompt", &"inspect", &attempt_id, &"--segment", &"main"],
    );
    let by_slot_env = envelope(&by_slot);
    assert_valid_envelope(&by_slot_env);
    let by_slot_data = &by_slot_env["data"];
    assert_eq!(by_slot_data["selector"], "main");
    let by_slot_segments = by_slot_data["segments"].as_array().unwrap();
    assert_eq!(by_slot_segments.len(), 1);
    let main_segment = &by_slot_segments[0];
    assert_eq!(main_segment["index"], 0);
    assert_eq!(main_segment["segment"]["slot"], "main");
    assert_eq!(main_segment["segment"]["source_revision"], preset_hash);
    assert!(
        main_segment["segment"]["raw_content"]
            .as_str()
            .unwrap()
            .contains("{{char}}")
    );
    assert!(
        main_segment["segment"]["content"]
            .as_str()
            .unwrap()
            .contains("Elspeth")
    );
    for field in [
        "source",
        "token_count",
        "in_chat_depth",
        "in_chat_order",
        "pruned",
        "truncation_priority",
    ] {
        assert!(
            main_segment["segment"].get(field).is_some(),
            "segment metadata must include {field}"
        );
    }
    assert!(
        !main_segment["transformations"]["macro_evaluations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(main_segment["transformations"]["regex_applications"].is_array());
    assert!(main_segment["transformations"]["state_mutations"].is_array());

    let human = stcli_cmd(&home)
        .args(["prompt", "inspect", attempt_id, "--segment", "main"])
        .output()
        .unwrap();
    assert!(
        human.status.success(),
        "human inspection failed: {}",
        String::from_utf8_lossy(&human.stderr)
    );
    let human_stdout = String::from_utf8(human.stdout).unwrap();
    for heading in [
        "Segment 0: main",
        "Raw authored content:",
        "Rendered content:",
        "Macro evaluations:",
        "Regex applications:",
        "State mutations:",
    ] {
        assert!(
            human_stdout.contains(heading),
            "human inspection must include {heading}"
        );
    }

    let by_index = envelope_data(&run(
        &home,
        &[&"prompt", &"inspect", &attempt_id, &"--segment", &"0"],
    ));
    assert_eq!(by_index["segments"], by_slot_data["segments"]);

    let missing_segment = run(
        &home,
        &[
            &"prompt",
            &"inspect",
            &attempt_id,
            &"--segment",
            &"missing-slot",
        ],
    );
    let missing_segment_error = error_envelope(&missing_segment);
    assert!(
        missing_segment_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("missing-slot")
    );

    let missing_attempt = EntityId::new().to_string();
    let missing_attempt_output = run(
        &home,
        &[
            &"prompt",
            &"inspect",
            &missing_attempt,
            &"--segment",
            &"main",
        ],
    );
    let missing_attempt_error = error_envelope(&missing_attempt_output);
    assert!(
        missing_attempt_error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("was not found")
    );

    // --- dry run (message send --dry-run) ---
    let dry_run = run(
        &home,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"--branch",
            &root_branch_id,
            &"--dry-run",
            &"Dry run text",
        ],
    );
    let dry_env = envelope(&dry_run);
    assert_valid_envelope(&dry_env);
    let dry_data = &dry_env["data"];

    // DryRunResult fields
    assert_eq!(dry_data["session_id"], session_id);
    assert_eq!(dry_data["branch_id"], root_branch_id);
    assert_eq!(dry_data["user_content"], "Dry run text");
    assert!(
        dry_data["prompt_plan"].is_object(),
        "dry run must include prompt_plan"
    );
    assert!(
        dry_data["provider_request"].is_object(),
        "dry run must include provider_request"
    );
    assert!(
        dry_data["provider_request"]["messages"].is_array(),
        "provider_request must have messages"
    );
    assert!(
        dry_data["effective_generation_settings"].is_object(),
        "dry run must include effective_generation_settings"
    );

    // Dry run must not create a turn or attempt.
    assert!(
        dry_data.get("turn").is_none() || dry_data["turn"].is_null(),
        "dry run must not produce a turn"
    );
    assert!(
        dry_data.get("attempt").is_none() || dry_data["attempt"].is_null(),
        "dry run must not produce an attempt"
    );

    // --- replay invariant: projection hash is stable across rebuild ---
    let database_path = database.to_path_buf();
    let hash_before = projection_hash(&database_path, attempt_id_parsed);
    let _ = run(&home, &[&"session", &"rebuild"]);
    let hash_after = projection_hash(&database_path, attempt_id_parsed);
    assert_eq!(
        hash_before, hash_after,
        "projection hash must be stable across rebuild"
    );

    provider.shutdown();
}
