//! L2 provider failure mode coverage.

use serde_json::{Value, json};
use stcli_core::{
    AttemptStatus, EntityId, ProviderError, ProviderSettings, SessionConfiguration, Store,
    TurnError,
};
use stcli_testkit::MockProvider;
use std::collections::BTreeMap;
use tempfile::{TempDir, tempdir};

const MINIMAL_CARD: &str = r#"{"spec":"chara_card_v2","spec_version":"2.0","data":{"name":"Alice","description":"","personality":"","scenario":"","first_mes":"Welcome.","mes_example":"","alternate_greetings":[],"extensions":{}}}"#;

struct FailureStore {
    _directory: TempDir,
    store: Store,
    session_id: EntityId,
    branch_id: EntityId,
}

async fn with_failed_provider(
    generation_settings: Value,
    timeout: u64,
) -> (FailureStore, MockProvider) {
    let provider = MockProvider::spawn(Vec::<String>::new()).await.unwrap();
    let settings = provider.provider_settings();

    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let card = store.import_artifact(MINIMAL_CARD.as_bytes()).unwrap();
    let created = store
        .create_session(
            SessionConfiguration {
                compatibility_profile: "sillytavern-1.18-core".to_owned(),
                character_revision: card.revision_hash,
                persona_name: "User".to_owned(),
                persona_description: None,
                lorebook_revisions: vec![],
                prompt_preset_revision: None,
                provider: ProviderSettings {
                    timeout_seconds: timeout,
                    ..settings
                },
                tokenizer: "tiktoken:o200k_base".to_owned(),
                generation_settings,
                plugins: vec![],
                script_grants: vec![],
            },
            0,
        )
        .unwrap();

    (
        FailureStore {
            _directory: directory,
            store,
            session_id: created.session.session_id,
            branch_id: created.branch.branch_id,
        },
        provider,
    )
}

fn assert_attempt_failed_no_candidate(store: &mut Store, branch_id: EntityId) {
    let turn = store.turns_for_branch(branch_id).unwrap().remove(0);
    let attempts = store.attempts_for_turn(turn.turn_id).unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, AttemptStatus::Failed);
    assert!(store.candidates_for_turn(turn.turn_id).unwrap().is_empty());
    assert!(
        store
            .trace_events(Some(turn.session_id))
            .unwrap()
            .iter()
            .any(|event| event.event_type == "attempt.failed")
    );
}

#[tokio::test]
async fn provider_429_records_failed_attempt() {
    let (mut f, _provider) = with_failed_provider(json!({"fixture_status": 429}), 5).await;
    let error = f
        .store
        .send_message(f.session_id, f.branch_id, "Hello".to_owned(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::Http { status: 429, .. })
    ));
    assert_attempt_failed_no_candidate(&mut f.store, f.branch_id);
}

#[tokio::test]
async fn provider_500_records_failed_attempt() {
    let (mut f, _provider) = with_failed_provider(json!({"fixture_status": 500}), 5).await;
    let error = f
        .store
        .send_message(f.session_id, f.branch_id, "Hello".to_owned(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::Http { status: 500, .. })
    ));
    assert_attempt_failed_no_candidate(&mut f.store, f.branch_id);
}

#[tokio::test]
async fn provider_503_records_failed_attempt() {
    let (mut f, _provider) = with_failed_provider(json!({"fixture_status": 503}), 5).await;
    let error = f
        .store
        .send_message(f.session_id, f.branch_id, "Hello".to_owned(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::Http { status: 503, .. })
    ));
    assert_attempt_failed_no_candidate(&mut f.store, f.branch_id);
}

#[tokio::test]
async fn provider_non_json_records_failed_attempt() {
    let (mut f, _provider) = with_failed_provider(json!({"fixture_non_json": true}), 5).await;
    let error = f
        .store
        .send_message(f.session_id, f.branch_id, "Hello".to_owned(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::ChunkDecode { .. })
    ));
    assert_attempt_failed_no_candidate(&mut f.store, f.branch_id);
}

#[tokio::test]
async fn provider_timeout_records_failed_attempt() {
    let (mut f, _provider) = with_failed_provider(json!({"fixture_delay_ms": 10_000}), 1).await;
    let error = f
        .store
        .send_message(f.session_id, f.branch_id, "Hello".to_owned(), |_| {})
        .await
        .unwrap_err();
    assert!(matches!(
        error,
        TurnError::Provider(ProviderError::Transport { .. })
    ));
    assert_attempt_failed_no_candidate(&mut f.store, f.branch_id);
}

#[tokio::test]
async fn provider_connection_refused_records_failed_attempt() {
    let directory = tempdir().unwrap();
    let database = directory.path().join("stcli.sqlite3");
    let mut store = Store::open(&database).unwrap();
    let card = store.import_artifact(MINIMAL_CARD.as_bytes()).unwrap();
    let created = store
        .create_session(
            SessionConfiguration {
                compatibility_profile: "sillytavern-1.18-core".to_owned(),
                character_revision: card.revision_hash,
                persona_name: "User".to_owned(),
                persona_description: None,
                lorebook_revisions: vec![],
                prompt_preset_revision: None,
                provider: ProviderSettings {
                    id: "conn-refused".to_owned(),
                    base_url: "https://127.0.0.1:1".to_owned(),
                    chat_completions_path: "/v1/chat/completions".to_owned(),
                    api_key_env: None,
                    static_headers: BTreeMap::new(),
                    timeout_seconds: 1,
                    ca_certificate_pem: None,
                    model: "fixture-model".to_owned(),
                    stream: false,
                    format_mode: Default::default(),
                    completions_path: None,
                    instruct_template: None,
                    context_formatting: None,
                },
                tokenizer: "tiktoken:o200k_base".to_owned(),
                generation_settings: json!({}),
                plugins: vec![],
                script_grants: vec![],
            },
            0,
        )
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
        TurnError::Provider(ProviderError::Transport { .. })
    ));
    assert_attempt_failed_no_candidate(&mut store, created.branch.branch_id);
}
