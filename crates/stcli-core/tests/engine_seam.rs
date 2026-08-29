use stcli_core::{EngineCommand, EngineInspection, EngineQuery, EngineResult, StcliEngine, Store};
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

    let EngineResult::Artifact(character) = engine
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
