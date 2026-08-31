//! L3 binary end-to-end: one complete roleplay through the real CLI.

use serde_json::Value;
use stcli_core::{CapsuleKind, EntityId, Store};
use stcli_testkit::{MockProviderProcess, TestHome};
use std::fs;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
    process::{Command, Output},
};

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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut lines = json_lines(&output.stdout);
    assert!(!lines.is_empty());
    let envelope = lines.pop().unwrap();
    assert_eq!(envelope["ok"], true);
    envelope
}

fn envelope_data(output: &Output) -> Value {
    envelope(output)["data"].clone()
}

fn assert_stream_events(output: &Output) {
    let lines = json_lines(&output.stdout);
    assert!(lines.len() >= 2);
    let last = lines.last().unwrap();
    assert_eq!(last["schema"], "stcli.cli/v1");
    assert_eq!(last["ok"], true);
    for event in &lines[..lines.len() - 1] {
        assert_eq!(event["schema"], "stcli.cli-event/v1");
    }
    let event_types: Vec<_> = lines[..lines.len() - 1]
        .iter()
        .map(|event| event["event_type"].as_str().unwrap())
        .collect();
    assert_eq!(event_types.first().unwrap(), &"provider.started");
    assert_eq!(event_types.last().unwrap(), &"provider.completed");
    assert_eq!(
        event_types
            .iter()
            .filter(|t| **t == "provider.completed")
            .count(),
        1
    );
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

#[tokio::test(flavor = "multi_thread")]
async fn complete_binary_session_loop_exercises_send_swipe_regenerate_continue_branch_replay() {
    let home = TestHome::new().unwrap();
    let provider = MockProviderProcess::spawn(&home).await.unwrap();
    let cert_path = home.root().join("provider-test-ca.pem");
    let cert_path_str = cert_path.to_str().unwrap();
    let database = home.paths().database();

    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();

    let lorebook = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("lorebook.json")],
    ));
    let lorebook_hash = lorebook["primary"]["revision_hash"].as_str().unwrap();

    let preset = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("preset.json")],
    ));
    let preset_hash = preset["primary"]["revision_hash"].as_str().unwrap();

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
    let database_path = database.to_path_buf();

    // 1. send
    let send = run(
        &home,
        &[&"message", &"send", &"--session", &session_id, &"Hello"],
    );
    assert_stream_events(&send);
    let send_data = envelope(&send)["data"].clone();
    let turn_id = send_data["turn"]["turn_id"].as_str().unwrap();
    let attempt_id = send_data["attempt"]["attempt_id"].as_str().unwrap();
    let attempt_id_parsed: EntityId = attempt_id.parse().unwrap();
    let first_candidate_id = send_data["candidate"]["candidate_id"].as_str().unwrap();
    let request_hash = send_data["attempt"]["provider_request_hash"]
        .as_str()
        .unwrap();
    let expected_content = format!("fixture-response:{request_hash}");
    assert_eq!(send_data["candidate"]["content"], expected_content);

    // 2. swipe
    let swipe = run(&home, &[&"message", &"swipe", &turn_id]);
    assert_stream_events(&swipe);
    let swipe_data = envelope(&swipe)["data"].clone();
    let second_candidate_id = swipe_data["candidate"]["candidate_id"].as_str().unwrap();
    assert_ne!(second_candidate_id, first_candidate_id);
    assert!(
        swipe_data["candidate"]["content"]
            .as_str()
            .unwrap()
            .starts_with("fixture-response:sha256:")
    );
    assert_eq!(
        swipe_data["turn"]["selected_candidate_id"],
        second_candidate_id
    );

    // 3. regenerate
    let regenerate = run(&home, &[&"message", &"regenerate", &turn_id]);
    assert_stream_events(&regenerate);
    let regenerate_data = envelope(&regenerate)["data"].clone();
    let third_candidate_id = regenerate_data["candidate"]["candidate_id"]
        .as_str()
        .unwrap();
    assert_ne!(third_candidate_id, second_candidate_id);
    assert_eq!(
        regenerate_data["turn"]["selected_candidate_id"],
        third_candidate_id
    );

    // 4. continue
    let cont = run(&home, &[&"message", &"continue", &turn_id]);
    assert_stream_events(&cont);
    let cont_data = envelope(&cont)["data"].clone();
    let continued_candidate_id = cont_data["candidate"]["candidate_id"].as_str().unwrap();
    assert_ne!(continued_candidate_id, third_candidate_id);
    assert_eq!(cont_data["candidate"]["origin"], "continued");
    let continued_content = cont_data["candidate"]["content"].as_str().unwrap();
    let third_content = regenerate_data["candidate"]["content"].as_str().unwrap();
    assert!(continued_content.starts_with(third_content));

    // 5. branch by editing the user turn
    let branch_send = run(
        &home,
        &[
            &"message",
            &"edit-user",
            &turn_id,
            &"Tell me about the archive",
        ],
    );
    assert_stream_events(&branch_send);
    let branch_data = envelope(&branch_send)["data"].clone();
    let new_branch_id = branch_data["turn"]["branch_id"].as_str().unwrap();
    let _new_turn_id = branch_data["turn"]["turn_id"].as_str().unwrap();
    assert_ne!(new_branch_id, root_branch_id);
    let branches = run(&home, &[&"session", &"branches", &session_id]);
    let branches_data = envelope_data(&branches).as_array().unwrap().clone();
    let new_branch = branches_data
        .iter()
        .find(|b| b["branch_id"] == new_branch_id)
        .cloned()
        .unwrap();
    assert_eq!(new_branch["parent_branch_id"], root_branch_id);
    assert_eq!(new_branch["forked_from_turn_id"], turn_id);

    // 6. send on branch
    let branch_send2 = run(
        &home,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"--branch",
            &new_branch_id,
            &"On the branch",
        ],
    );
    assert_stream_events(&branch_send2);
    let branch_send2_data = envelope(&branch_send2)["data"].clone();
    assert_eq!(branch_send2_data["turn"]["branch_id"], new_branch_id);
    assert!(
        branch_send2_data["candidate"]["content"]
            .as_str()
            .unwrap()
            .starts_with("fixture-response:sha256:")
    );

    // 7a. logical deletion: branch, candidate, turn
    let side_branch = run(
        &home,
        &[&"message", &"edit-user", &turn_id, &"Side branch text"],
    );
    assert_stream_events(&side_branch);
    let side_data = envelope(&side_branch)["data"].clone();
    let side_branch_id = side_data["turn"]["branch_id"].as_str().unwrap();
    let _side_turn_id = side_data["turn"]["turn_id"].as_str().unwrap();

    let side_send = run(
        &home,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"--branch",
            &side_branch_id,
            &"Side content",
        ],
    );
    assert_stream_events(&side_send);

    let _ = run(&home, &[&"branch", &"delete", &side_branch_id]);
    let branches_after = run(&home, &[&"session", &"branches", &session_id]);
    let branches_after_data = envelope_data(&branches_after);
    let branch_list = branches_after_data.as_array().unwrap();
    assert!(!branch_list.iter().any(|b| b["branch_id"] == side_branch_id));

    let throw = run(
        &home,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"--branch",
            &root_branch_id,
            &"Throw-away",
        ],
    );
    assert_stream_events(&throw);
    let throw_data = envelope(&throw)["data"].clone();
    let throw_turn_id = throw_data["turn"]["turn_id"].as_str().unwrap();
    let throw_candidate_id = throw_data["candidate"]["candidate_id"].as_str().unwrap();
    let throw_attempt_id = throw_data["attempt"]["attempt_id"].as_str().unwrap();

    let _ = run(&home, &[&"candidate", &"delete", &throw_candidate_id]);
    let _ = run(&home, &[&"turn", &"hide", &throw_turn_id]);
    let _ = run(&home, &[&"turn", &"delete", &throw_turn_id]);
    let _ = run(&home, &[&"session", &"rebuild"]);
    let turns_after = run(&home, &[&"message", &"turns", &root_branch_id]);

    let turns_after_data = envelope_data(&turns_after);
    let turn_list = turns_after_data.as_array().unwrap();
    assert!(
        !turn_list
            .iter()
            .any(|entry| entry["turn"]["turn_id"] == throw_turn_id)
    );

    // the throw-away attempt is still in the turn trace
    let store = Store::open(&database_path).unwrap();
    assert!(
        store
            .attempt(throw_attempt_id.parse().unwrap())
            .unwrap()
            .is_some()
    );

    // 7. compact
    let compact = run(&home, &[&"session", &"compact", &session_id]);
    let compact_data = envelope_data(&compact);
    assert!(compact_data["removed"].is_object() && compact_data["preserved"].is_object());

    // projection hash before export
    let projection_before_export = projection_hash(&database_path, attempt_id_parsed);

    // 8. export portable capsule from the first turn's attempt
    let capsule_path = home.root().join("exported-capsule.json");
    let capsule_path_str = capsule_path.to_str().unwrap();
    let export = run(
        &home,
        &[
            &"turn",
            &"export",
            &"--session",
            &session_id,
            &attempt_id,
            &"--file",
            &capsule_path_str,
        ],
    );
    let export_data = envelope_data(&export);
    assert_eq!(export_data["kind"], "portable");
    assert!(
        export_data["capabilities"]["replay"]
            .as_bool()
            .unwrap_or(false)
    );

    // 9. replay offline
    let replay = run(&home, &[&"turn", &"replay", &capsule_path_str]);
    let replay_data = envelope_data(&replay);
    assert_eq!(replay_data["projection_hash"], projection_before_export);

    // 10. rebuild and confirm projection unchanged
    let _ = run(&home, &[&"session", &"rebuild"]);
    assert_eq!(
        projection_hash(&database_path, attempt_id_parsed),
        projection_before_export
    );

    // 11. prompt inspect and dry-run on the last turn are protocol-conformant
    let dry_run = run(
        &home,
        &[
            &"message",
            &"send",
            &"--session",
            &session_id,
            &"--branch",
            &new_branch_id,
            &"--dry-run",
            &"Dry run text",
        ],
    );
    let dry_run_data = envelope_data(&dry_run);
    assert!(dry_run_data["prompt_plan"].is_object());

    let prompt_attempt = branch_send2_data["attempt"]["attempt_id"].as_str().unwrap();
    let prompt = run(&home, &[&"prompt", &"inspect", &prompt_attempt]);
    let prompt_data = envelope_data(&prompt);
    assert!(prompt_data["messages"].is_array());

    // 12. archive / purge lifecycle with shared revisions
    let archive = run(&home, &[&"session", &"archive", &session_id]);
    let archive_data = envelope_data(&archive);
    assert_eq!(archive_data["archived"], true);

    let purge = run(&home, &[&"session", &"purge", &session_id]);
    let purge_data = envelope_data(&purge);
    assert!(purge_data["removed_trace_events"].as_u64().is_some());

    // purged session is gone
    let sessions = run(&home, &[&"session", &"list"]);
    let session_data = envelope_data(&sessions).as_array().unwrap().clone();
    assert!(
        !session_data
            .iter()
            .any(|record| record["session_id"] == session_id)
    );

    provider.shutdown();
}

#[test]
fn session_duplicate_returns_new_session_metadata() {
    let home = TestHome::new().unwrap();
    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();
    let source = envelope_data(&run(
        &home,
        &[&"session", &"create", &"--character", &character_hash],
    ));
    let source_session_id = source["session"]["session_id"].as_str().unwrap();

    let duplicate = envelope(&run(
        &home,
        &[
            &"session",
            &"duplicate",
            &source_session_id,
            &"--name",
            &"Test duplicate",
        ],
    ));

    assert_eq!(duplicate["command"], "session.duplicate");
    assert_ne!(
        duplicate["data"]["session"]["session_id"],
        source["session"]["session_id"]
    );
    assert_eq!(
        duplicate["data"]["session"]["custom_name"],
        "Test duplicate"
    );
}

#[test]
fn session_duplicate_returns_error_envelope_for_missing_session() {
    let home = TestHome::new().unwrap();
    let missing_session_id = EntityId::new().to_string();

    let output = run(&home, &[&"session", &"duplicate", &missing_session_id]);

    assert!(!output.status.success());
    let error = json_lines(&output.stderr).pop().unwrap();
    assert_eq!(error["ok"], false);
    assert_eq!(error["command"], "session.duplicate");
    assert_eq!(error["error"]["code"], "command_failed");
    assert_eq!(
        error["error"]["message"],
        format!("session {missing_session_id} was not found")
    );
}

#[test]
fn session_commands_accept_inline_and_file_persona_descriptions() {
    let home = TestHome::new().unwrap();
    let character = envelope_data(&run(
        &home,
        &[&"artifact", &"import", &example("character.json")],
    ));
    let character_hash = character["primary"]["revision_hash"].as_str().unwrap();
    let create = run(
        &home,
        &[
            &"session",
            &"create",
            &"--character",
            &character_hash,
            &"--persona-description",
            &"Inline persona",
        ],
    );
    let created = envelope_data(&create);
    let session_id = created["session"]["session_id"].as_str().unwrap();
    let persona_path = home.root().join("persona.txt");
    fs::write(&persona_path, "{{user}} knows {{char}}.").unwrap();
    let persona_argument = format!("@{}", persona_path.display());
    let update = run(
        &home,
        &[
            &"session",
            &"update",
            &session_id,
            &"--character",
            &character_hash,
            &"--persona",
            &"Morgan",
            &"--persona-description",
            &persona_argument,
        ],
    );
    envelope(&update);

    let store = Store::open(home.paths().database()).unwrap();
    let session = store.session(session_id.parse().unwrap()).unwrap().unwrap();
    let configuration = store
        .configuration(&session.current_config_hash)
        .unwrap()
        .unwrap()
        .configuration;
    assert_eq!(
        configuration.persona_description.as_deref(),
        Some("{{user}} knows {{char}}.")
    );

    let missing = run(
        &home,
        &[
            &"session",
            &"update",
            &session_id,
            &"--character",
            &character_hash,
            &"--persona-description",
            &"@missing-persona.txt",
        ],
    );
    assert!(!missing.status.success());
    assert!(
        String::from_utf8_lossy(&missing.stderr)
            .contains("failed to read persona description file 'missing-persona.txt'")
    );
}
