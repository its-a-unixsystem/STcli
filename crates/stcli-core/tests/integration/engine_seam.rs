use serde_json::json;
use stcli_core::{ContextFormatting, FormatMode, InstructTemplate};
use stcli_core::{
    EngineCommand, EngineError, EngineInspection, EngineQuery, EngineResult, EntityId,
    SessionError, StcliEngine, Store,
};
use stcli_testkit::{
    configuration, fixtures, write_roadway_interaction_fixture,
    write_stepped_thinking_interaction_fixture,
};
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

#[cfg(feature = "scripting")]
#[tokio::test]
async fn summarize_interaction_exposes_declared_settings_and_action() {
    // Regression test for ticket 02: settings must be editable without exposing checkpoints.
    use stcli_core::{DEFAULT_MEMORY_EXTENSION_ID, InteractionControl};

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some(DEFAULT_MEMORY_EXTENSION_ID.to_owned()),
        })
        .unwrap()
    else {
        panic!("memory package inventory");
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    drop(store);
    engine
        .execute(
            EngineCommand::AdoptExtension {
                session_id: created.session.session_id,
                id: memory.manifest.id,
                version: memory.manifest.version.to_string(),
                digest: memory.manifest.component_sha256,
                settings: json!({"memoryFrozen": true}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();

    let EngineInspection::ExtensionInteractions(surfaces) = engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    else {
        panic!("interaction surfaces");
    };
    let surface = surfaces.into_iter().next().unwrap();
    assert_eq!(surface.identity.extension_id, DEFAULT_MEMORY_EXTENSION_ID);
    assert_eq!(surface.identity.package_version, "1.1.0");
    assert_eq!(surface.groups.len(), 3);
    assert!(
        surface
            .groups
            .iter()
            .flat_map(|group| &group.fields)
            .any(|field| {
                field.label == "Freeze automatic refresh"
                    && field.control == InteractionControl::Boolean
            })
    );
    assert!(
        surface
            .groups
            .iter()
            .flat_map(|group| &group.fields)
            .all(|field| { field.label != "checkpoints" })
    );
    assert_eq!(surface.actions[0].label, "Summarize now");
    assert!(!surface.actions[0].enabled);
}
#[cfg(feature = "scripting")]
#[tokio::test]
async fn summarize_interaction_save_creates_revision_and_rejects_stale_submission() {
    // Regression test for ticket 02: saved settings create a revision without replacing state.
    use stcli_core::{
        DEFAULT_MEMORY_EXTENSION_ID, InteractionEdit, InteractionOutcome, InteractionSubmission,
        InteractionValue, VariableScope,
    };

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some(DEFAULT_MEMORY_EXTENSION_ID.to_owned()),
        })
        .unwrap()
    else {
        panic!("memory package inventory");
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let created = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    let mut state = store.state_transaction(created.session.session_id).unwrap();
    state.set(
        VariableScope::Local,
        "extension.memory.settings",
        json!({"checkpoints": [{"sentinel": true}], "unknown": "kept"}),
        "memory",
        "test",
    );
    store
        .commit_state_transaction(EntityId::new(), state)
        .unwrap();
    drop(store);
    engine
        .execute(
            EngineCommand::AdoptExtension {
                session_id: created.session.session_id,
                id: memory.manifest.id,
                version: memory.manifest.version.to_string(),
                digest: memory.manifest.component_sha256,
                settings: json!({"memoryFrozen": true}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let before = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record,
        _ => panic!("configuration"),
    };
    let surface = match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!("surface"),
    };
    let prompt_words = surface
        .groups
        .iter()
        .flat_map(|group| &group.fields)
        .find(|field| field.label == "Summary word target")
        .unwrap()
        .target
        .clone();
    let command = EngineCommand::SubmitExtensionInteraction {
        identity: surface.identity.clone(),
        expected_revision: surface.revision.clone(),
        submission: InteractionSubmission::Save {
            target: surface.save.target.clone(),
            edits: vec![InteractionEdit {
                target: prompt_words,
                value: InteractionValue::Number(123.into()),
            }],
        },
    };
    let EngineResult::ExtensionInteraction(saved) =
        engine.execute(command.clone(), |_| {}).await.unwrap()
    else {
        panic!("interaction result");
    };
    let InteractionOutcome::Saved {
        configuration_revision,
    } = &saved.outcome
    else {
        panic!("saved outcome");
    };
    assert_ne!(configuration_revision, &before.revision_hash);
    let current = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record,
        _ => panic!("configuration"),
    };
    assert_eq!(
        current.configuration.plugins[0].settings["promptWords"],
        123
    );
    let trace_before_stale = Store::open(&database)
        .unwrap()
        .trace_events(Some(created.session.session_id))
        .unwrap()
        .len();
    let EngineResult::ExtensionInteraction(stale) = engine.execute(command, |_| {}).await.unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(stale.outcome, InteractionOutcome::Rejected { .. }));
    assert_eq!(
        Store::open(&database)
            .unwrap()
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .len(),
        trace_before_stale
    );
    let state = Store::open(&database)
        .unwrap()
        .state_transaction(created.session.session_id)
        .unwrap();
    let settings = &state
        .get(VariableScope::Local, "extension.memory.settings")
        .unwrap()
        .value;
    assert_eq!(settings["checkpoints"][0]["sentinel"], true);
    assert_eq!(settings["unknown"], "kept");
}

#[cfg(feature = "scripting")]
#[tokio::test]
async fn extension_interactions_reject_invalid_context_and_authority_before_effects() {
    // Regression test for ticket 03: invalid interaction identities and values must have no effects.
    use std::collections::BTreeSet;

    use stcli_core::{
        DEFAULT_MEMORY_EXTENSION_ID, InteractionEdit, InteractionOutcome, InteractionSubmission,
        InteractionValue,
    };

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = engine
        .inspect(EngineQuery::Plugins {
            plugin_id: Some(DEFAULT_MEMORY_EXTENSION_ID.to_owned()),
        })
        .unwrap()
    else {
        panic!("memory package inventory");
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let first = store
        .create_session(configuration(character.revision_hash.clone()), 0)
        .unwrap();
    let second = store
        .create_session(configuration(character.revision_hash), 0)
        .unwrap();
    drop(store);
    for session_id in [first.session.session_id, second.session.session_id] {
        engine
            .execute(
                EngineCommand::AdoptExtension {
                    session_id,
                    id: memory.manifest.id.clone(),
                    version: memory.manifest.version.to_string(),
                    digest: memory.manifest.component_sha256.clone(),
                    settings: json!({"memoryFrozen": true}),
                    egress: Vec::new(),
                },
                |_| {},
            )
            .await
            .unwrap();
    }
    let current_surface = |session_id, branch_id| match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id,
            branch_id: Some(branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!("surface"),
    };
    let surface = current_surface(first.session.session_id, first.branch.branch_id);
    assert_eq!(surface.identity.session_id, first.session.session_id);
    assert_eq!(surface.identity.branch_id, Some(first.branch.branch_id));
    assert_eq!(surface.identity.extension_id, DEFAULT_MEMORY_EXTENSION_ID);
    assert_eq!(
        surface.identity.component_sha256,
        memory.manifest.component_sha256
    );
    assert_eq!(surface.support, stcli_core::InteractionSupport::Available);

    let trace_len = |session_id| {
        Store::open(&database)
            .unwrap()
            .trace_events(Some(session_id))
            .unwrap()
            .len()
    };
    let before_second = trace_len(second.session.session_id);
    let mut cross_session = surface.identity.clone();
    cross_session.session_id = second.session.session_id;
    cross_session.branch_id = Some(second.branch.branch_id);
    let EngineResult::ExtensionInteraction(rejected) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: cross_session,
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: surface.save.target.clone(),
                    edits: Vec::new(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        rejected.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(trace_len(second.session.session_id), before_second);

    let EngineResult::Branch(branch) = engine
        .execute(
            EngineCommand::CreateBranch {
                session_id: first.session.session_id,
                source_branch_id: Some(first.branch.branch_id),
                at_turn_id: None,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("branch");
    };
    let before_first = trace_len(first.session.session_id);
    let mut cross_branch = surface.identity.clone();
    cross_branch.branch_id = Some(branch.branch_id);
    let EngineResult::ExtensionInteraction(rejected) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: cross_branch,
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: surface.save.target.clone(),
                    edits: Vec::new(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        rejected.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(trace_len(first.session.session_id), before_first);

    let prompt_words = surface
        .groups
        .iter()
        .flat_map(|group| &group.fields)
        .find(|field| field.label == "Summary word target")
        .unwrap()
        .target
        .clone();
    let EngineResult::ExtensionInteraction(malformed) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: surface.identity.clone(),
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: surface.save.target.clone(),
                    edits: vec![InteractionEdit {
                        target: prompt_words,
                        value: InteractionValue::Text("not-a-number".to_owned()),
                    }],
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        malformed.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert!(
        malformed
            .surface
            .groups
            .iter()
            .flat_map(|group| &group.fields)
            .any(|field| field.error.is_some())
    );
    assert_eq!(trace_len(first.session.session_id), before_first);

    let EngineResult::ExtensionInteraction(missing) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: surface.identity.clone(),
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: surface.actions[0].target.clone(),
                    edits: Vec::new(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        missing.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(trace_len(first.session.session_id), before_first);

    engine
        .execute(
            EngineCommand::AdoptPlugin {
                session_id: first.session.session_id,
                id: memory.manifest.id.clone(),
                version: memory.manifest.version.to_string(),
                digest: memory.manifest.component_sha256.clone(),
                capabilities: BTreeSet::new(),
                settings: json!({"memoryFrozen": true}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let unauthorized = current_surface(first.session.session_id, first.branch.branch_id);
    assert!(!unauthorized.save.enabled);
    let before_unauthorized = trace_len(first.session.session_id);
    let EngineResult::ExtensionInteraction(rejected) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: unauthorized.identity.clone(),
                expected_revision: unauthorized.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: unauthorized.save.target.clone(),
                    edits: Vec::new(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        rejected.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(trace_len(first.session.session_id), before_unauthorized);

    engine
        .execute(
            EngineCommand::SetExtensionEnabled {
                session_id: first.session.session_id,
                id: DEFAULT_MEMORY_EXTENSION_ID.to_owned(),
                enabled: false,
            },
            |_| {},
        )
        .await
        .unwrap();
    let disabled = current_surface(first.session.session_id, first.branch.branch_id);
    assert!(!disabled.save.enabled);
    let before_disabled = trace_len(first.session.session_id);
    let EngineResult::ExtensionInteraction(rejected) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: disabled.identity.clone(),
                expected_revision: disabled.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: disabled.save.target.clone(),
                    edits: Vec::new(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("interaction result");
    };
    assert!(matches!(
        rejected.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(trace_len(first.session.session_id), before_disabled);
}
#[cfg(feature = "scripting")]
#[tokio::test]
async fn summarize_interaction_action_uses_saved_settings_and_appends_checkpoints() {
    // Regression test for ticket 02: native edits must affect the real action without losing checkpoints.
    use parking_lot::Mutex;
    use stcli_core::{
        Config, DEFAULT_MEMORY_EXTENSION_ID, EgressBroker, InferenceBroker, InferenceTransport,
        InferenceTransportError, InteractionEdit, InteractionOutcome, InteractionSubmission,
        InteractionValue, ProviderResult, ProviderSettings, VariableScope,
    };
    use stcli_testkit::MockProvider;
    use std::{collections::BTreeMap, sync::Arc};

    struct CaptureInference {
        requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    }
    impl InferenceTransport for CaptureInference {
        fn generate(
            &self,
            settings: &ProviderSettings,
            request: &serde_json::Value,
        ) -> Result<ProviderResult, InferenceTransportError> {
            self.requests
                .lock()
                .push((settings.id.clone(), request.clone()));
            Ok(ProviderResult {
                text: format!("Summary {}", self.requests.lock().len()),
                request_hash: stcli_core::provider_request_hash(request)
                    .map_err(|error| InferenceTransportError(error.to_string()))?,
                receipt: json!({"stub": true}),
                events: Vec::new(),
            })
        }
    }

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let primary = MockProvider::spawn(["Reply one", "Reply two", "Reply three"])
        .await
        .unwrap();
    let bootstrap = StcliEngine::new(&database);
    let EngineInspection::Plugins(plugins) = bootstrap
        .inspect(EngineQuery::Plugins {
            plugin_id: Some(DEFAULT_MEMORY_EXTENSION_ID.to_owned()),
        })
        .unwrap()
    else {
        panic!("plugins")
    };
    let memory = plugins.into_iter().next().unwrap();
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    drop(store);
    let mut configuration = configuration(character.revision_hash);
    configuration.provider = primary.provider_settings();
    configuration.generation_settings = json!({"max_context": 4096, "max_tokens": 64});
    let EngineResult::CreatedSession(created) = bootstrap
        .execute(
            EngineCommand::CreateSession {
                configuration: Box::new(configuration),
                greeting_index: 0,
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("session")
    };
    bootstrap
        .execute(
            EngineCommand::AdoptExtension {
                session_id: created.session.session_id,
                id: memory.manifest.id,
                version: memory.manifest.version.to_string(),
                digest: memory.manifest.component_sha256,
                settings: json!({"memoryFrozen": true}),
                egress: Vec::new(),
            },
            |_| {},
        )
        .await
        .unwrap();

    let mut summary = primary.provider_settings();
    summary.id = "summary-profile".to_owned();
    Config::add_provider_profile(directory.path(), "primary", primary.provider_settings()).unwrap();
    Config::add_provider_profile(directory.path(), "summary", summary.clone()).unwrap();
    let captured = Arc::new(Mutex::new(Vec::new()));
    let broker = InferenceBroker::stub(
        Config {
            providers: BTreeMap::from([
                (
                    primary.provider_settings().id.clone(),
                    primary.provider_settings(),
                ),
                ("summary".to_owned(), summary),
            ]),
            enabled_extensions: BTreeMap::new(),
        },
        Arc::new(CaptureInference {
            requests: captured.clone(),
        }),
    );
    let engine = StcliEngine::with_effect_brokers(&database, EgressBroker::live(), broker)
        .with_config_directory(directory.path());
    let EngineResult::CompletedTurn(first) = engine
        .execute(
            EngineCommand::Send {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "First fact".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("turn")
    };
    engine
        .execute(
            EngineCommand::Send {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Second fact".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let surface = match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!("surface"),
    };
    assert!(surface.actions[0].enabled);
    let EngineResult::ExtensionInteraction(first_action) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: surface.identity.clone(),
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Invoke {
                    target: surface.actions[0].target.clone(),
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("action")
    };
    let InteractionOutcome::Invoked { result: command } = &first_action.outcome else {
        panic!("invoked")
    };
    assert_eq!(command.receipt.inference.len(), 1);
    assert_eq!(command.receipt.inference[0].text, "Summary 1");
    let settings = Store::open(&database)
        .unwrap()
        .state_transaction(created.session.session_id)
        .unwrap()
        .get(VariableScope::Local, "extension.memory.settings")
        .unwrap()
        .value
        .clone();
    assert_eq!(settings["checkpoints"].as_array().unwrap().len(), 1);
    assert!(settings.get("unknown").is_none());

    let surface = first_action.surface;
    let field = |label: &str| {
        surface
            .groups
            .iter()
            .flat_map(|group| &group.fields)
            .find(|field| field.label == label)
            .unwrap()
            .target
            .clone()
    };
    let before_save_attempt = match engine
        .inspect(EngineQuery::Attempt {
            attempt_id: first.attempt.attempt_id,
        })
        .unwrap()
    {
        EngineInspection::Attempt(attempt) => attempt,
        _ => panic!(),
    };
    let EngineResult::ExtensionInteraction(saved) = engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: surface.identity.clone(),
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Save {
                    target: surface.save.target.clone(),
                    edits: vec![
                        InteractionEdit {
                            target: field("Provider profile"),
                            value: InteractionValue::Text("summary".to_owned()),
                        },
                        InteractionEdit {
                            target: field("Summary word target"),
                            value: InteractionValue::Number(123.into()),
                        },
                        InteractionEdit {
                            target: field("Summary template"),
                            value: InteractionValue::Text("Memory: {{summary}}".to_owned()),
                        },
                    ],
                },
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("save")
    };
    let config_after_save = match &saved.outcome {
        InteractionOutcome::Saved {
            configuration_revision,
        } => configuration_revision.clone(),
        _ => panic!("saved"),
    };
    let unchanged_attempt = match engine
        .inspect(EngineQuery::Attempt {
            attempt_id: first.attempt.attempt_id,
        })
        .unwrap()
    {
        EngineInspection::Attempt(attempt) => attempt,
        _ => panic!(),
    };
    assert_eq!(
        unchanged_attempt.config_hash,
        before_save_attempt.config_hash
    );
    engine
        .execute(
            EngineCommand::Send {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Third fact".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap();
    let surface = match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!(),
    };
    engine
        .execute(
            EngineCommand::SubmitExtensionInteraction {
                identity: surface.identity.clone(),
                expected_revision: surface.revision.clone(),
                submission: InteractionSubmission::Invoke {
                    target: surface.actions[0].target.clone(),
                },
            },
            |_| {},
        )
        .await
        .unwrap();
    {
        let requests = captured.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[1].0, "summary-profile");
        assert!(
            requests[1].1["messages"][0]["content"]
                .as_str()
                .unwrap()
                .contains("123")
        );
    }
    let settings = Store::open(&database)
        .unwrap()
        .state_transaction(created.session.session_id)
        .unwrap()
        .get(VariableScope::Local, "extension.memory.settings")
        .unwrap()
        .value
        .clone();
    assert!(settings.get("unknown").is_none());
    assert_eq!(settings["checkpoints"].as_array().unwrap().len(), 2);
    let current = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.revision_hash,
        _ => panic!(),
    };
    assert_eq!(current, config_after_save);
    let EngineResult::DryRun(dry) = engine
        .execute(
            EngineCommand::DryRunSend {
                session_id: created.session.session_id,
                branch_id: created.branch.branch_id,
                content: "Preview".to_owned(),
            },
            |_| {},
        )
        .await
        .unwrap()
    else {
        panic!("dry run")
    };
    assert!(
        serde_json::to_string(&dry.provider_request["messages"])
            .unwrap()
            .contains("Memory: Summary 2")
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

#[tokio::test]
async fn ordered_list_and_resource_selector_save_atomically_and_reject_stale_reorder() {
    // Regression test for ticket 04: ordered edits and permitted resource references are atomic.
    use stcli_core::{
        InteractionControl, InteractionEdit, InteractionListItem, InteractionOutcome,
        InteractionSubmission, InteractionValue,
    };

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let package = write_stepped_thinking_interaction_fixture(directory.path());
    let EngineResult::InstalledPlugin(installed) = engine
        .execute(EngineCommand::InstallPlugin { directory: package }, |_| {})
        .await
        .unwrap()
    else {
        panic!("installed fixture")
    };
    let mut store = Store::open(&database).unwrap();
    let alice = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let bob_source = fixtures::minimal_card().replace("Alice", "Bob");
    let bob = store.import_artifact(bob_source.as_bytes()).unwrap();
    let mut config = configuration(alice.revision_hash.clone());
    config.plugins.push(stcli_core::PluginPin {
        id: installed.manifest.id,
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256,
        capabilities: [stcli_core::PluginCapability::WriteOwnState]
            .into_iter()
            .collect(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    });
    let created = store.create_session(config, 0).unwrap();
    drop(store);

    let surface = match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!("interaction surface"),
    };
    let prompts = surface.groups[0]
        .fields
        .iter()
        .find(|field| field.control == InteractionControl::OrderedList)
        .unwrap();
    let character = surface.groups[0]
        .fields
        .iter()
        .find(|field| field.control == InteractionControl::ResourceSelector)
        .unwrap();
    assert!(character.choices.iter().any(|choice| choice.value
        == InteractionValue::Text(alice.revision_hash.to_string())
        && choice.label == "Alice"
        && choice.available));
    assert!(character.choices.iter().any(|choice| choice.value
        == InteractionValue::Text(bob.revision_hash.to_string())
        && choice.label == "Bob"
        && choice.available));

    let InteractionValue::OrderedList(mut reordered) = prompts.value.clone().unwrap() else {
        panic!("ordered list")
    };
    reordered.swap(0, 1);
    reordered.push(InteractionListItem {
        id: "polish".to_owned(),
        values: [
            (
                "prompt".to_owned(),
                InteractionValue::Text("Polish the answer".to_owned()),
            ),
            ("enabled".to_owned(), InteractionValue::Boolean(true)),
        ]
        .into_iter()
        .collect(),
    });
    let command = EngineCommand::SubmitExtensionInteraction {
        identity: surface.identity.clone(),
        expected_revision: surface.revision.clone(),
        submission: InteractionSubmission::Save {
            target: surface.save.target.clone(),
            edits: vec![
                InteractionEdit {
                    target: prompts.target.clone(),
                    value: InteractionValue::OrderedList(reordered.clone()),
                },
                InteractionEdit {
                    target: character.target.clone(),
                    value: InteractionValue::Text(bob.revision_hash.to_string()),
                },
            ],
        },
    };
    let EngineResult::ExtensionInteraction(saved) =
        engine.execute(command.clone(), |_| {}).await.unwrap()
    else {
        panic!("save")
    };
    assert!(matches!(saved.outcome, InteractionOutcome::Saved { .. }));
    let stored = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.configuration.plugins[0].settings.clone(),
        _ => panic!("configuration"),
    };
    assert_eq!(stored["prompts"][0]["id"], "check");
    assert_eq!(stored["prompts"][2]["prompt"], "Polish the answer");
    assert_eq!(stored["character"], bob.revision_hash.to_string());

    let EngineResult::ExtensionInteraction(stale) = engine.execute(command, |_| {}).await.unwrap()
    else {
        panic!("stale result")
    };
    assert!(matches!(stale.outcome, InteractionOutcome::Rejected { .. }));
    let unchanged = match engine
        .inspect(EngineQuery::Configuration {
            session_id: created.session.session_id,
        })
        .unwrap()
    {
        EngineInspection::Configuration(record) => record.configuration.plugins[0].settings.clone(),
        _ => panic!("configuration"),
    };
    assert_eq!(unchanged, stored);
}

#[cfg(feature = "scripting")]
#[tokio::test]
async fn roadway_choices_bind_to_selected_candidate_and_stale_content_rejects_before_effects() {
    // Regression test for ticket 05: choice actions stay bound to their source Candidate.
    use stcli_core::{InteractionOutcome, InteractionSubmission};

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let engine = StcliEngine::new(&database);
    let package = write_roadway_interaction_fixture(directory.path());
    let EngineResult::InstalledPlugin(installed) = engine
        .execute(EngineCommand::InstallPlugin { directory: package }, |_| {})
        .await
        .unwrap()
    else {
        panic!("installed fixture")
    };
    let mut store = Store::open(&database).unwrap();
    let character = store
        .import_artifact(fixtures::minimal_card().as_bytes())
        .unwrap();
    let mut config = configuration(character.revision_hash);
    config.plugins.push(stcli_core::PluginPin {
        id: installed.manifest.id,
        version: installed.manifest.version.to_string(),
        component_hash: installed.manifest.component_sha256,
        capabilities: [stcli_core::PluginCapability::RegisterCommand]
            .into_iter()
            .collect(),
        settings: json!({}),
        egress_allow_list: Vec::new(),
        enabled: true,
    });
    let created = store.create_session(config, 0).unwrap();
    let turn = create_failed_turn(
        &mut store,
        created.session.session_id,
        created.branch.branch_id,
        "Look around",
    )
    .await;
    let first_candidate = complete_with_candidate(&mut store, &turn, "The archive is quiet.");
    drop(store);

    let surface = match engine
        .inspect(EngineQuery::ExtensionInteractions {
            session_id: created.session.session_id,
            branch_id: Some(created.branch.branch_id),
        })
        .unwrap()
    {
        EngineInspection::ExtensionInteractions(mut surfaces) => surfaces.pop().unwrap(),
        _ => panic!("surface"),
    };
    let action = &surface.actions[0];
    assert_eq!(
        action.content.as_ref().unwrap().candidate_id,
        first_candidate
    );
    assert_eq!(action.content.as_ref().unwrap().choices.len(), 0);

    let command = EngineCommand::SubmitExtensionInteraction {
        identity: surface.identity.clone(),
        expected_revision: surface.revision.clone(),
        submission: InteractionSubmission::Invoke {
            target: action.target.clone(),
        },
    };
    let EngineResult::ExtensionInteraction(generated) =
        engine.execute(command.clone(), |_| {}).await.unwrap()
    else {
        panic!("generated")
    };
    assert!(matches!(
        generated.outcome,
        InteractionOutcome::Invoked { .. }
    ));
    assert_eq!(
        generated.surface.actions[0]
            .content
            .as_ref()
            .unwrap()
            .choices
            .len(),
        2
    );

    let mut store = Store::open(&database).unwrap();
    let second_candidate = complete_with_candidate(&mut store, &turn, "A door opens.");
    store.select_swipe(turn.turn_id, second_candidate).unwrap();
    let trace_before = store
        .trace_events(Some(created.session.session_id))
        .unwrap()
        .len();
    drop(store);
    let EngineResult::ExtensionInteraction(rejected) =
        engine.execute(command, |_| {}).await.unwrap()
    else {
        panic!("rejected")
    };
    assert!(matches!(
        rejected.outcome,
        InteractionOutcome::Rejected { .. }
    ));
    assert_eq!(
        Store::open(&database)
            .unwrap()
            .trace_events(Some(created.session.session_id))
            .unwrap()
            .len(),
        trace_before
    );
}
