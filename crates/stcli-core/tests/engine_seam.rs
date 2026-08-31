use serde_json::json;
use stcli_core::{ContextFormatting, FormatMode, InstructTemplate};
use stcli_core::{
    EngineCommand, EngineError, EngineInspection, EngineQuery, EngineResult, EntityId,
    SessionError, StcliEngine, Store,
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

#[tokio::test]
async fn duplicate_session_reauthors_an_independent_lineage_through_an_inclusive_turn() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
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
        "first",
    )
    .await;
    let first_candidate = complete_with_candidate(&mut store, &first, "first answer");
    let alternate_candidate = EntityId::new();
    store
        .record_event(
            Some(created.session.session_id),
            "candidate.manual-created",
            &json!({
                "candidate_id": alternate_candidate,
                "turn_id": first.turn_id,
                "parent_candidate_id": first_candidate,
                "content": "alternate first answer",
            }),
        )
        .unwrap();
    store.rebuild_session_projections().unwrap();
    store
        .select_swipe(first.turn_id, alternate_candidate)
        .unwrap();
    store.select_swipe(first.turn_id, first_candidate).unwrap();
    let second = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "second",
    )
    .await;
    let deleted_candidate = complete_with_candidate(&mut store, &second, "deleted answer");
    store.hide_turn(second.turn_id).unwrap();
    store.delete_candidate(deleted_candidate).unwrap();
    let third = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "not copied",
    )
    .await;
    complete_with_candidate(&mut store, &third, "not copied answer");
    store
        .rename_session(created.session.session_id, "Original")
        .unwrap();

    let mut updated_configuration = created.configuration.configuration.clone();
    updated_configuration.persona_name = "Duplicated persona".to_owned();
    let selected_configuration = store
        .update_session_configuration(created.session.session_id, updated_configuration)
        .unwrap();
    assert!(
        store
            .archive_session(created.session.session_id)
            .unwrap()
            .archived
    );
    let source_trace = store
        .trace_events(Some(created.session.session_id))
        .unwrap();
    drop(store);

    let engine = StcliEngine::new(&database);
    let EngineResult::DuplicatedSession(duplicated) = engine
        .execute(
            EngineCommand::DuplicateSession {
                session_id: created.session.session_id,
                branch_id: None,
                up_to_turn_id: Some(second.turn_id),
                new_name: None,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected duplication result");
    };

    assert_ne!(duplicated.session.session_id, created.session.session_id);
    assert_ne!(duplicated.branch.branch_id, created.branch.branch_id);
    assert_eq!(
        duplicated.configuration.revision_hash,
        selected_configuration.revision_hash
    );
    assert_eq!(
        duplicated.session.custom_name.as_deref(),
        Some("Original (copy)")
    );
    assert!(!duplicated.session.archived);

    let mut store = Store::open(&database).unwrap();
    assert_eq!(
        store
            .trace_events(Some(created.session.session_id))
            .unwrap(),
        source_trace
    );
    let duplicated_trace = store
        .trace_events(Some(duplicated.session.session_id))
        .unwrap();
    let provenance = duplicated_trace
        .iter()
        .find(|event| event.event_type == "session.duplicated")
        .unwrap();
    assert_eq!(
        provenance.payload,
        json!({
            "source_session_id": created.session.session_id,
            "source_branch_id": created.branch.branch_id,
            "source_up_to_turn_id": second.turn_id,
            "copied_turns": 2,
            "copied_candidates": 3,
        })
    );
    assert!(
        duplicated_trace
            .iter()
            .any(|event| event.event_type == "turn.hidden")
    );
    assert!(
        duplicated_trace
            .iter()
            .any(|event| event.event_type == "candidate.deleted")
    );
    assert!(duplicated_trace.iter().all(|event| !matches!(
        event.event_type.as_str(),
        "state.committed" | "plugin.command" | "stscript.started" | "stscript.completed"
    )));
    // Regression: duplication re-emits explicit Candidate selections without adding another.
    assert_eq!(
        duplicated_trace
            .iter()
            .filter(|event| event.event_type == "turn.candidate-selected")
            .count(),
        source_trace
            .iter()
            .filter(|event| event.event_type == "turn.candidate-selected")
            .count()
    );

    let duplicated_turns = store.turns_for_branch(duplicated.branch.branch_id).unwrap();
    assert_eq!(
        duplicated_turns
            .iter()
            .map(|turn| turn.user_content.as_str())
            .collect::<Vec<_>>(),
        vec!["first", "second"]
    );
    assert!(duplicated_turns[1].hidden);
    assert!(
        store
            .candidates_for_turn(duplicated_turns[1].turn_id)
            .unwrap()
            .is_empty()
    );
    assert_ne!(duplicated_turns[0].turn_id, first.turn_id);
    assert_eq!(
        store
            .attempts_for_turn(duplicated_turns[0].turn_id)
            .unwrap()
            .len(),
        store.attempts_for_turn(first.turn_id).unwrap().len()
    );
    assert_eq!(
        store
            .candidates_for_turn(duplicated_turns[0].turn_id)
            .unwrap()
            .len(),
        2
    );
    store.rebuild_session_projections().unwrap();
    assert_eq!(
        store
            .session(duplicated.session.session_id)
            .unwrap()
            .unwrap()
            .custom_name
            .as_deref(),
        Some("Original (copy)")
    );

    create_failed_turn(
        &mut store,
        duplicated.session.session_id,
        duplicated.branch.branch_id,
        "duplicate only",
    )
    .await;
    create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "source only",
    )
    .await;
    let source_contents = store
        .turns_for_branch(created.branch.branch_id)
        .unwrap()
        .into_iter()
        .map(|turn| turn.user_content)
        .collect::<Vec<_>>();
    let duplicated_contents = store
        .turns_for_branch(duplicated.branch.branch_id)
        .unwrap()
        .into_iter()
        .map(|turn| turn.user_content)
        .collect::<Vec<_>>();
    assert!(
        source_contents
            .iter()
            .any(|content| content == "source only")
    );
    assert!(
        !source_contents
            .iter()
            .any(|content| content == "duplicate only")
    );
    assert!(
        duplicated_contents
            .iter()
            .any(|content| content == "duplicate only")
    );
    assert!(
        !duplicated_contents
            .iter()
            .any(|content| content == "source only")
    );
}

#[tokio::test]
async fn duplicate_session_rejects_a_turn_outside_the_selected_lineage() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let root_turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "root only",
    )
    .await;
    let other_branch = store
        .create_branch(created.session.session_id, created.branch.branch_id, 0)
        .unwrap();
    let sessions_before = store.sessions().unwrap().len();
    drop(store);

    let engine = StcliEngine::new(&database);
    let error = engine
        .execute(
            EngineCommand::DuplicateSession {
                session_id: created.session.session_id,
                branch_id: Some(other_branch.branch_id),
                up_to_turn_id: Some(root_turn.turn_id),
                new_name: Some("Rejected".to_owned()),
            },
            |_| {},
        )
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        EngineError::Session(SessionError::TurnNotOnBranch {
            turn_id,
            branch_id,
        }) if turn_id == root_turn.turn_id && branch_id == other_branch.branch_id
    ));
    assert_eq!(
        Store::open(database).unwrap().sessions().unwrap().len(),
        sessions_before
    );
}

#[tokio::test]
async fn create_branch_command_records_fork_and_validates_lineage() {
    // Regression test for 01-chat-b: explicit Branch creation must preserve fork semantics.
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let fork_turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "try another path",
    )
    .await;
    let trace_count = store
        .trace_events(Some(created.session.session_id))
        .unwrap()
        .len();
    drop(store);

    let engine = StcliEngine::new(&database);
    let EngineResult::Branch(branch) = engine
        .execute(
            EngineCommand::CreateBranch {
                session_id: created.session.session_id,
                source_branch_id: None,
                at_turn_id: Some(fork_turn.turn_id),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected engine result");
    };
    assert_eq!(branch.parent_branch_id, Some(created.branch.branch_id));
    assert_eq!(branch.forked_from_turn_id, Some(fork_turn.turn_id));
    let store = Store::open(&database).unwrap();
    let events = store
        .trace_events(Some(created.session.session_id))
        .unwrap();
    assert_eq!(events.len(), trace_count + 1);
    assert_eq!(events.last().unwrap().event_type, "branch.created");
    drop(store);

    let EngineResult::Branch(from_start) = engine
        .execute(
            EngineCommand::CreateBranch {
                session_id: created.session.session_id,
                source_branch_id: Some(created.branch.branch_id),
                at_turn_id: None,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("unexpected engine result");
    };
    assert_eq!(from_start.forked_from_turn_id, None);

    let error = engine
        .execute(
            EngineCommand::CreateBranch {
                session_id: created.session.session_id,
                source_branch_id: Some(from_start.branch_id),
                at_turn_id: Some(fork_turn.turn_id),
            },
            |_| {},
        )
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        EngineError::Session(SessionError::TurnNotOnBranch { turn_id, branch_id })
            if turn_id == fork_turn.turn_id && branch_id == from_start.branch_id
    ));
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

fn complete_with_candidate(
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
