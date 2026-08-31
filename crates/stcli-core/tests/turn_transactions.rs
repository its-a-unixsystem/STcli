use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::{fs, io::Write as _};

use flate2::{Compression, write::ZlibEncoder};

use serde_json::json;
use stcli_core::{
    AttemptStatus, CapsuleKind, ContentHash, EntityId, HeaderSetting, OpenAiProvider,
    ProviderError, Store, TurnError,
};
use stcli_testkit::{EnvironmentGuard, configuration, fixtures};
use tempfile::tempdir;

fn append_png_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(kind);
    hasher.update(data);
    png.extend_from_slice(&hasher.finalize().to_be_bytes());
}

fn png_card() -> Vec<u8> {
    let mut metadata = b"chara\0".to_vec();
    metadata.extend_from_slice(STANDARD.encode(fixtures::minimal_card()).as_bytes());
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    append_png_chunk(&mut png, b"IHDR", &[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    append_png_chunk(&mut png, b"tEXt", &metadata);
    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&[0, 0, 0, 0, 0]).unwrap();
    append_png_chunk(&mut png, b"IDAT", &encoder.finish().unwrap());
    append_png_chunk(&mut png, b"IEND", &[]);
    png
}

#[test]
fn dry_run_builds_prompt_without_trace_or_turn() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let event_count = store.trace_events(None).unwrap().len();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "{{setvar::mood::happy}}Hello",
        )
        .unwrap();

    assert_eq!(store.trace_events(None).unwrap().len(), event_count);
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        dry_run.prompt_plan.messages.last().unwrap().content,
        "Hello"
    );
    assert_eq!(dry_run.prompt_plan.state_mutations.len(), 1);
    assert!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(stcli_core::VariableScope::Local, "mood")
            .is_none()
    );
    assert!(dry_run.prompt_plan.total_tokens > 0);
}

#[test]
fn dry_run_activates_recursive_lore_and_only_applies_activated_macro_overlays() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let lorebook = store
        .import_artifact(
            br#"{
                "entries": {
                    "archive": {
                        "key": ["library"],
                        "content": "{{setvar::found::yes}}A sealed archive",
                        "constant": false,
                        "order": 100
                    },
                    "archivist": {
                        "key": ["archive"],
                        "content": "The archivist is Mira",
                        "constant": false,
                        "order": 90
                    },
                    "disabled": {
                        "key": [],
                        "content": "{{setvar::bad::yes}}",
                        "constant": true,
                        "disable": true,
                        "order": 80
                    }
                }
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.lorebook_revisions.push(lorebook.revision_hash);
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Search the library",
        )
        .unwrap();

    assert_eq!(dry_run.prompt_plan.lore.activated.len(), 2);
    assert!(
        dry_run
            .prompt_plan
            .segments
            .iter()
            .any(|segment| segment.source == "world-info-after"
                && segment.content.contains("A sealed archive")
                && segment.content.contains("The archivist is Mira"))
    );
    assert!(
        dry_run
            .prompt_plan
            .state_mutations
            .iter()
            .any(|mutation| mutation.key.name == "found")
    );
    assert!(
        !dry_run
            .prompt_plan
            .state_mutations
            .iter()
            .any(|mutation| mutation.key.name == "bad")
    );
}

#[test]
fn dry_run_applies_prompt_preset_order_custom_macros_and_in_chat_depth() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "prompts": [
                    {"identifier": "main", "role": "system", "content": "Main for {{char}}"},
                    {"identifier": "custom", "role": "system", "content": "Custom for {{user}}"},
                    {"identifier": "depth", "role": "user", "content": "Depth note", "injection_position": 1, "injection_depth": 1}
                ],
                "prompt_order": [{"order": [
                    {"identifier": "main", "enabled": true},
                    {"identifier": "custom", "enabled": true},
                    {"identifier": "charDescription", "enabled": true},
                    {"identifier": "chatHistory", "enabled": true},
                    {"identifier": "depth", "enabled": true},
                    {"identifier": "userInput", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash);
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    let contents = dry_run
        .prompt_plan
        .segments
        .iter()
        .map(|segment| segment.content.as_str())
        .collect::<Vec<_>>();

    assert_eq!(contents.first(), Some(&"Main for Alice"));
    assert_eq!(contents.get(1), Some(&"Custom for User"));
    assert_eq!(contents.get(contents.len() - 2), Some(&"Depth note"));
    assert_eq!(contents.last(), Some(&"Hello"));
    assert_eq!(
        dry_run
            .prompt_plan
            .segments
            .iter()
            .find(|segment| segment.content == "Depth note")
            .unwrap()
            .in_chat_depth,
        Some(1)
    );
    assert!(
        dry_run
            .compatibility_warnings
            .iter()
            .any(|warning| warning.code == "prompt-order-profile-fallback")
    );
}

#[tokio::test]
async fn configuration_revisions_and_greeting_changes_preserve_history() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let initial = configuration(character.revision_hash);
    let created = store.create_session(initial.clone(), 0).unwrap();

    let selected = store
        .select_greeting(created.session.session_id, created.branch.branch_id, 1)
        .unwrap();
    assert_eq!(selected.branch_id, created.branch.branch_id);
    assert_eq!(selected.greeting, "Hello again.");

    let mut updated = initial;
    updated.persona_name = "Updated User".to_owned();
    let revision = store
        .update_session_configuration(created.session.session_id, updated)
        .unwrap();
    assert_ne!(revision.revision_hash, created.configuration.revision_hash);
    assert!(
        store
            .configuration(&created.configuration.revision_hash)
            .unwrap()
            .is_some()
    );

    store
        .send_message(
            created.session.session_id,
            selected.branch_id,
            "A recorded failed action".to_owned(),
            |_| {},
        )
        .await
        .unwrap_err();
    let branched = store
        .select_greeting(created.session.session_id, selected.branch_id, 0)
        .unwrap();
    assert_ne!(branched.branch_id, selected.branch_id);
    assert_eq!(
        branched.parent_branch_id,
        Some(created.session.root_branch_id)
    );
    assert_eq!(store.turns_for_branch(selected.branch_id).unwrap().len(), 1);

    store.rebuild_session_projections().unwrap();
    assert_eq!(
        store
            .session(created.session.session_id)
            .unwrap()
            .unwrap()
            .current_config_hash,
        revision.revision_hash
    );
    assert_eq!(
        store.branch(selected.branch_id).unwrap().unwrap().greeting,
        "Hello again."
    );
}

#[tokio::test]
async fn provider_setup_failure_leaves_failed_attempt_without_candidate() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
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
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::HttpsRequired(_))
    ));

    let turns = store.turns_for_branch(created.branch.branch_id).unwrap();
    assert_eq!(turns.len(), 1);
    assert!(turns[0].selected_candidate_id.is_none());
    let attempts = store.attempts_for_turn(turns[0].turn_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, AttemptStatus::Failed);
    assert!(attempts[0].error_message.is_some());
}
#[tokio::test]
async fn capsules_replay_offline_import_isolated_and_recalculate_redaction_capabilities() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("source.sqlite3")).unwrap();
    // Regression test: portable capsules must preserve binary PNG artifact sources.
    let character = store.import_artifact(&png_card()).unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Explain the archive".to_owned(),
            |_| {},
        )
        .await
        .unwrap_err();
    let turn = store
        .turns_for_branch(created.branch.branch_id)
        .unwrap()
        .pop()
        .unwrap();
    let attempt = store
        .attempts_for_turn(turn.turn_id)
        .unwrap()
        .pop()
        .unwrap();

    let portable = store
        .export_turn_capsule(attempt.attempt_id, CapsuleKind::Portable, false)
        .unwrap();
    assert!(
        portable
            .artifacts
            .iter()
            .all(|artifact| artifact.source.is_some())
    );
    let schema =
        serde_json::from_str(include_str!("../../../schemas/turn-capsule.schema.json")).unwrap();
    jsonschema::validator_for(&schema)
        .unwrap()
        .validate(&serde_json::to_value(&portable).unwrap())
        .unwrap();
    assert!(portable.capabilities.replay);
    let replay = store.replay_turn_capsule(&portable).unwrap();
    assert_eq!(replay.provider_calls, 0);
    assert_eq!(replay.plugin_executions, 0);

    let import_directory = tempdir().unwrap();
    let mut imported_store = Store::open(import_directory.path().join("imported.sqlite3")).unwrap();
    let imported = imported_store.import_turn_capsule(&portable).unwrap();
    assert_ne!(imported.session_id, created.session.session_id);
    assert_eq!(
        imported_store
            .attempt(imported.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Failed
    );
    assert_eq!(
        imported_store
            .import_turn_capsule(&portable)
            .unwrap()
            .session_id,
        imported.session_id
    );
    imported_store.rebuild_session_projections().unwrap();
    assert!(
        imported_store
            .session(imported.session_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        imported_store
            .attempt(imported.attempt_id)
            .unwrap()
            .unwrap()
            .status,
        AttemptStatus::Failed
    );

    let thin = store
        .export_turn_capsule(attempt.attempt_id, CapsuleKind::Thin, false)
        .unwrap();
    assert!(
        thin.artifacts
            .iter()
            .all(|artifact| artifact.source.is_none())
    );
    assert!(thin.capabilities.replay);

    let redacted = store
        .export_turn_capsule(attempt.attempt_id, CapsuleKind::Portable, true)
        .unwrap();
    assert!(redacted.capabilities.inspect);
    assert!(!redacted.capabilities.replay);
    assert!(!redacted.capabilities.rerun);
    assert!(store.replay_turn_capsule(&redacted).is_err());
}

#[test]
fn archive_and_purge_preserve_shared_artifact_revisions() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let first = store
        .create_session(configuration(character.revision_hash.clone()), 0)
        .unwrap();
    let second = store
        .create_session(configuration(character.revision_hash.clone()), 0)
        .unwrap();

    assert!(
        store
            .archive_session(first.session.session_id)
            .unwrap()
            .archived
    );
    assert!(
        !store
            .trace_events(Some(first.session.session_id))
            .unwrap()
            .is_empty()
    );
    assert!(store.purge_session(first.session.session_id).unwrap() > 0);
    assert!(store.session(first.session.session_id).unwrap().is_none());
    assert!(store.session(second.session.session_id).unwrap().is_some());
    assert!(store.artifact(&character.revision_hash).unwrap().is_some());
    assert_eq!(
        store.export_artifact(&character.revision_hash).unwrap(),
        fixtures::minimal_card().as_bytes()
    );
}

#[tokio::test]
async fn explicit_retry_links_a_new_failed_attempt() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    store
        .send_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello".to_owned(),
            |_| {},
        )
        .await
        .unwrap_err();
    let turn = store
        .turns_for_branch(created.branch.branch_id)
        .unwrap()
        .remove(0);
    let first = store.attempts_for_turn(turn.turn_id).unwrap().remove(0);

    store
        .retry_turn(turn.turn_id, first.attempt_id, |_| {})
        .await
        .unwrap_err();
    let attempts = store.attempts_for_turn(turn.turn_id).unwrap();
    assert_eq!(attempts.len(), 2);
    assert_eq!(attempts[1].retry_of_attempt_id, Some(first.attempt_id));
    assert_eq!(attempts[1].status, AttemptStatus::Failed);
}

#[test]
fn resolved_provider_secrets_never_enter_sqlite_trace_or_cli_data() {
    let mut environment = EnvironmentGuard::new();
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let secret = "phase2-super-secret-value";
    environment.set("STCLI_TEST_API_KEY", secret);
    environment.set("STCLI_TEST_HEADER", secret);

    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut configuration = configuration(character.revision_hash);
    configuration.provider.base_url = "https://example.invalid".to_owned();
    configuration.provider.api_key_env = Some("STCLI_TEST_API_KEY".to_owned());
    configuration.provider.static_headers.insert(
        "x-api-key".to_owned(),
        HeaderSetting::Environment("STCLI_TEST_HEADER".to_owned()),
    );
    let created = store.create_session(configuration, 0).unwrap();
    OpenAiProvider::new(created.configuration.configuration.provider.clone()).unwrap();
    let cli_json = serde_json::to_vec(&created).unwrap();
    let trace_json = serde_json::to_vec(&store.trace_events(None).unwrap()).unwrap();
    drop(store);
    let sqlite = fs::read(database).unwrap();

    assert!(!String::from_utf8_lossy(&cli_json).contains(secret));
    assert!(!String::from_utf8_lossy(&trace_json).contains(secret));
    assert!(!String::from_utf8_lossy(&sqlite).contains(secret));
    assert!(String::from_utf8_lossy(&cli_json).contains("STCLI_TEST_API_KEY"));
    assert!(String::from_utf8_lossy(&cli_json).contains("STCLI_TEST_HEADER"));
}

#[test]
fn dry_run_resolves_preset_settings_and_applies_assembly_behavior() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "temperature": 0.8,
                "top_p": 0.97,
                "openai_max_tokens": 256,
                "openai_max_context": 4096,
                "squash_system_messages": true,
                "use_sysprompt": true,
                "continue_prefill": true,
                "assistant_prefill": "Ready",
                "prompts": [
                    {"identifier": "first", "role": "system", "content": "First"},
                    {"identifier": "second", "role": "system", "content": "Second"},
                    {"identifier": "chatHistory", "role": "system", "content": ""}
                ],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "first", "enabled": true},
                    {"identifier": "second", "enabled": true},
                    {"identifier": "chatHistory", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash);
    config.generation_settings =
        json!({"temperature": 0.2, "continue_nudge_prompt": "Session nudge"});
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();

    assert_eq!(
        dry_run.effective_generation_settings.values["temperature"],
        0.2
    );
    assert_eq!(
        dry_run.effective_generation_settings.provenance["temperature"],
        stcli_core::GenerationSettingSource::Session
    );
    assert_eq!(dry_run.effective_generation_settings.values["top_p"], 0.97);
    assert_eq!(
        dry_run.effective_generation_settings.provenance["top_p"],
        stcli_core::GenerationSettingSource::Preset
    );
    assert_eq!(
        dry_run.effective_generation_settings.values["continue_nudge_prompt"],
        "Session nudge"
    );
    assert!(
        dry_run
            .provider_request
            .get("continue_nudge_prompt")
            .is_none()
    );
    assert_eq!(dry_run.provider_request["max_tokens"], 256);
    assert!(
        dry_run
            .provider_request
            .get("squash_system_messages")
            .is_none()
    );
    assert!(dry_run.provider_request.get("openai_max_context").is_none());
    assert_eq!(
        dry_run.provider_request["messages"],
        json!([
            {"role": "system", "content": "First\nSecond"},
            {"role": "assistant", "content": "Welcome."},
            {"role": "user", "content": "Hello"},
            {"role": "assistant", "content": "Ready"}
        ])
    );
    assert_eq!(dry_run.prompt_plan.pruning.context_limit, 4096);
    assert_eq!(dry_run.prompt_plan.pruning.response_reserve, 256);
}

#[test]
fn dry_run_preserves_sequential_macro_effects_and_absolute_injections() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "prompts": [
                    {"identifier": "init", "role": "system", "content": "{{setvar::mood::bright}}"},
                    {"identifier": "context", "role": "system", "content": "{{description}}|{{personality}}|{{scenario}}|{{group}}|{{groupNotMuted}}|{{summary}}|{{getvar::mood}}|{{ExtensionValue}}"},
                    {"identifier": "chatHistory", "role": "system", "content": ""},
                    {"identifier": "late-a", "role": "user", "content": "A", "injection_position": 1, "injection_depth": 0, "injection_order": 10},
                    {"identifier": "late-b", "role": "assistant", "content": "B", "injection_position": 1, "injection_depth": 0, "injection_order": 20},
                    {"identifier": "deep", "role": "system", "content": "Deep", "injection_position": 1, "injection_depth": 99, "injection_order": 0}
                ],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "init", "enabled": true},
                    {"identifier": "context", "enabled": true},
                    {"identifier": "chatHistory", "enabled": true},
                    {"identifier": "late-b", "enabled": true},
                    {"identifier": "deep", "enabled": true},
                    {"identifier": "late-a", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash);
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    let messages = &dry_run.provider_request["messages"];

    assert_eq!(messages[0], json!({"role": "system", "content": "Deep"}));
    assert_eq!(
        messages[1],
        json!({"role": "system", "content": "A librarian.|Curious|An old library||||bright|{{ExtensionValue}}"})
    );
    assert_eq!(
        messages.as_array().unwrap()[messages.as_array().unwrap().len() - 2],
        json!({"role": "user", "content": "A"})
    );
    assert_eq!(
        messages.as_array().unwrap().last().unwrap(),
        &json!({"role": "assistant", "content": "B"})
    );
    assert_eq!(dry_run.prompt_plan.state_mutations.len(), 1);
    assert!(
        store
            .state_transaction(created.session.session_id)
            .unwrap()
            .get(stcli_core::VariableScope::Local, "mood")
            .is_none()
    );
}

#[test]
fn dry_run_reports_unexecuted_preset_transformations() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "extensions": {
                    "regex_scripts": [{
                        "id": "cleanup",
                        "scriptName": "Cleanup",
                        "findRegex": "secret",
                        "replaceString": "safe",
                        "disabled": false,
                        "placement": [1, 2]
                    }]
                },
                "prompts": [{
                    "identifier": "directive",
                    "role": "system",
                    "content": "<!-- NEMO:activate alpha -->\n{{// @mutual-exclusive-group style }}\nUnchanged"
                }],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "directive", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash.clone());
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    let codes = dry_run
        .compatibility_warnings
        .iter()
        .map(|warning| warning.code.as_str())
        .collect::<Vec<_>>();

    assert!(codes.contains(&"preset-scripts-not-authorized"));
    assert!(!codes.contains(&"preset-scripts-placement-unsupported"));
    // Regression test for issue 06: vendor directives are preserved, never evaluated by core.
    assert!(!codes.contains(&"prompt-directives-not-evaluated"));
    assert_eq!(dry_run.preset_transformations.len(), 1);
    assert!(dry_run.preset_transformations[0].enabled);
    assert!(!dry_run.preset_transformations[0].granted);
    assert_eq!(dry_run.preset_transformations[0].placement, json!([1, 2]));
    // An ungranted script is neither executed nor reflected in the prompt.
    assert!(dry_run.prompt_plan.regex_applications.is_empty());
    let transformed = stcli_core::transform_preset_content(
        "sillytavern-1.18-core",
        &preset.revision_hash,
        &store
            .decoded_artifact(&preset.revision_hash)
            .unwrap()
            .semantic,
        &[dry_run.preset_transformations[0].digest.clone()],
    );
    assert_eq!(
        transformed.content,
        store
            .decoded_artifact(&preset.revision_hash)
            .unwrap()
            .semantic
    );
    assert!(
        !transformed
            .warnings
            .iter()
            .any(|warning| warning.code == "preset-scripts-not-authorized")
    );
    let content = dry_run.provider_request["messages"][0]["content"]
        .as_str()
        .unwrap();
    assert!(content.contains("NEMO:activate"));
}

#[test]
fn disabled_structural_marker_warns_without_blocking_prompt_assembly() {
    // Regression test for issue 08: marker risk is diagnosed from effective pinned state.
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "prompts": [{
                    "identifier": "structural",
                    "role": "system",
                    "content": "STRUCTURAL CONTENT",
                    "marker": true
                }],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "structural", "enabled": false}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash);
    let created = store.create_session(config, 0).unwrap();

    let disabled = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    let warning = disabled
        .compatibility_warnings
        .iter()
        .find(|warning| warning.code == "structural-prompt-marker-disabled")
        .unwrap();
    assert_eq!(warning.affected_identifiers, vec!["structural"]);
    assert!(warning.non_blocking);
    assert!(
        !disabled
            .provider_request
            .to_string()
            .contains("STRUCTURAL CONTENT")
    );

    let session = store.session(created.session.session_id).unwrap().unwrap();
    let mut config = store
        .configuration(&session.current_config_hash)
        .unwrap()
        .unwrap()
        .configuration;
    config
        .prompt_order_overrides
        .insert("structural".to_owned(), true);
    store
        .update_session_configuration(created.session.session_id, config)
        .unwrap();
    let enabled = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "Hello",
        )
        .unwrap();
    assert!(
        enabled
            .compatibility_warnings
            .iter()
            .all(|warning| warning.code != "structural-prompt-marker-disabled")
    );
    assert!(
        enabled
            .provider_request
            .to_string()
            .contains("STRUCTURAL CONTENT")
    );
}

/// Import a preset carrying a single regex script and return its grant digest.
fn preset_with_regex_script(
    store: &mut Store,
    script: serde_json::Value,
) -> (ContentHash, ContentHash) {
    let source = json!({
        "extensions": {"regex_scripts": [script]},
        "prompts": [{"identifier": "chatHistory", "role": "system", "content": ""}],
        "prompt_order": [{"character_id": 100001, "order": [
            {"identifier": "chatHistory", "enabled": true}
        ]}]
    });
    let preset = store
        .import_artifact(source.to_string().as_bytes())
        .unwrap();
    let semantic = store
        .decoded_artifact(&preset.revision_hash)
        .unwrap()
        .semantic;
    let transformed = stcli_core::transform_preset_content(
        "sillytavern-1.18-core",
        &preset.revision_hash,
        &semantic,
        &[],
    );
    let digest: ContentHash = transformed.scripts[0].digest.clone();
    (preset.revision_hash, digest)
}

// Tests that need the isolated worker to actually run live in
// `crates/stcli-cli/tests/regex_scripts.rs`, where the real `stcli` binary is
// available to service the `internal regex-replace-worker` subcommand. Here we
// only cover the ungranted path, which never spawns a worker.

#[test]
fn ungranted_script_leaves_the_prompt_unchanged() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let (preset_revision, _digest) = preset_with_regex_script(
        &mut store,
        json!({
            "id": "greet",
            "scriptName": "Greet",
            "findRegex": "/Welcome\\./g",
            "replaceString": "Greetings",
            "placement": [2]
        }),
    );
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset_revision);
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(created.session.session_id, created.branch.branch_id, "hi")
        .unwrap();

    let messages = dry_run.provider_request["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("the greeting assistant message");
    assert_eq!(assistant["content"], json!("Welcome."));
    assert!(dry_run.prompt_plan.regex_applications.is_empty());
    assert!(
        dry_run
            .compatibility_warnings
            .iter()
            .any(|warning| warning.code == "preset-scripts-not-authorized")
    );
}

#[test]
fn prompt_budget_matches_final_squashed_messages() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "openai_max_context": 4096,
                "openai_max_tokens": 128,
                "squash_system_messages": true,
                "prompts": [
                    {"identifier": "a", "role": "system", "content": "one"},
                    {"identifier": "b", "role": "system", "content": "two"}
                ],
                "prompt_order": [{"character_id": 100001, "order": [
                    {"identifier": "a", "enabled": true},
                    {"identifier": "b", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset.revision_hash);
    let created = store.create_session(config, 0).unwrap();

    let dry_run = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "ignored",
        )
        .unwrap();
    let submitted = dry_run.provider_request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|message| message["content"].as_str().unwrap())
        .collect::<Vec<_>>()
        .join("");

    assert_eq!(
        dry_run.prompt_plan.pruning.kept_tokens,
        dry_run.prompt_plan.tokenizer.count(&submitted)
    );
}

async fn create_failed_turn(
    store: &mut Store,
    session_id: EntityId,
    branch_id: EntityId,
    content: &str,
) -> stcli_core::TurnProjection {
    store
        .send_message(session_id, branch_id, content.to_owned(), |_| {})
        .await
        .unwrap_err();
    store.turns_for_branch(branch_id).unwrap().pop().unwrap()
}

fn complete_with_fixture_candidate(
    store: &mut Store,
    turn: &stcli_core::TurnProjection,
    content: &str,
) -> EntityId {
    let attempt = store
        .attempts_for_turn(turn.turn_id)
        .unwrap()
        .pop()
        .unwrap();
    let candidate_id = EntityId::new();
    store
        .record_event(
            Some(turn.session_id),
            "attempt.completed",
            &json!({
                "attempt_id": attempt.attempt_id,
                "turn_id": turn.turn_id,
                "candidate_id": candidate_id,
                "origin": "generated",
                "content": content,
                "provider_request_hash": "sha256:0000000000000000000000000000000000000000000000000000000000000000",
                "provider_receipt": {},
            }),
        )
        .unwrap();
    store.rebuild_session_projections().unwrap();
    candidate_id
}

#[tokio::test]
async fn logical_state_changes_drive_trace_projection_and_prompt_history() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let first = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "hidden user content",
    )
    .await;
    let candidate_id =
        complete_with_fixture_candidate(&mut store, &first, "hidden candidate content");

    let hidden_turn = store.hide_turn(first.turn_id).unwrap();
    assert!(hidden_turn.hidden);
    assert_eq!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .last()
            .unwrap()
            .event_type,
        "turn.hidden"
    );
    let preview = store
        .dry_run_message(created.session.session_id, created.branch.branch_id, "next")
        .unwrap();
    assert!(
        preview
            .prompt_plan
            .messages
            .iter()
            .all(|message| !message.content.contains("hidden user content"))
    );

    let second = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "visible user content",
    )
    .await;
    let second_candidate_id =
        complete_with_fixture_candidate(&mut store, &second, "candidate to hide");
    let hidden_candidate = store.hide_candidate(second_candidate_id).unwrap();
    assert!(hidden_candidate.hidden);
    let preview = store
        .dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            "after hidden candidate",
        )
        .unwrap();
    assert!(
        preview
            .prompt_plan
            .messages
            .iter()
            .all(|message| !message.content.contains("candidate to hide"))
    );
    assert!(
        preview
            .prompt_plan
            .messages
            .iter()
            .any(|message| message.content.contains("visible user content"))
    );

    store.delete_candidate(candidate_id).unwrap();
    assert!(store.candidate(candidate_id).unwrap().is_none());
    store.delete_turn(first.turn_id).unwrap();
    assert!(store.turn(first.turn_id).unwrap().is_none());
    assert!(
        store
            .turns_for_branch(created.branch.branch_id)
            .unwrap()
            .iter()
            .all(|turn| turn.turn_id != first.turn_id)
    );

    store.rebuild_session_projections().unwrap();
    assert!(store.candidate(candidate_id).unwrap().is_none());
    assert!(store.turn(first.turn_id).unwrap().is_none());
    assert!(
        store
            .candidate(second_candidate_id)
            .unwrap()
            .unwrap()
            .hidden
    );
}

#[tokio::test]
async fn compaction_reaps_unreferenced_tombstones_but_preserves_active_forks() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let fork = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "fork point",
    )
    .await;
    store
        .edit_user_turn(fork.turn_id, "active fork".to_owned(), |_| {})
        .await
        .unwrap_err();
    let active_branch = store
        .branches(created.session.session_id)
        .unwrap()
        .into_iter()
        .find(|branch| branch.forked_from_turn_id == Some(fork.turn_id))
        .unwrap();
    let disposable = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "disposable",
    )
    .await;
    store.delete_turn(fork.turn_id).unwrap();
    store.delete_turn(disposable.turn_id).unwrap();

    let report = store.compact_session(created.session.session_id).unwrap();

    assert_eq!(report.removed.turns, 1);
    assert_eq!(report.preserved.turns, 1);
    store.rebuild_session_projections().unwrap();
    assert!(
        store
            .turns_for_branch(active_branch.branch_id)
            .unwrap()
            .iter()
            .any(|turn| turn.user_content == "active fork")
    );
    let remaining_references = store
        .trace_events(Some(created.session.session_id))
        .unwrap()
        .into_iter()
        .filter(|event| {
            event
                .payload
                .to_string()
                .contains(&disposable.turn_id.to_string())
        })
        .map(|event| event.event_type)
        .collect::<Vec<_>>();
    assert_eq!(remaining_references, vec!["session.compacted"]);
}

#[tokio::test]
async fn compaction_reaps_deleted_candidates_and_empty_branches() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "candidate owner",
    )
    .await;
    let candidate_id = complete_with_fixture_candidate(&mut store, &turn, "deleted candidate");
    let child_candidate = store
        .edit_candidate(candidate_id, "deleted child candidate".to_owned())
        .unwrap()
        .candidate;
    let attempt_id = store
        .attempts_for_turn(turn.turn_id)
        .unwrap()
        .into_iter()
        .find(|attempt| attempt.status == AttemptStatus::Completed)
        .unwrap()
        .attempt_id;
    let empty_branch = store
        .create_branch(
            created.session.session_id,
            created.branch.branch_id,
            created.branch.greeting_index,
        )
        .unwrap();

    store
        .delete_candidate(child_candidate.candidate_id)
        .unwrap();
    store.delete_candidate(candidate_id).unwrap();
    store.delete_branch(empty_branch.branch_id).unwrap();
    assert!(store.candidate(candidate_id).unwrap().is_none());
    assert!(
        store
            .turn(turn.turn_id)
            .unwrap()
            .unwrap()
            .selected_candidate_id
            .is_none()
    );
    assert!(store.branch(empty_branch.branch_id).unwrap().is_none());
    let events = store
        .trace_events(Some(created.session.session_id))
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "candidate.deleted")
    );
    assert!(
        events
            .iter()
            .any(|event| event.event_type == "branch.deleted")
    );

    let report = store.compact_session(created.session.session_id).unwrap();

    assert_eq!(report.removed.candidates, 2);
    assert_eq!(report.removed.branches, 1);
    assert!(store.attempt(attempt_id).unwrap().is_none());
    assert!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .iter()
            .all(|event| !event.payload.to_string().contains("deleted candidate"))
    );
    assert!(
        !fs::read(store.path())
            .unwrap()
            .windows(b"deleted candidate".len())
            .any(|window| window == b"deleted candidate")
    );
    assert!(
        !fs::read(store.path())
            .unwrap()
            .windows(b"deleted child candidate".len())
            .any(|window| window == b"deleted child candidate")
    );
    store.rebuild_session_projections().unwrap();
    assert!(store.candidate(candidate_id).unwrap().is_none());
    assert!(
        store
            .candidate(child_candidate.candidate_id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .turn(turn.turn_id)
            .unwrap()
            .unwrap()
            .selected_candidate_id
            .is_none()
    );
    assert!(store.branch(empty_branch.branch_id).unwrap().is_none());
}

#[tokio::test]
async fn compaction_preserves_deleted_candidate_with_active_retry_descendant() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let original_turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "original",
    )
    .await;
    let candidate_id =
        complete_with_fixture_candidate(&mut store, &original_turn, "retry ancestor");
    let original_attempt = store
        .attempts_for_turn(original_turn.turn_id)
        .unwrap()
        .into_iter()
        .find(|attempt| attempt.status == AttemptStatus::Completed)
        .unwrap();
    let retry_turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "active retry",
    )
    .await;
    let retry_attempt_id = EntityId::new();
    store
        .record_event(
            Some(created.session.session_id),
            "attempt.started",
            &json!({
                "attempt_id": retry_attempt_id,
                "turn_id": retry_turn.turn_id,
                "config_hash": original_attempt.config_hash,
                "retry_of_attempt_id": original_attempt.attempt_id,
                "prompt_plan": original_attempt.prompt_plan,
            }),
        )
        .unwrap();
    store.rebuild_session_projections().unwrap();
    store.delete_candidate(candidate_id).unwrap();

    let report = store.compact_session(created.session.session_id).unwrap();

    assert_eq!(report.preserved.candidates, 1);
    assert!(
        store
            .attempt(original_attempt.attempt_id)
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .attempt(retry_attempt_id)
            .unwrap()
            .unwrap()
            .retry_of_attempt_id,
        Some(original_attempt.attempt_id)
    );
    assert!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .iter()
            .any(|event| event.payload.to_string().contains("retry ancestor"))
    );
}

#[tokio::test]
async fn compaction_reaps_deleted_fork_branch_and_its_turns_in_reference_order() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let fork = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "deleted fork",
    )
    .await;
    store
        .edit_user_turn(fork.turn_id, "deleted child".to_owned(), |_| {})
        .await
        .unwrap_err();
    let child_branch = store
        .branches(created.session.session_id)
        .unwrap()
        .into_iter()
        .find(|branch| branch.forked_from_turn_id == Some(fork.turn_id))
        .unwrap();
    let child_turn = store
        .turns_for_branch(child_branch.branch_id)
        .unwrap()
        .pop()
        .unwrap();
    store.delete_turn(child_turn.turn_id).unwrap();
    store.delete_branch(child_branch.branch_id).unwrap();
    store.delete_turn(fork.turn_id).unwrap();

    let report = store.compact_session(created.session.session_id).unwrap();

    assert_eq!(report.removed.branches, 1);
    assert_eq!(report.removed.turns, 2);
    store.rebuild_session_projections().unwrap();
    assert!(store.branch(child_branch.branch_id).unwrap().is_none());
    assert!(store.turn(fork.turn_id).unwrap().is_none());
    assert!(store.turn(child_turn.turn_id).unwrap().is_none());
}
