//! End-to-end regex-script execution through the isolated worker.
//!
//! These tests exercise the real prompt pipeline, which spawns the
//! `internal regex-replace-worker` subcommand. The shared environment guard
//! points `STCLI_REGEX_WORKER` at the built `stcli` binary for the test scope.

use serde_json::{Value, json};
use stcli_core::{
    ContentHash, ContextFormatting, FormatMode, InstructTemplate, ScriptSource, Store,
    extract_character_scripts,
};
use stcli_testkit::{EnvironmentGuard, configuration, fixtures};
use tempfile::tempdir;

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
    (preset.revision_hash, transformed.scripts[0].digest.clone())
}

fn with_real_worker<T>(body: impl FnOnce() -> T) -> T {
    let mut environment = EnvironmentGuard::new();
    environment.set(
        stcli_core::ecma_regex::WORKER_EXECUTABLE_ENV,
        env!("CARGO_BIN_EXE_stcli"),
    );
    body()
}

fn configure_text_completion(config: &mut stcli_core::SessionConfiguration) {
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(InstructTemplate {
        input_sequence: "<user>".to_owned(),
        output_sequence: "<assistant>".to_owned(),
        system_sequence: "<system>".to_owned(),
        stop_sequence: "</turn>".to_owned(),
        wrap: true,
        ..InstructTemplate::default()
    });
    config.provider.context_formatting = Some(ContextFormatting {
        story_string: "{{system}}\n{{description}}\n{{wiAfter}}".to_owned(),
        ..ContextFormatting::default()
    });
}

#[test]
fn granted_user_input_script_transforms_the_prompt() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let (preset_revision, digest) = preset_with_regex_script(
            &mut store,
            json!({
                "id": "redact",
                "scriptName": "Redact",
                "findRegex": "/secret/gi",
                "replaceString": "safe",
                "placement": [1]
            }),
        );
        let mut config = configuration(character.revision_hash);
        config.prompt_preset_revision = Some(preset_revision);
        config.script_grants = vec![digest.clone()];
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(
                created.session.session_id,
                created.branch.branch_id,
                "the SECRET code",
            )
            .unwrap();

        let messages = dry_run.provider_request["messages"].as_array().unwrap();
        let user = messages
            .iter()
            .find(|message| message["role"] == "user")
            .expect("a user message");
        assert_eq!(user["content"], json!("the safe code"));
        assert_eq!(dry_run.prompt_plan.regex_applications.len(), 1);
        assert_eq!(
            dry_run.prompt_plan.regex_applications[0].id,
            digest.to_string()
        );
        assert_eq!(dry_run.prompt_plan.regex_applications[0].placement, 1);
    });
}

#[test]
fn granted_script_preserves_flat_prompt_segment_attribution() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let (preset_revision, digest) = preset_with_regex_script(
            &mut store,
            json!({
                "id": "flat-redact",
                "scriptName": "Flat Redact",
                "findRegex": "/secret/gi",
                "replaceString": "safe",
                "placement": [1]
            }),
        );
        let mut config = configuration(character.revision_hash);
        config.prompt_preset_revision = Some(preset_revision);
        config.script_grants = vec![digest];
        configure_text_completion(&mut config);
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(
                created.session.session_id,
                created.branch.branch_id,
                "the SECRET code",
            )
            .unwrap();

        let user = dry_run
            .prompt_plan
            .segments
            .iter()
            .find(|segment| segment.source == "current-user-action")
            .unwrap();
        assert_eq!(user.raw_content, "the SECRET code");
        assert_eq!(user.content, "the safe code");
        assert_eq!(user.regex_applications.len(), 1);
        let prompt = dry_run.prompt_plan.text_prompt.as_deref().unwrap();
        assert!(prompt.contains("the safe code"));
        assert!(!prompt.contains("the SECRET code"));
    });
}

#[test]
fn granted_ai_output_script_transforms_history_messages() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let (preset_revision, digest) = preset_with_regex_script(
            &mut store,
            json!({
                "id": "greet",
                "scriptName": "Greet",
                "findRegex": "/Welcome\\./g",
                "replaceString": "Greetings, {{match}}",
                "placement": [2]
            }),
        );
        let mut config = configuration(character.revision_hash);
        config.prompt_preset_revision = Some(preset_revision);
        config.script_grants = vec![digest];
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(created.session.session_id, created.branch.branch_id, "hi")
            .unwrap();

        let messages = dry_run.provider_request["messages"].as_array().unwrap();
        let assistant = messages
            .iter()
            .find(|message| message["role"] == "assistant")
            .expect("the greeting assistant message");
        assert_eq!(assistant["content"], json!("Greetings, Welcome."));
        assert_eq!(dry_run.prompt_plan.regex_applications[0].placement, 2);
    });
}

fn character_card_with_script(script: Value) -> String {
    json!({
        "spec": "chara_card_v2",
        "spec_version": "2.0",
        "data": {
            "name": "Alice",
            "description": "A librarian.",
            "personality": "Curious",
            "scenario": "An old library",
            "first_mes": "Welcome.",
            "mes_example": "",
            "alternate_greetings": [],
            "extensions": {
                "regex_scripts": [script]
            }
        }
    })
    .to_string()
}

#[test]
fn character_card_scripts_are_extracted_with_source() {
    let card_json = character_card_with_script(json!({
        "id": "strip-ooc",
        "scriptName": "Strip OOC",
        "findRegex": "/\\(OOC:.*?\\)/g",
        "replaceString": "",
        "placement": [2]
    }));
    let card: Value = serde_json::from_str(&card_json).unwrap();
    let scripts = extract_character_scripts(&card, &[]);
    assert_eq!(scripts.len(), 1);
    assert_eq!(scripts[0].source, ScriptSource::Character);
    assert!(!scripts[0].granted);
}

#[test]
fn character_script_requires_grant_for_prompt_transformation() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let card_json = character_card_with_script(json!({
            "id": "redact-char",
            "scriptName": "CharRedact",
            "findRegex": "/secret/gi",
            "replaceString": "safe",
            "placement": [1]
        }));
        let character = store.import_artifact(card_json.as_bytes()).unwrap();
        let config = configuration(character.revision_hash);
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(
                created.session.session_id,
                created.branch.branch_id,
                "the SECRET code",
            )
            .unwrap();

        let messages = dry_run.provider_request["messages"].as_array().unwrap();
        let user = messages.iter().find(|m| m["role"] == "user").unwrap();
        assert_eq!(
            user["content"], "the SECRET code",
            "ungranted character script must not transform content"
        );
        assert!(dry_run.prompt_plan.regex_applications.is_empty());
    });
}

#[test]
fn granted_character_script_transforms_prompt() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let card_json = character_card_with_script(json!({
            "id": "redact-char",
            "scriptName": "CharRedact",
            "findRegex": "/secret/gi",
            "replaceString": "safe",
            "placement": [1]
        }));
        let character = store.import_artifact(card_json.as_bytes()).unwrap();
        let card = store.decoded_artifact(&character.revision_hash).unwrap();
        let scripts = extract_character_scripts(&card.semantic, &[]);
        let digest = scripts[0].digest.clone();

        let mut config = configuration(character.revision_hash);
        config.script_grants = vec![digest.clone()];
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(
                created.session.session_id,
                created.branch.branch_id,
                "the SECRET code",
            )
            .unwrap();

        let messages = dry_run.provider_request["messages"].as_array().unwrap();
        let user = messages.iter().find(|m| m["role"] == "user").unwrap();
        assert_eq!(user["content"], "the safe code");
        assert_eq!(dry_run.prompt_plan.regex_applications.len(), 1);
    });
}

#[test]
fn worldinfo_placement_transforms_lorebook_content() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let lorebook_json = json!({
            "entries": {
                "0": {
                    "uid": 0,
                    "key": ["Welcome"],
                    "content": "CLASSIFIED data here",
                    "enabled": true,
                    "constant": true,
                    "position": 0
                }
            }
        })
        .to_string();
        let lorebook = store.import_artifact(lorebook_json.as_bytes()).unwrap();

        let (preset_revision, digest) = preset_with_regex_script(
            &mut store,
            json!({
                "id": "wi-transform",
                "scriptName": "WI Transform",
                "findRegex": "/CLASSIFIED/g",
                "replaceString": "PUBLIC",
                "placement": [5]
            }),
        );

        let mut config = configuration(character.revision_hash);
        config.prompt_preset_revision = Some(preset_revision);
        config.script_grants = vec![digest];
        config.lorebook_revisions = vec![lorebook.revision_hash];
        let created = store.create_session(config, 0).unwrap();

        let dry_run = store
            .dry_run_message(
                created.session.session_id,
                created.branch.branch_id,
                "Welcome",
            )
            .unwrap();

        let activated = &dry_run.prompt_plan.lore.activated;
        assert!(
            !activated.is_empty(),
            "lorebook entry should be activated by keyword 'Welcome'"
        );
        let lore_content = &activated[0].content;
        assert!(
            lore_content.contains("PUBLIC"),
            "WorldInfo placement script should transform lorebook content: {lore_content}"
        );
        assert!(
            !lore_content.contains("CLASSIFIED"),
            "original text should be replaced"
        );
    });
}

#[test]
fn redos_timeout_does_not_hang_the_prompt() {
    with_real_worker(|| {
        let directory = tempdir().unwrap();
        let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
        let character = store
            .import_artifact(fixtures::minimal_card().as_bytes())
            .unwrap();
        let (preset_revision, digest) = preset_with_regex_script(
            &mut store,
            json!({
                "id": "redos",
                "scriptName": "ReDoS",
                "findRegex": "/(a+)+$/g",
                "replaceString": "safe",
                "placement": [1]
            }),
        );
        let mut config = configuration(character.revision_hash);
        config.prompt_preset_revision = Some(preset_revision);
        config.script_grants = vec![digest];
        let created = store.create_session(config, 0).unwrap();

        let start = std::time::Instant::now();
        let result = store.dry_run_message(
            created.session.session_id,
            created.branch.branch_id,
            &("a".repeat(30) + "!"),
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed < std::time::Duration::from_secs(2),
            "ReDoS must be killed by timeout, not hang; took {elapsed:?}"
        );
        assert!(
            result.is_err(),
            "ReDoS pattern should produce a timeout error"
        );
    });
}

#[test]
fn session_details_lists_discovered_scripts() {
    let directory = tempdir().unwrap();
    let mut store = Store::open(directory.path().join("stcli.sqlite3")).unwrap();
    let card_json = character_card_with_script(json!({
        "id": "char-script",
        "scriptName": "CharScript",
        "findRegex": "/foo/g",
        "replaceString": "bar",
        "placement": [2]
    }));
    let character = store.import_artifact(card_json.as_bytes()).unwrap();
    let (preset_revision, _) = preset_with_regex_script(
        &mut store,
        json!({
            "id": "preset-script",
            "scriptName": "PresetScript",
            "findRegex": "/baz/g",
            "replaceString": "qux",
            "placement": [1]
        }),
    );
    let mut config = configuration(character.revision_hash);
    config.prompt_preset_revision = Some(preset_revision);
    let created = store.create_session(config, 0).unwrap();

    let engine = stcli_core::StcliEngine::new(directory.path().join("stcli.sqlite3"));
    let inspection = engine
        .inspect(stcli_core::EngineQuery::SessionDetails {
            session_id: created.session.session_id,
        })
        .unwrap();
    let details = match inspection {
        stcli_core::EngineInspection::SessionDetails(d) => d,
        _ => panic!("expected SessionDetails"),
    };

    assert_eq!(
        details.discovered_scripts.len(),
        2,
        "should discover one preset and one character script"
    );
    let sources: Vec<_> = details
        .discovered_scripts
        .iter()
        .map(|s| s.source)
        .collect();
    assert!(sources.contains(&ScriptSource::Preset));
    assert!(sources.contains(&ScriptSource::Character));
}
