use stcli_core::{
    ContextFormatting, EngineCommand, EngineInspection, EngineQuery, EngineResult, FormatMode,
    InstructTemplate, StcliEngine, Store,
};
use stcli_testkit::{configuration, fixtures};
use tempfile::tempdir;

#[tokio::test]
async fn engine_inspection_returns_authoritative_branch_history() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    drop(store);

    let engine = StcliEngine::new(database);
    let EngineInspection::BranchHistory(history) = engine
        .inspect(EngineQuery::BranchHistory {
            session_id: created.session.session_id,
            branch_id: created.branch.branch_id,
        })
        .unwrap()
    else {
        panic!("unexpected inspection result");
    };

    assert_eq!(history.session, created.session);
    assert_eq!(history.branch, created.branch);
    assert!(history.turns.is_empty());
    assert!(history.greeting.is_some());
}

#[tokio::test]
async fn engine_commands_mutate_through_the_turn_trace() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let before = store
        .trace_events(Some(created.session.session_id))
        .unwrap()
        .len();
    drop(store);

    let engine = StcliEngine::new(&database);
    engine
        .execute(
            EngineCommand::SelectGreeting {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap();

    let store = Store::open(database).unwrap();
    assert_eq!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .len(),
        before + 1
    );
}

#[tokio::test]
async fn engine_owns_artifact_and_session_storage_operations() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);

    let EngineResult::ArtifactBundle {
        primary: character,
        supplementary_artifacts,
        asset_count,
    } = engine
        .execute(
            EngineCommand::ImportArtifact {
                source: fixtures::minimal_card().as_bytes().to_vec(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected import result");
    };
    assert!(supplementary_artifacts.is_empty());
    assert_eq!(asset_count, 0);
    let EngineResult::CreatedSession(created) = engine
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(configuration(character.revision_hash)),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected create result");
    };

    let EngineInspection::SessionProjections(sessions) =
        engine.inspect(EngineQuery::SessionProjections).unwrap()
    else {
        panic!("unexpected Session inspection");
    };
    assert_eq!(sessions, vec![created.session.clone()]);

    let EngineResult::Session(archived) = engine
        .execute(
            EngineCommand::ArchiveSession {
                session_id: created.session.session_id,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected archive result");
    };
    assert!(archived.archived);
}

#[tokio::test]
async fn engine_persona_description_is_pinned_rendered_and_ordered_by_preset() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let preset = store
        .import_artifact(
            br#"{
                "prompts": [
                    {"identifier": "main", "role": "system", "content": ""},
                    {"identifier": "personaDescription", "role": "system", "content": ""},
                    {"identifier": "charDescription", "role": "system", "content": ""},
                    {"identifier": "chatHistory", "role": "system", "content": ""},
                    {"identifier": "userInput", "role": "user", "content": ""}
                ],
                "prompt_order": [{"order": [
                    {"identifier": "main", "enabled": true},
                    {"identifier": "personaDescription", "enabled": true},
                    {"identifier": "charDescription", "enabled": true},
                    {"identifier": "chatHistory", "enabled": true},
                    {"identifier": "userInput", "enabled": true}
                ]}]
            }"#,
        )
        .unwrap();
    let mut config = configuration(character.revision_hash);
    assert_eq!(config.persona_description, None);
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("persona_description")
            .is_none()
    );
    config.persona_description = Some(" \n".to_owned());
    assert!(
        serde_json::to_value(&config)
            .unwrap()
            .get("persona_description")
            .is_none()
    );
    config.persona_description = None;
    let engine = StcliEngine::new(&database);
    let EngineResult::CreatedSession(created) = engine
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(config.clone()),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected create result");
    };
    assert_eq!(
        created.configuration.revision_hash.to_string(),
        "sha256:ebc64a6bbbacac40d860dc3771c39ca6a1a64cdf2a3dd693c4b8acec9fbe7d99"
    );

    config.persona_name = "Morgan".to_owned();
    config.persona_description = Some("{{user}} is searching for {{char}}.".to_owned());
    config.prompt_preset_revision = Some(preset.revision_hash);
    engine
        .execute(
            EngineCommand::UpdateConfiguration {
                session_id: created.session.session_id,
                configuration: Box::new(config.clone()),
            },
            |_| {},
        )
        .await
        .unwrap();
    let EngineResult::DryRun(dry_run) = engine
        .execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Hello".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected dry-run result");
    };
    let persona_index = dry_run
        .prompt_plan
        .segments
        .iter()
        .position(|segment| segment.slot == "personaDescription")
        .unwrap();
    let persona = &dry_run.prompt_plan.segments[persona_index];
    assert_eq!(persona.source, "persona-description");
    assert_eq!(persona.raw_content, "{{user}} is searching for {{char}}.");
    assert_eq!(persona.content, "Morgan is searching for Alice.");
    assert_eq!(
        dry_run.prompt_plan.segments[persona_index + 1].slot,
        "charDescription"
    );
    assert_eq!(persona.macro_evaluations.len(), 2);

    config.prompt_preset_revision = None;
    config.provider.format_mode = FormatMode::TextCompletion;
    config.provider.completions_path = Some("/v1/completions".to_owned());
    config.provider.instruct_template = Some(InstructTemplate {
        r#macro: true,
        stop_sequence: "{{personaDescription}}".to_owned(),
        ..InstructTemplate::default()
    });
    config.provider.context_formatting = Some(ContextFormatting {
        story_string: "{{personaDescription}}|{{persona_description}}".to_owned(),
        ..ContextFormatting::default()
    });
    engine
        .execute(
            EngineCommand::UpdateConfiguration {
                session_id: created.session.session_id,
                configuration: Box::new(config),
            },
            |_| {},
        )
        .await
        .unwrap();
    let EngineResult::DryRun(flat) = engine
        .execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Hello".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected dry-run result");
    };
    assert!(
        flat.prompt_plan
            .text_prompt
            .as_deref()
            .unwrap()
            .starts_with("Morgan is searching for Alice.|Morgan is searching for Alice.")
    );
    assert!(
        flat.prompt_plan
            .stop_sequences
            .contains(&"Morgan is searching for Alice.".to_owned())
    );
}
