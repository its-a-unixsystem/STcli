use std::collections::BTreeMap;

use serde_json::json;
use stcli_core::{EngineCommand, EngineResult, StcliEngine, Store};
use stcli_testkit::{configuration, fixtures};
use tempfile::tempdir;

fn preset_source() -> Vec<u8> {
    serde_json::to_vec(&json!({
        "name": "Toggle fixture",
        "temperature": 0.7,
        "prompts": [
            {"identifier": "main", "role": "system", "content": "MAIN"},
            {"identifier": "optional", "role": "system", "content": "OPTIONAL"}
        ],
        "prompt_order": [{"character_id": 100001, "order": [
            {"identifier": "main", "enabled": true},
            {"identifier": "optional", "enabled": true}
        ]}],
        "extension_field": {"preserved": true}
    }))
    .unwrap()
}

#[tokio::test]
async fn prompt_order_update_is_atomic_deduplicated_forward_only_and_effective() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store.import_artifact(&preset_source()).unwrap();
    let mut first_config = configuration(character.revision_hash.clone());
    first_config.prompt_preset_revision = Some(preset.revision_hash.clone());
    let first = store.create_session(first_config, 0).unwrap();
    let mut second_config = configuration(character.revision_hash);
    second_config.prompt_preset_revision = Some(preset.revision_hash.clone());
    let second = store.create_session(second_config, 0).unwrap();

    store
        .send_message(
            first.session.session_id,
            first.branch.branch_id,
            "Before toggle".to_owned(),
            |_| {},
        )
        .await
        .unwrap_err();
    let prior_attempt = store
        .attempts_for_turn(
            store
                .turns_for_branch(first.branch.branch_id)
                .unwrap()
                .last()
                .unwrap()
                .turn_id,
        )
        .unwrap()
        .last()
        .unwrap()
        .clone();
    let prior_config_hash = prior_attempt.config_hash.clone();
    drop(store);

    let engine = StcliEngine::new(&database);
    let changes = BTreeMap::from([("optional".to_owned(), false)]);
    let EngineResult::PromptOrderUpdated {
        artifact,
        configuration: Some(updated_configuration),
    } = engine
        .execute(
            EngineCommand::UpdatePromptOrder {
                session_id: Some(first.session.session_id),
                revision_hash: preset.revision_hash.clone(),
                character_id: Some(100001),
                changes: changes.clone(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected update result");
    };
    assert_ne!(artifact.revision_hash, preset.revision_hash);
    assert_eq!(
        updated_configuration.configuration.prompt_preset_revision,
        Some(artifact.revision_hash.clone())
    );

    let store = Store::open(&database).unwrap();
    let source = store
        .decoded_artifact(&preset.revision_hash)
        .unwrap()
        .semantic;
    let updated = store
        .decoded_artifact(&artifact.revision_hash)
        .unwrap()
        .semantic;
    let mut expected = source.clone();
    expected["prompt_order"][0]["order"][1]["enabled"] = json!(false);
    assert_eq!(updated, expected);
    assert_eq!(
        store
            .session(second.session.session_id)
            .unwrap()
            .unwrap()
            .current_config_hash,
        second.configuration.revision_hash
    );
    assert_eq!(
        store
            .attempt(prior_attempt.attempt_id)
            .unwrap()
            .unwrap()
            .config_hash,
        prior_config_hash
    );
    let dry_run = store
        .dry_run_message(
            first.session.session_id,
            first.branch.branch_id,
            "After toggle",
        )
        .unwrap();
    assert!(!dry_run.provider_request.to_string().contains("OPTIONAL"));
    drop(store);

    let EngineResult::PromptOrderUpdated {
        artifact: duplicate,
        configuration: None,
    } = engine
        .execute(
            EngineCommand::UpdatePromptOrder {
                session_id: None,
                revision_hash: preset.revision_hash,
                character_id: Some(100001),
                changes,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected duplicate update result");
    };
    assert_eq!(duplicate.revision_hash, artifact.revision_hash);
}
